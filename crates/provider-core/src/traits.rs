//! Capability traits (see `docs/api-contract.md`).

use async_trait::async_trait;

use crate::schema::{ChannelMessage, SendMessage, SendReceipt};
use crate::ProviderError;

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
}
