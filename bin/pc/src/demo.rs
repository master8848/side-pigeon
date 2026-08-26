// TODO(P1): extract to crates/provider-demo crate - see docs/POLISH.md P1
//! Built-in `demo` provider: no network, echoes every send back as an inbound
//! message and announces itself on `start()`. Used for local testing of the
//! full stdio JSON-RPC flow without a real platform.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use provider_core::{
    ChannelMessage, ChatProvider, ContentPart, ProviderError, ProviderEvents, SendMessage,
    SendReceipt, Sender,
};

static SEQ: AtomicU64 = AtomicU64::new(0);

/// Echo provider (`id = "demo"`).
pub struct DemoProvider {
    events: Arc<dyn ProviderEvents>,
    name: String,
}

impl DemoProvider {
    /// Build from `{"name": "..."}` config (optional).
    pub fn new(events: Arc<dyn ProviderEvents>, config: &serde_json::Value) -> Self {
        let name = config
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("demo")
            .to_string();
        DemoProvider { events, name }
    }

    fn now() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }

    fn message(id: &str, text: &str, channel_id: &str, name: &str) -> ChannelMessage {
        ChannelMessage {
            id: id.to_string(),
            channel: "demo".to_string(),
            channel_id: channel_id.to_string(),
            sender: Sender {
                id: "demo-bot".to_string(),
                name: Some(name.to_string()),
                username: None,
                avatar_url: None,
            },
            reply_target: Some(channel_id.to_string()),
            content: vec![ContentPart::Text(text.to_string())],
            thread_ts: None,
            attachments: vec![],
            explicitly_addressed: false,
            ts: Self::now(),
            raw: None,
        }
    }
}

#[async_trait]
impl ChatProvider for DemoProvider {
    fn id(&self) -> &'static str {
        "demo"
    }

    async fn start(&mut self) -> Result<(), ProviderError> {
        let msg = Self::message(
            &format!("demo-{}", SEQ.fetch_add(1, Ordering::Relaxed)),
            "demo provider started; ready to echo",
            "demo-room",
            &self.name,
        );
        self.events.on_message(msg);
        Ok(())
    }

    async fn stop(&mut self) -> Result<(), ProviderError> {
        Ok(())
    }

    async fn send(&self, msg: &SendMessage) -> Result<SendReceipt, ProviderError> {
        let echo = Self::message(
            &format!("demo-{}", SEQ.fetch_add(1, Ordering::Relaxed)),
            &format!("echo: {}", msg.text),
            &msg.channel_id,
            &self.name,
        );
        self.events.on_message(echo);
        Ok(SendReceipt {
            message_id: format!("demo-{}", SEQ.fetch_add(1, Ordering::Relaxed)),
            ts: Self::now(),
        })
    }
}
