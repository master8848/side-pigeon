# provider-core API contract v0.1

`crates/provider-core` MUST expose exactly these items (this is the contract; provider crates and the transport build against it):

```rust
// schema
pub struct ChannelMessage {
    pub id: String,
    pub channel: String,          // provider id, e.g. "telegram"
    pub channel_id: String,       // chat/room id
    pub sender: Sender,
    pub reply_target: Option<String>,
    pub content: Vec<ContentPart>, // text + media refs
    pub thread_ts: Option<String>,
    pub attachments: Vec<MediaAttachment>,
    pub explicitly_addressed: bool,
    pub ts: i64,                  // epoch millis
    pub raw: Option<serde_json::Value>,
}
pub struct Sender { pub id: String, pub name: Option<String>, pub username: Option<String>, pub avatar_url: Option<String> }
pub enum ContentPart { Text(String), Media(MediaAttachment) }
pub struct MediaAttachment { pub kind: MediaKind, pub url: Option<String>, pub mime: Option<String>, pub data: Option<Vec<u8>>, pub caption: Option<String> }
pub enum MediaKind { Image, Audio, Video, File, Sticker }
pub struct SendMessage { pub channel_id: String, pub text: String, pub reply_to: Option<String>, pub attachments: Vec<MediaAttachment> }  // NOTE: derives serde(default) so reply_to/attachments are optional on the wire
pub struct SendReceipt { pub message_id: String, pub ts: i64 }

// capabilities (split, NOT monolithic)
// NOTE: implemented with #[async_trait] (providers are Box<dyn ChatProvider> in the registry); signatures otherwise identical.
#[async_trait::async_trait]
pub trait ChatProvider: Send + Sync {
    fn id(&self) -> &'static str;
    async fn start(&mut self) -> Result<(), ProviderError>;
    async fn stop(&mut self) -> Result<(), ProviderError>;
    async fn send(&self, msg: &SendMessage) -> Result<SendReceipt, ProviderError>;
}
pub trait ProviderEvents: Send + Sync {
    fn on_message(&self, msg: ChannelMessage); // implemented by the transport; providers call it
}

// errors
#[derive(thiserror::Error, Debug)]
pub enum ProviderError {
    #[error("configuration error: {0}")] Config(String),
    #[error("auth error: {0}")] Auth(String),
    #[error("network error: {0}")] Network(String),
    #[error("rate limited: {0}")] RateLimit(String),
    #[error("protocol error: {0}")] Protocol(String),
    #[error("other: {0}")] Other(String),
}
```

JSON-RPC method surface (transport, ACP-style): requests `initialize`, `capabilities`, `listen` (start providers), `send`, `shutdown`; server->client notifications `event.message` (ChannelMessage as JSON), `event.draft`, `event.choice`, `event.error`.
