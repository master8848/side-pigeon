//! Headless client — [`EventBus`], [`ProviderClient`] and friends.
//!
//! This module ports TankStack's `query-core` headless pattern to the
//! provider world: an [`EventBus`] owns typed, filterable subscriptions and
//! a plugin chain, mirroring before/after middleware. Providers publish via
//! `Arc<dyn ProviderEvents>` which is an [`EventBus`] so all inbound paths
//! go through the same filter + plugin pipeline.

use std::sync::{Arc, Mutex};

use crate::plugin::{ControlFlow, Plugin};
use crate::registry::ProviderRegistry;
use crate::schema::{ChannelMessage, SendMessage, SendReceipt};
use crate::traits::ProviderEvents;
use crate::ProviderError;

// ---------------------------------------------------------------------------
// EventFilter
// ---------------------------------------------------------------------------

/// Filter for [`EventBus::subscribe`].
///
/// Every field is an opt-in AND: `None` means "match any", `Some(v)` means
/// the message must equal `v`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventFilter {
    /// Exact `ChannelMessage.channel` (provider id) to match.
    pub provider: Option<String>,
    /// Exact `ChannelMessage.channel_id` (room) to match.
    pub channel_id: Option<String>,
    /// Exact `ChannelMessage.explicitly_addressed` to match.
    pub explicitly_addressed: Option<bool>,
}

impl EventFilter {
    /// Create an empty filter (matches everything).
    pub fn new() -> Self {
        EventFilter::default()
    }

    /// Convenience: filter by provider id.
    pub fn provider(mut self, id: impl Into<String>) -> Self {
        self.provider = Some(id.into());
        self
    }

    /// Convenience: filter by room.
    pub fn channel_id(mut self, room: impl Into<String>) -> Self {
        self.channel_id = Some(room.into());
        self
    }

    /// Convenience: filter by explicit addressing.
    pub fn explicitly_addressed(mut self, b: bool) -> Self {
        self.explicitly_addressed = Some(b);
        self
    }

    /// Whether `msg` satisfies this filter.
    pub fn matches(&self, msg: &ChannelMessage) -> bool {
        if let Some(p) = &self.provider {
            if msg.channel != *p {
                return false;
            }
        }
        if let Some(c) = &self.channel_id {
            if msg.channel_id != *c {
                return false;
            }
        }
        if let Some(ea) = self.explicitly_addressed {
            if msg.explicitly_addressed != ea {
                return false;
            }
        }
        true
    }
}

// ---------------------------------------------------------------------------
// EventBus
// ---------------------------------------------------------------------------

type Callback = Arc<dyn Fn(&ChannelMessage) + Send + Sync>;

struct Subscriber {
    id: u64,
    filter: EventFilter,
    cb: Callback,
}

struct EventBusInner {
    subscribers: Mutex<Vec<Subscriber>>,
    plugins: Mutex<Vec<Box<dyn Plugin>>>,
    next_id: Mutex<u64>,
}

/// Typed, filterable event bus with a plugin chain.
///
/// Cheap to clone (`Arc` under the hood). Every `publish` runs the plugin
/// `on_message` chain before fanning out to per-subscriber callbacks. Dropped
/// messages never reach subscribers. Each subscriber is independent (no global
/// bounded queue loss — callers process synchronously in `publish`).
#[derive(Clone)]
pub struct EventBus {
    inner: Arc<EventBusInner>,
}

impl Default for EventBus {
    fn default() -> Self {
        EventBus::new()
    }
}

impl std::fmt::Debug for EventBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let subs = self
            .inner
            .subscribers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len();
        let plugins = self
            .inner
            .plugins
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len();
        f.debug_struct("EventBus")
            .field("subscribers", &subs)
            .field("plugins", &plugins)
            .finish()
    }
}

impl EventBus {
    /// Create an empty bus.
    pub fn new() -> Self {
        EventBus {
            inner: Arc::new(EventBusInner {
                subscribers: Mutex::new(Vec::new()),
                plugins: Mutex::new(Vec::new()),
                next_id: Mutex::new(1),
            }),
        }
    }

