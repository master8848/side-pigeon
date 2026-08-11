//! Transport-level errors.

use thiserror::Error;

/// An error in the transport layer (I/O, framing, websocket).
#[derive(Debug, Error)]
pub enum TransportError {
    /// Underlying I/O error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// JSON serialization/deserialization error.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    /// WebSocket error (feature `ws`).
    #[cfg(feature = "ws")]
    #[error("websocket error: {0}")]
    Tungstenite(#[from] tokio_tungstenite::tungstenite::Error),
}
