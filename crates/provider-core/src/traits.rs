//! Capability traits (see `docs/api-contract.md`).

use async_trait::async_trait;

use crate::schema::{ChannelMessage, SendMessage, SendReceipt};
use crate::ProviderError;

/// How many consecutive transient failures before a provider emits
/// `on_error` for a still-running (non-fatal) connection.
pub const TRANSIENT_ERROR_EVENT_THRESHOLD: u32 = 10;

/// A chat provider: one messaging platform connection.
///
/// Lifecycle: construct → [`start`](ChatProvider::start) (called by `listen`)
/// → [`send`](ChatProvider::send) → [`stop`](ChatProvider::stop).
///
/// Implementations are written with `#[async_trait]` so they can be registered
/// dynamically in a [`ProviderRegistry`](crate::registry::ProviderRegistry).
#[async_trait]
pub trait ChatProvider: Send + Sync {
    /// Stable provider id, e.g. `"telegram"`.
    fn id(&self) -> &'static str;

    /// Start listening for inbound messages. Must be called before `send`.
    async fn start(&mut self) -> Result<(), ProviderError>;

    /// Stop listening and release platform resources.
    async fn stop(&mut self) -> Result<(), ProviderError>;

    /// Send one message and return the platform receipt.
    async fn send(&self, msg: &SendMessage) -> Result<SendReceipt, ProviderError>;
}

/// Inbound event sink. Implemented by the transport; providers call
/// `on_message` for every normalized inbound message.
pub trait ProviderEvents: Send + Sync {
    /// A new inbound message arrived.
    fn on_message(&self, msg: ChannelMessage);

    /// An asynchronous provider error occurred.
    ///
    /// Called with a **fatal** error right before the provider stops (e.g.
    /// Telegram HTTP 401/409, Discord gateway close 4004/4010-4014) and with
    /// a persistent transient error once a connection has failed
    /// [`TRANSIENT_ERROR_EVENT_THRESHOLD`] times in a row (so hosts see
    /// sustained degradation instead of silence). The default is a no-op;
    /// the transport sink implements it by emitting `event.error`.
    fn on_error(&self, provider: &str, error: &ProviderError) {
        let _ = (provider, error);
    }
}
