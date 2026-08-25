//! Plugin pipeline for the headless [`EventBus`](crate::client::EventBus).
//!
//! Plugins intercept inbound [`ChannelMessage`](crate::ChannelMessage) and
//! outbound [`SendMessage`](crate::SendMessage) before they reach subscribers
//! or the provider. The host composes them at runtime instead of forking
//! providers for pacing/policy.
//!
//! Built-ins: [`DedupPlugin`], [`AllowListPlugin`], [`LoggerPlugin`].

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::schema::{ChannelMessage, SendMessage};
use crate::ProviderError;

/// Flow-control returned by a [`Plugin`] hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlFlow {
    /// Continue to the next plugin / subscriber.
    Continue,
    /// Drop the message (do not fan out or send).
    Drop,
    /// The message was rewritten in place; continue with the mutated value.
    Rewrite,
}

/// Runtime-composable message interceptor.
///
/// Implementors are `Send + Sync` and may keep interior-mutable state
/// (e.g. `Mutex<HashMap>`). The [`EventBus`](crate::client::EventBus)
/// calls `on_message` for every inbound [`ChannelMessage`] and `on_send`
/// for every outbound [`SendMessage`] before they are delivered.
pub trait Plugin: Send + Sync {
    /// Inspect or mutate an inbound message. Return [`ControlFlow::Drop`] to
    /// suppress fan-out.
    fn on_message(&self, msg: &mut ChannelMessage) -> ControlFlow;

    /// Inspect or mutate an outbound message. Return [`ControlFlow::Drop`] to
    /// abort the send.
    fn on_send(&self, msg: &mut SendMessage) -> ControlFlow;

    /// Observe an asynchronous provider error (always no-op by default).
    fn on_error(&self, _provider: &str, _err: &ProviderError) {}
}

/// De-duplicate inbound messages by `(channel, id)` within a sliding window.
///
/// A message whose `id` was seen less than `window` ago is dropped. The
/// cache is bounded by expiring entries older than `window`.
pub struct DedupPlugin {
    /// Deduplication window.
    pub window: Duration,
    cache: Mutex<HashMap<String, Instant>>,
}

impl DedupPlugin {
    /// Create a dedup plugin with the given window.
    pub fn new(window: Duration) -> Self {
        DedupPlugin {
            window,
            cache: Mutex::new(HashMap::new()),
        }
    }
}

impl Plugin for DedupPlugin {
    fn on_message(&self, msg: &mut ChannelMessage) -> ControlFlow {
        let key = format!("{}:{}", msg.channel, msg.id);
        let now = Instant::now();
        let mut cache = self.cache.lock().unwrap();
        // Evict expired entries to bound memory.
        cache.retain(|_, t| now.duration_since(*t) < self.window);
        if let Some(last) = cache.get(&key) {
            if now.duration_since(*last) < self.window {
                return ControlFlow::Drop;
            }
        }
        cache.insert(key, now);
        ControlFlow::Continue
    }

    fn on_send(&self, _msg: &mut SendMessage) -> ControlFlow {
        ControlFlow::Continue
    }
}

/// Allow-list inbound messages by `channel_id` (room).
///
/// When `rooms` is empty every message passes. Otherwise only messages whose
/// `channel_id` is in the set are forwarded; the rest are dropped.
pub struct AllowListPlugin {
    /// Allowed room ids. Empty = allow all.
    pub rooms: HashSet<String>,
}

impl AllowListPlugin {
    /// Create an allow-list from an iterator of room ids.
    pub fn new(rooms: impl IntoIterator<Item = String>) -> Self {
        AllowListPlugin {
            rooms: rooms.into_iter().collect(),
        }
    }
}

impl Plugin for AllowListPlugin {
    fn on_message(&self, msg: &mut ChannelMessage) -> ControlFlow {
        if self.rooms.is_empty() {
            return ControlFlow::Continue;
        }
        if self.rooms.contains(&msg.channel_id) {
            ControlFlow::Continue
        } else {
            ControlFlow::Drop
        }
    }

    fn on_send(&self, msg: &mut SendMessage) -> ControlFlow {
        if self.rooms.is_empty() {
            return ControlFlow::Continue;
        }
        if self.rooms.contains(&msg.channel_id) {
            ControlFlow::Continue
        } else {
            ControlFlow::Drop
        }
    }
}

/// Log every hook invocation to `tracing` / stderr (no external deps).
///
/// Useful as the last plugin in a chain to observe what survived filtering.
pub struct LoggerPlugin;

impl Plugin for LoggerPlugin {
    fn on_message(&self, msg: &mut ChannelMessage) -> ControlFlow {
        // Use eprintln so we do not require a `tracing` dependency in
        // provider-core (keep the crate lean per the architecture doc).
        eprintln!(
            "[LoggerPlugin] on_message channel={} id={} room={} addressed={}",
            msg.channel, msg.id, msg.channel_id, msg.explicitly_addressed
        );
        ControlFlow::Continue
    }

    fn on_send(&self, msg: &mut SendMessage) -> ControlFlow {
        eprintln!(
            "[LoggerPlugin] on_send room={} text_len={}",
            msg.channel_id,
            msg.text.len()
        );
        ControlFlow::Continue
    }

    fn on_error(&self, provider: &str, err: &ProviderError) {
        eprintln!("[LoggerPlugin] on_error provider={provider} err={err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::Sender;

    fn msg(id: &str, room: &str) -> ChannelMessage {
        ChannelMessage {
            id: id.into(),
            channel: "test".into(),
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
            explicitly_addressed: false,
            ts: 1,
            raw: None,
        }
    }

    #[test]
    fn allowlist_blocks() {
        let plugin = AllowListPlugin::new(vec!["room-a".to_string()]);
        let mut ok = msg("1", "room-a");
        assert_eq!(plugin.on_message(&mut ok), ControlFlow::Continue);
        let mut blocked = msg("2", "room-b");
        assert_eq!(plugin.on_message(&mut blocked), ControlFlow::Drop);
    }

    #[test]
    fn allowlist_empty_allows_all() {
        let plugin = AllowListPlugin::new(Vec::<String>::new());
        let mut a = msg("1", "room-a");
        let mut b = msg("2", "room-b");
        assert_eq!(plugin.on_message(&mut a), ControlFlow::Continue);
        assert_eq!(plugin.on_message(&mut b), ControlFlow::Continue);
    }

    #[test]
    fn dedup_suppresses_replay() {
        let plugin = DedupPlugin::new(Duration::from_secs(60));
        let mut a = msg("dup", "room");
        assert_eq!(plugin.on_message(&mut a), ControlFlow::Continue);
        let mut replay = msg("dup", "room");
        assert_eq!(plugin.on_message(&mut replay), ControlFlow::Drop);
        // Different id passes
        let mut b = msg("other", "room");
        assert_eq!(plugin.on_message(&mut b), ControlFlow::Continue);
    }

    #[test]
    fn dedup_different_channel_not_deduped() {
        let plugin = DedupPlugin::new(Duration::from_secs(60));
        let mut a = ChannelMessage {
            channel: "telegram".into(),
            ..msg("same-id", "room")
        };
        let mut b = ChannelMessage {
            channel: "discord".into(),
            ..msg("same-id", "room")
        };
        assert_eq!(plugin.on_message(&mut a), ControlFlow::Continue);
        assert_eq!(plugin.on_message(&mut b), ControlFlow::Continue);
    }
}
