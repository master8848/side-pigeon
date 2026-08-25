//! Optional WebSocket JSON-RPC server (feature `ws`, tokio-tungstenite).
//!
//! One JSON document per text message. Notifications from providers fan out
//! to every connected client via the broadcast channel. A `shutdown` request
//! closes the connection that issued it (the server keeps accepting others).

use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, Mutex};

use crate::error::TransportError;
use crate::jsonrpc::parse_request;
use crate::state::{dropped_frames_notification, AppState, DispatchOutcome, Outbound};

/// Accept WebSocket connections on `listener` and serve JSON-RPC over them.
pub async fn serve_ws(
    state: Arc<Mutex<AppState>>,
    notify_tx: broadcast::Sender<Outbound>,
    listener: TcpListener,
) -> Result<(), TransportError> {
    loop {
        let (stream, peer) = listener.accept().await?;
        let state = state.clone();
        let notify_tx = notify_tx.clone();
        tokio::spawn(async move {
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
    let websocket = tokio_tungstenite::accept_async(stream).await?;
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