    /// Push a plugin to the end of the chain.
    pub fn use_plugin<P: Plugin + 'static>(&self, plugin: P) {
        self.inner
            .plugins
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Box::new(plugin));
    }

    /// Number of plugins (for tests / diagnostics).
    pub fn plugin_count(&self) -> usize {
        self.inner
            .plugins
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    /// Number of active subscriptions.
    pub fn subscriber_count(&self) -> usize {
        self.inner
            .subscribers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    /// Publish one message: run `on_message` plugins in order, then fan out
    /// to every matching subscriber callback.
    ///
    /// Returns the final [`ControlFlow`]; `Drop` means no subscriber was
    /// invoked.
    pub fn publish(&self, msg: ChannelMessage) -> ControlFlow {
        self.publish_filtered(msg)
            .map_or(ControlFlow::Drop, |(flow, _)| flow)
    }

    /// Like [`EventBus::publish`] but returns the final (possibly rewritten)
    /// message when it was not dropped. Used by the transport bridge to forward
    /// the mutated payload to the JSON-RPC broadcast channel.
    pub fn publish_filtered(
        &self,
        mut msg: ChannelMessage,
    ) -> Option<(ControlFlow, ChannelMessage)> {
        let mut final_flow = ControlFlow::Continue;
        {
            let plugins = self.inner.plugins.lock().unwrap_or_else(|e| e.into_inner());
            for p in plugins.iter() {
                match p.on_message(&mut msg) {
                    ControlFlow::Continue => {}
                    ControlFlow::Drop => return None,
                    ControlFlow::Rewrite => final_flow = ControlFlow::Rewrite,
                }
            }
        }
        // Collect matching callbacks without holding the subscriber lock
        // while invoking (a callback may subscribe/unsubscribe).
        let callbacks: Vec<Callback> = {
            let subs = self
                .inner
                .subscribers
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            subs.iter()
                .filter(|s| s.filter.matches(&msg))
                .map(|s| s.cb.clone())
                .collect()
        };
        for cb in callbacks {
            cb(&msg);
        }
        Some((final_flow, msg))
    }

    /// Run the outbound plugin chain on `msg`. Returns `Drop` if any plugin
    /// voted to drop (caller should abort the send), otherwise `Continue` /
    /// `Rewrite`.
    pub fn filter_send(&self, msg: &mut SendMessage) -> ControlFlow {
        let mut final_flow = ControlFlow::Continue;
        let plugins = self.inner.plugins.lock().unwrap_or_else(|e| e.into_inner());
        for p in plugins.iter() {
            match p.on_send(msg) {
                ControlFlow::Continue => {}
                ControlFlow::Drop => return ControlFlow::Drop,
                ControlFlow::Rewrite => final_flow = ControlFlow::Rewrite,
            }
        }
        final_flow
    }

    /// Notify plugins of an async error (fan-out, no filtering).
    pub fn publish_error(&self, provider: &str, err: &ProviderError) {
        let plugins = self.inner.plugins.lock().unwrap_or_else(|e| e.into_inner());
        for p in plugins.iter() {
            p.on_error(provider, err);
        }
    }

    /// Subscribe to messages matching `filter`. Returns an RAII handle:
    /// dropping it (or calling [`Subscription::unsubscribe`]) removes the
    /// callback.
    pub fn subscribe<F>(&self, filter: EventFilter, cb: F) -> Subscription
    where
        F: Fn(&ChannelMessage) + Send + Sync + 'static,
    {
        let cb: Callback = Arc::new(cb);
        let id = {
            let mut next = self.inner.next_id.lock().unwrap_or_else(|e| e.into_inner());
            let id = *next;
            *next += 1;
            id
        };
        self.inner
            .subscribers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Subscriber { id, filter, cb });
        Subscription {
            id,
            inner: Some(self.inner.clone()),
        }
    }
}

impl ProviderEvents for EventBus {
    fn on_message(&self, msg: ChannelMessage) {
        let _ = self.publish(msg);
    }

    fn on_error(&self, provider: &str, error: &ProviderError) {
        self.publish_error(provider, error);
    }
}

// ---------------------------------------------------------------------------
// Subscription (RAII)
// ---------------------------------------------------------------------------

/// RAII unsubscribe handle for an [`EventBus`] subscription.
///
/// Dropping the handle removes the callback. Call [`Subscription::unsubscribe`]
/// to unsubscribe explicitly (idempotent).
pub struct Subscription {
    id: u64,
    inner: Option<Arc<EventBusInner>>,
}

