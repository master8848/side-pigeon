//! Optional WebSocket JSON-RPC server (feature `ws`, tokio-tungstenite).
//!
//! One JSON document per text message. Notifications from providers fan out
//! to every connected client via the broadcast channel. A `shutdown` request
//! closes the connection that issued it (the server keeps accepting others).
//!
//! # Security note — TLS
//! Plaintext WebSockets here are **localhost-only** by design. Production
//! deployments MUST place a reverse-proxy (nginx, Caddy, Fly, etc.) in front
//! that terminates TLS (wss://) and enforces authentication. Do not expose
//! the raw listener directly to the public internet.

use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, Mutex, Semaphore};

use crate::error::TransportError;
use crate::jsonrpc::parse_request;
use crate::state::{dropped_frames_notification, AppState, DispatchOutcome, Outbound};

const MAX_BODY_BYTES: usize = 1 << 20; // 1 MiB — matches http.rs

fn extract_origin_host(origin: &str) -> Option<String> {
    let scheme_end = origin.find("://")?;
    let after = &origin[scheme_end + 3..];
    let end = after.find(['/', '?', '#']).unwrap_or(after.len());
    let authority = &after[..end];
    if authority.is_empty() {
        return None;
    }
    if authority.starts_with('[') {
        let closing = authority.find(']')?;
        Some(authority[1..closing].to_ascii_lowercase())
    } else if authority == "::1" || authority.starts_with("::1:") {
        Some("::1".to_string())
    } else {
        let host_part = authority.rsplit('@').next().unwrap_or(authority);
        let host = host_part.split(':').next().unwrap_or(host_part);
        if host.is_empty() {
            return None;
        }
        Some(host.to_ascii_lowercase())
    }
}

fn is_allowed_ws_origin(origin: &str) -> bool {
    match extract_origin_host(origin) {
        Some(host) => host == "127.0.0.1" || host == "localhost" || host == "::1",
        None => false,
    }
}

#[allow(clippy::result_large_err)]
fn ws_origin_callback(
    req: &tokio_tungstenite::tungstenite::handshake::server::Request,
    resp: tokio_tungstenite::tungstenite::handshake::server::Response,
) -> Result<
    tokio_tungstenite::tungstenite::handshake::server::Response,
    tokio_tungstenite::tungstenite::handshake::server::ErrorResponse,
> {
    if let Some(origin) = req.headers().get("origin").and_then(|v| v.to_str().ok()) {
        if !is_allowed_ws_origin(origin) {
            // 403 rejection: handshake callback error produces an HTTP error response
            let err = tokio_tungstenite::tungstenite::http::Response::builder()
                .status(tokio_tungstenite::tungstenite::http::StatusCode::FORBIDDEN)
                .body(Some("forbidden origin".to_string()))
                .expect("valid 403 response");
            return Err(err);
        }
    }
    Ok(resp)
}

/// Accept WebSocket connections on `listener` and serve JSON-RPC over them.
pub async fn serve_ws(
    state: Arc<Mutex<AppState>>,
    notify_tx: broadcast::Sender<Outbound>,
    listener: TcpListener,
) -> Result<(), TransportError> {
    const MAX_CONNECTIONS: usize = 256;
    let semaphore = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    // TODO(idle-timeout): enforce idle timeout on WebSocket connections (e.g.
    // `tokio::time::timeout` around read loops / ping interval) to reclaim
    // idle or half-open sockets. Currently relies on TCP keepalive + peer close.
    loop {
        let (stream, peer) = listener.accept().await?;
        let permit = match semaphore.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                tracing::warn!(peer = %peer, "ws connection limit reached (256), dropping");
                continue;
            }
        };
        let state = state.clone();
        let notify_tx = notify_tx.clone();
        tokio::spawn(async move {
            let _permit = permit; // held for connection lifetime
            if let Err(e) = handle_connection(state, notify_tx, stream).await {
                tracing::warn!(peer = %peer, error = %e, "ws connection error");
            }
        });
    }
}

