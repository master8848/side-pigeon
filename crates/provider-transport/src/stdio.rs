//! Newline-delimited JSON-RPC 2.0 over stdio (primary transport).

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, BufWriter};
use tokio::sync::broadcast::{self, error::RecvError};

use crate::error::TransportError;
use crate::jsonrpc::parse_request;
use crate::state::{AppState, DispatchOutcome, Outbound};

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
    let mut reader = BufReader::new(stdin).lines();
    let mut result: Result<(), TransportError> = Ok(());

    'read: loop {
        let line = match reader.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => break, // client closed stdin
            Err(e) => {
                result = Err(e.into());
                break;
            }
        };
        let line = line.trim();
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
                tracing::warn!(skipped, "stdio writer lagged; dropped frames");
            }
            Err(RecvError::Closed) => break,
        }
    }
    Ok(())
}