impl Subscription {
    /// Unsubscribe and consume the handle. Dropping without calling this
    /// also unsubscribes (via [`Drop`]).
    pub fn unsubscribe(mut self) {
        if let Some(inner) = self.inner.take() {
            inner
                .subscribers
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .retain(|s| s.id != self.id);
        }
        // prevent Drop from double-removing (inner is now None)
    }

    /// Whether the subscription is still active (not yet removed).
    pub fn is_active(&self) -> bool {
        self.inner.is_some()
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            inner
                .subscribers
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .retain(|s| s.id != self.id);
        }
    }
}

impl std::fmt::Debug for Subscription {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Subscription")
            .field("id", &self.id)
            .field("active", &self.is_active())
            .finish()
    }
}

// ---------------------------------------------------------------------------
// ProviderClient + Builder
// ---------------------------------------------------------------------------

/// Headless client that owns an [`EventBus`] and a [`ProviderRegistry`].
///
/// Providers are given the bus as their `Arc<dyn ProviderEvents>` so every
/// inbound message flows through the same plugin + filter pipeline. Outbound
/// `send` runs the `on_send` plugin chain before delegating to the registry.
pub struct ProviderClient {
    bus: EventBus,
    registry: ProviderRegistry,
}

impl ProviderClient {
    /// Create a builder.
    pub fn builder() -> ProviderClientBuilder {
        ProviderClientBuilder::new()
    }

    /// The shared event bus (typed subscriptions + plugins).
    pub fn bus(&self) -> &EventBus {
        &self.bus
    }

    /// Immutable access to the registry.
    pub fn registry(&self) -> &ProviderRegistry {
        &self.registry
    }

    /// Mutable access to the registry.
    pub fn registry_mut(&mut self) -> &mut ProviderRegistry {
        &mut self.registry
    }

    /// The `Arc<dyn ProviderEvents>` handed to providers.
    pub fn events(&self) -> Arc<dyn ProviderEvents> {
        self.registry.events().clone()
    }

    /// Subscribe to typed events.
    pub fn subscribe<F>(&self, filter: EventFilter, cb: F) -> Subscription
    where
        F: Fn(&ChannelMessage) + Send + Sync + 'static,
    {
        self.bus.subscribe(filter, cb)
    }

    /// Push a plugin at runtime (mirrors `query-core` middleware composition).
    pub fn use_plugin<P: Plugin + 'static>(&mut self, plugin: P) {
        self.bus.use_plugin(plugin);
    }

    /// Send a message through the named provider after running the outbound
    /// plugin chain. Returns `Protocol("dropped by plugin")` when a plugin
    /// returns `ControlFlow::Drop`.
    pub async fn send(
        &self,
        provider: &str,
        mut msg: SendMessage,
    ) -> Result<SendReceipt, ProviderError> {
        if self.bus.filter_send(&mut msg) == ControlFlow::Drop {
            return Err(ProviderError::Protocol("dropped by plugin".into()));
        }
        self.registry.send(provider, &msg).await
    }

    /// Convenience: publish a message directly onto the bus (e.g. for tests
    /// or for bridging from a custom source).
    pub fn publish(&self, msg: ChannelMessage) -> ControlFlow {
        self.bus.publish(msg)
    }
}

impl std::fmt::Debug for ProviderClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderClient")
            .field("bus", &self.bus)
            .field("registry_ids", &self.registry.ids())
            .finish()
    }
}

/// Builder for [`ProviderClient`].
pub struct ProviderClientBuilder {
    bus: EventBus,
    providers: Vec<Box<dyn crate::traits::ChatProvider>>,
}

impl ProviderClientBuilder {
    /// Create an empty builder with a fresh [`EventBus`].
    pub fn new() -> Self {
        ProviderClientBuilder {
            bus: EventBus::new(),
            providers: Vec::new(),
        }
    }

    /// Create a builder that reuses an existing bus (e.g. shared with
    /// `AppState::with_event_bus`).
    pub fn with_bus(bus: EventBus) -> Self {
        ProviderClientBuilder {
            bus,
            providers: Vec::new(),
        }
    }

    /// Register a provider (boxed). Call repeatedly for multiple platforms.
    pub fn provider(mut self, p: Box<dyn crate::traits::ChatProvider>) -> Self {
        self.providers.push(p);
        self
    }

