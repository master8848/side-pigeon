//! Alias support for multi-bot per provider (F1).
//!
//! One driver kind (e.g. `telegram`) can be instantiated multiple times under
//! different aliases (e.g. `tg-main`, `tg-ops`). The provider trait's
//! `id()` is driver-hardcoded, so we wrap both the event sink and the
//! provider to expose the alias on every surface: registry key, ids(),
//! ChannelMessage.channel, and on_error provider.

use std::sync::Arc;

use async_trait::async_trait;

use crate::schema::{ChannelMessage, SendMessage, SendReceipt};
use crate::traits::{ChatProvider, ProviderEvents};
use crate::ProviderError;

/// Resolve the driver kind for a provider entry.
/// `alias` is the registry key (`ProviderConfig.id`); `config` is the
/// provider's JSON config. `config["kind"]` overrides the alias when
/// present so `{"id":"tg-main","config":{"kind":"telegram",...}}` loads the
/// `telegram` driver under alias `tg-main`.
pub fn provider_kind<'a>(alias: &'a str, config: &'a serde_json::Value) -> &'a str {
    config.get("kind").and_then(|v| v.as_str()).unwrap_or(alias)
}

/// Leak an alias string to obtain a `'static` reference required by
/// `ChatProvider::id()`.
pub fn leak_alias(alias: &str) -> &'static str {
    Box::leak(alias.to_owned().into_boxed_str())
}

/// Event sink that rewrites `ChannelMessage.channel` and `on_error`
/// provider to the alias before forwarding.
pub struct AliasEvents {
    alias: &'static str,
    inner: Arc<dyn ProviderEvents>,
}

impl AliasEvents {
    /// Create a new wrapper.
    pub fn new(alias: &'static str, inner: Arc<dyn ProviderEvents>) -> Self {
        Self { alias, inner }
    }

    /// Convenience: wrap into `Arc<dyn ProviderEvents>`.
    pub fn wrap(alias: &'static str, inner: Arc<dyn ProviderEvents>) -> Arc<dyn ProviderEvents> {
        Arc::new(Self::new(alias, inner)) as Arc<dyn ProviderEvents>
    }
}

impl ProviderEvents for AliasEvents {
    fn on_message(&self, mut msg: ChannelMessage) {
        msg.channel = self.alias.to_string();
        self.inner.on_message(msg);
    }

    fn on_error(&self, _provider: &str, error: &ProviderError) {
        self.inner.on_error(self.alias, error);
    }
}

/// `ChatProvider` wrapper that exposes the alias as `id()` and delegates
/// lifecycle to the inner driver.
pub struct AliasedProvider {
    alias: &'static str,
    inner: Box<dyn ChatProvider>,
}

impl AliasedProvider {
    /// Create a new aliased provider.
    pub fn new(alias: &'static str, inner: Box<dyn ChatProvider>) -> Self {
        Self { alias, inner }
    }
}

#[async_trait]
impl ChatProvider for AliasedProvider {
    fn id(&self) -> &'static str {
        self.alias
    }

    async fn start(&mut self) -> Result<(), ProviderError> {
        self.inner.start().await
    }

    async fn stop(&mut self) -> Result<(), ProviderError> {
        self.inner.stop().await
    }

    async fn send(&self, msg: &SendMessage) -> Result<SendReceipt, ProviderError> {
        self.inner.send(msg).await
    }
}
