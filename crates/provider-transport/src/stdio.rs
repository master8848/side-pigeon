//! Newline-delimited JSON-RPC 2.0 over stdio (primary transport).

use tokio::io::{
    AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, BufWriter,
};
use tokio::sync::broadcast::{self, error::RecvError};

use crate::error::TransportError;
use crate::jsonrpc::parse_request;
use crate::state::{dropped_frames_notification, AppState, DispatchOutcome, Outbound};

const MAX_LINE_BYTES: usize = 1 << 20; // 1 MiB — same as HTTP/ws limit

/// Serve JSON-RPC 2.0 over `stdin`/`stdout`, one JSON document per line.
///
/// Runs until the client sends `shutdown` or closes stdin (EOF). Responses
/// and `event.*` notifications are written to `stdout` in the order they were
/// produced. On exit all started providers are stopped.
///
/// `notify_tx` is the sender returned by [`AppState::new`]; it is dropped on
/// exit so the writer task observes channel close and flushes cleanly.
pub async fn serve_stdio(
    mut state: AppState,
    notify_tx: broadcast::Sender<Outbound>,
    stdin: impl AsyncRead + Unpin + Send + 'static,
    stdout: impl AsyncWrite + Unpin + Send + 'static,
) -> Result<(), TransportError> {
    let writer_task = tokio::spawn(write_loop(notify_tx.subscribe(), BufWriter::new(stdout)));
    let mut reader = BufReader::new(stdin);
    let mut result: Result<(), TransportError> = Ok(());
    let mut buf: Vec<u8> = Vec::with_capacity(8192);

    'read: loop {
        buf.clear();
        // Bounded read: cap at MAX_LINE_BYTES+1 to detect oversize without unbounded alloc
        let n = match (&mut reader)
            .take((MAX_LINE_BYTES + 1) as u64)
            .read_until(b'\n', &mut buf)
            .await
        {
            Ok(n) => n,
            Err(e) => {
                result = Err(e.into());
                break;
            }
        };
        if n == 0 {
            break; // EOF
        }
        // Detect oversize: payload (without trailing newline) > MAX_LINE_BYTES.
        // Take limit is MAX+1 so a valid MAX payload plus '\n' fits exactly.
        let payload_len = if buf.ends_with(b"\n") {
            // handle \r\n
            if buf.len() >= 2 && buf[buf.len() - 2] == b'\r' {
                buf.len() - 2
            } else {
                buf.len() - 1
            }
        } else {
            buf.len()
        };
        let truncated = n as usize == MAX_LINE_BYTES + 1 && !buf.ends_with(b"\n");
        let oversized = payload_len > MAX_LINE_BYTES || truncated;
        if oversized {
            // Discard remainder of this overlong line until newline/EOF to resync
            if !buf.ends_with(b"\n") {
                let mut discard = Vec::new();
                loop {
                    discard.clear();
                    // Bounded discard: 8 KiB per iteration to avoid unbounded alloc
                    match (&mut reader)
                        .take(8192)
                        .read_until(b'\n', &mut discard)
                        .await
                    {
                        Ok(0) => break,
                        Ok(_) => {
                            if discard.ends_with(b"\n") {
                                break;
                            }
                            if discard.is_empty() {
                                break;
                            }
                            // No newline yet; continue discarding in bounded chunks
                        }
                        Err(e) => {
                            result = Err(e.into());
                            break 'read;
                        }
                    }
                }
            }
            tracing::warn!(len = buf.len(), "stdio line exceeds 1 MiB, rejecting");
            let err_resp = crate::jsonrpc::Response::err(
                crate::jsonrpc::Id::Null,
                crate::jsonrpc::JsonRpcError::INVALID_REQUEST,
                "line too large (max 1 MiB)",
                None,
            );
            let _ = state.notify().send(Outbound::Response(err_resp));
            continue;
        }
        // Convert to string; buf is known to be <=1MiB+1 (payload <=1MiB)
        let line_str = String::from_utf8_lossy(&buf);
        let line = line_str.trim();
        if line.is_empty() {
            continue;
        }
        match parse_request(line) {
            Ok(request) => {
                let outcome = state.handle_request(request).await;
                let (response, shutdown) = match outcome {
                    DispatchOutcome::Response(r) => (Some(r), false),
                    DispatchOutcome::Shutdown(r) => (Some(r), true),
                    DispatchOutcome::Ignore => (None, false),
                };
                if let Some(response) = response {
                    let _ = state.notify().send(Outbound::Response(response));
                }
                if shutdown {
                    break 'read;
                }
            }
            Err(response) => {
                let _ = state.notify().send(Outbound::Response(*response));
            }
        }
    }

    // Clean shutdown: stop providers, then close the broadcast channel so the
    // writer task drains and exits.
    if let Err(e) = state.registry_mut().stop_all().await {
        tracing::warn!(error = %e, "error stopping providers on shutdown");
    }
    drop(state);
    drop(notify_tx); // close the broadcast channel so the writer drains and exits
    let _ = writer_task.await;
    result
}

/// Writer task: serialize every outbound frame to one line on `stdout`.
async fn write_loop<W: AsyncWrite + Unpin>(
    mut rx: broadcast::Receiver<Outbound>,
    mut writer: BufWriter<W>,
) -> Result<(), TransportError> {
    loop {
        match rx.recv().await {
            Ok(outbound) => {
                let text = match outbound {
                    Outbound::Response(r) => serde_json::to_string(&r)?,
                    Outbound::Notification(n) => serde_json::to_string(&n)?,
                };
                writer.write_all(text.as_bytes()).await?;
                writer.write_all(b"\n").await?;
                writer.flush().await?;
            }
            Err(RecvError::Lagged(skipped)) => {
                // The client did not read fast enough and frames were dropped.
                // Emit an honest event.error so hosts can react (the writer owns
                // stdout, so it writes the synthetic notification directly).
                tracing::warn!(skipped, "stdio writer lagged; dropped frames");
                let text = serde_json::to_string(&dropped_frames_notification(skipped))?;
                writer.write_all(text.as_bytes()).await?;
                writer.write_all(b"\n").await?;
                writer.flush().await?;
            }
            Err(RecvError::Closed) => break,
        }
    }
    Ok(())
}