    /// Add a plugin to the bus (order matters — first added runs first).
    pub fn plugin<P: Plugin + 'static>(self, plugin: P) -> Self {
        self.bus.use_plugin(plugin);
        self
    }

    /// Build the client, registering all providers against the shared bus.
    pub fn build(self) -> Result<ProviderClient, ProviderError> {
        let events: Arc<dyn ProviderEvents> = Arc::new(self.bus.clone());
        let mut registry = ProviderRegistry::new(events);
        for p in self.providers {
            registry.register(p)?;
        }
        Ok(ProviderClient {
            bus: self.bus,
            registry,
        })
    }
}

impl Default for ProviderClientBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Sender;
    use std::sync::{Arc, Mutex};

    fn msg(id: &str, channel: &str, room: &str, addressed: bool) -> ChannelMessage {
        ChannelMessage {
            id: id.into(),
            channel: channel.into(),
            channel_id: room.into(),
            sender: Sender {
                id: "peer".into(),
                name: None,
                username: None,
                avatar_url: None,
            },
            reply_target: None,
            content: vec![],
            thread_ts: None,
            attachments: vec![],
            explicitly_addressed: addressed,
            ts: 1,
            raw: None,
        }
    }

    #[test]
    fn subscribe_filter() {
        let bus = EventBus::new();
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen2 = seen.clone();
        let _sub = bus.subscribe(
            EventFilter {
                provider: Some("telegram".into()),
                channel_id: None,
                explicitly_addressed: Some(true),
            },
            move |m| {
                seen2
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(m.id.clone())
            },
        );
        bus.publish(msg("1", "telegram", "room", true));
        bus.publish(msg("2", "telegram", "room", false));
        bus.publish(msg("3", "discord", "room", true));
        let ids = seen.lock().unwrap_or_else(|e| e.into_inner()).clone();
        assert_eq!(ids, vec!["1"]);
    }

    #[test]
    fn unsubscribe_removes_callback() {
        let bus = EventBus::new();
        let seen = Arc::new(Mutex::new(0usize));
        let seen2 = seen.clone();
        let sub = bus.subscribe(EventFilter::default(), move |_| {
            *seen2.lock().unwrap_or_else(|e| e.into_inner()) += 1;
        });
        bus.publish(msg("1", "telegram", "room", false));
        assert_eq!(*seen.lock().unwrap_or_else(|e| e.into_inner()), 1);
        sub.unsubscribe();
        bus.publish(msg("2", "telegram", "room", false));
        assert_eq!(
            *seen.lock().unwrap_or_else(|e| e.into_inner()),
            1,
            "should not receive after unsubscribe"
        );
    }

    #[test]
    fn drop_unsubscribes() {
        let bus = EventBus::new();
        let seen = Arc::new(Mutex::new(0usize));
        let seen2 = seen.clone();
        {
            let _sub = bus.subscribe(EventFilter::default(), move |_| {
                *seen2.lock().unwrap_or_else(|e| e.into_inner()) += 1;
            });
            bus.publish(msg("1", "x", "room", false));
            assert_eq!(*seen.lock().unwrap_or_else(|e| e.into_inner()), 1);
        }
        bus.publish(msg("2", "x", "room", false));
        assert_eq!(*seen.lock().unwrap_or_else(|e| e.into_inner()), 1);
    }

    #[test]
    fn allowlist_blocks() {
        use crate::plugin::AllowListPlugin;
        let bus = EventBus::new();
        bus.use_plugin(AllowListPlugin::new(vec!["room-a".to_string()]));
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen2 = seen.clone();
        let _sub = bus.subscribe(EventFilter::default(), move |m| {
            seen2
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(m.channel_id.clone());
        });
        bus.publish(msg("1", "test", "room-a", false));
        bus.publish(msg("2", "test", "room-b", false));
        assert_eq!(
            *seen.lock().unwrap_or_else(|e| e.into_inner()),
            vec!["room-a"]
        );
    }

    #[test]
    fn dedup_suppresses_replay() {
        use crate::plugin::DedupPlugin;
        use std::time::Duration;
        let bus = EventBus::new();
        bus.use_plugin(DedupPlugin::new(Duration::from_secs(60)));
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen2 = seen.clone();
        let _sub = bus.subscribe(EventFilter::default(), move |m| {
            seen2
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(m.id.clone());
        });
        bus.publish(msg("dup", "test", "room", false));
        bus.publish(msg("dup", "test", "room", false));
        bus.publish(msg("other", "test", "room", false));
        assert_eq!(
            *seen.lock().unwrap_or_else(|e| e.into_inner()),
            vec!["dup", "other"]
        );
    }
}