/// Push an outbound frame onto the connection queue. Returns `Ok(true)` on
/// success and `Ok(false)` (closing the connection) when the client is too
/// slow and the queue is full — bounded memory, honest backpressure.
async fn queue_or_close(
    out_tx: &mpsc::Sender<Outbound>,
    frame: Outbound,
) -> Result<bool, TransportError> {
    match out_tx.try_send(frame) {
        Ok(()) => Ok(true),
        Err(mpsc::error::TrySendError::Full(_)) => {
            tracing::warn!("ws outbound queue full; closing slow client");
            let _ = out_tx
                .send(Outbound::Notification(dropped_frames_notification(0)))
                .await;
            Ok(false)
        }
        Err(mpsc::error::TrySendError::Closed(_)) => Ok(false),
    }
}

async fn handle_connection(
    state: Arc<Mutex<AppState>>,
    notify_tx: broadcast::Sender<Outbound>,
    stream: TcpStream,
) -> Result<(), TransportError> {
    let ws_config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig {
        max_message_size: Some(MAX_BODY_BYTES),
        max_frame_size: Some(MAX_BODY_BYTES),
        ..Default::default()
    };
    let websocket = tokio_tungstenite::accept_hdr_async_with_config(
        stream,
        ws_origin_callback,
        Some(ws_config),
    )
    .await?;
    let (mut sink, mut source) = websocket.split();

    // Per-connection outbound queue: responses (this handler) + notifications
    // (forwarded from the broadcast) are merged here so only the writer task
    // touches the socket. Bounded so a slow client cannot grow memory without
    // bound; on overflow the connection is closed with an honest event.error.
    const OUT_QUEUE_CAPACITY: usize = 1024;
    let (out_tx, mut out_rx) = mpsc::channel::<Outbound>(OUT_QUEUE_CAPACITY);

    let mut bcast_rx = notify_tx.subscribe();
    let forward_tx = out_tx.clone();
    let forwarder = tokio::spawn(async move {
        loop {
            match bcast_rx.recv().await {
                Ok(Outbound::Notification(n)) => {
                    if forward_tx.send(Outbound::Notification(n)).await.is_err() {
                        break; // client connection went away
                    }
                }
                Ok(Outbound::Response(_)) => {}
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "ws notification forwarder lagged");
                    // Honest signal: tell this client it missed frames.
                    if forward_tx
                        .send(Outbound::Notification(dropped_frames_notification(skipped)))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    let writer = tokio::spawn(async move {
        while let Some(outbound) = out_rx.recv().await {
            let text = match outbound {
                Outbound::Response(r) => serde_json::to_string(&r)?,
                Outbound::Notification(n) => serde_json::to_string(&n)?,
            };
            sink.send(tokio_tungstenite::tungstenite::Message::Text(text))
                .await?;
        }
        Ok::<(), TransportError>(())
    });

    while let Some(message) = source.next().await {
        let message = message?;
        match message {
            tokio_tungstenite::tungstenite::Message::Text(text) => {
                match parse_request(text.as_str()) {
                    Ok(request) => {
                        let outcome = state.lock().await.handle_request(request).await;
                        let (response, shutdown) = match outcome {
                            DispatchOutcome::Response(r) => (Some(r), false),
                            DispatchOutcome::Shutdown(r) => (Some(r), true),
                            DispatchOutcome::Ignore => (None, false),
                        };
                        if let Some(response) = response {
                            if !queue_or_close(&out_tx, Outbound::Response(response)).await? {
                                break;
                            }
                        }
                        if shutdown {
                            break;
                        }
                    }
                    Err(response) => {
                        if !queue_or_close(&out_tx, Outbound::Response(*response)).await? {
                            break;
                        }
                    }
                }
            }
            tokio_tungstenite::tungstenite::Message::Close(_) => break,
            _ => {}
        }
    }

    forwarder.abort();
    let _ = forwarder.await; // drops the forwarder's sender clone
    drop(out_tx);
    let _ = writer.await;
    Ok(())
}
