//! Typed payloads for the server→client notifications.
//!
//! Wire vocabulary (see `docs/research/zeroclaw.md` §5.4–5.5): drafts stream
//! reply content, choices surface approval/selection prompts, errors carry a
//! stable [`ProviderError`](provider_core::ProviderError) code + message.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `event.message` — an inbound [`ChannelMessage`](provider_core::ChannelMessage).
pub const EVENT_MESSAGE: &str = "event.message";
/// `event.draft` — a streaming draft update.
pub const EVENT_DRAFT: &str = "event.draft";
/// `event.choice` — an approval/choice prompt.
pub const EVENT_CHOICE: &str = "event.choice";
/// `event.error` — an asynchronous provider error.
pub const EVENT_ERROR: &str = "event.error";

/// Payload of `event.draft`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DraftEvent {
    /// Provider id, e.g. `"telegram"`.
    pub channel: String,
    /// Chat/room id the draft targets.
    pub channel_id: String,
    /// Provider message id being edited (`None` = new draft).
    pub message_id: Option<String>,
    /// Latest draft text.
    pub content: String,
    /// `true` when the draft is final and must replace the message.
    #[serde(default)]
    pub done: bool,
}

/// Payload of `event.choice` — an approval/selection prompt with a fixed
/// wire-token vocabulary in `choices`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChoiceEvent {
    /// Provider id.
    pub channel: String,
    /// Chat/room id.
    pub channel_id: String,
    /// Correlation token; approval replies must carry it back.
    pub reference: String,
    /// Human-readable prompt.
    pub prompt: String,
    /// Fixed choice tokens, e.g. `["approve", "deny", "edit", "revise"]`.
    pub choices: Vec<String>,
}

/// Payload of `event.error`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorEvent {
    /// Provider id the error came from, when known.
    pub provider: Option<String>,
    /// Stable error code (see `provider_core::ProviderError::kind` / JSON-RPC mapping).
    pub code: i64,
    /// Human-readable message.
    pub message: String,
    /// Extra diagnostics.
    pub data: Option<Value>,
}
