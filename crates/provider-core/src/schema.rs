//! Unified message schema (ported from ZeroClaw, MIT/Apache-2.0).
//!
//! Every type in this module derives `Serialize`/`Deserialize` and is used
//! directly as the JSON-RPC wire type by `provider-transport`.

use serde::{Deserialize, Serialize};

/// A normalized inbound message from any provider.
///
/// One struct for every platform — no per-platform event types leak past the
/// adapter. `content` holds text + inline media refs; `attachments` holds the
/// full media list. `raw` preserves the platform payload for debugging and is
/// optional (and never required to round-trip).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelMessage {
    /// Platform message id (e.g. `wamid.xxx`, snowflake, update_id).
    pub id: String,
    /// Provider id, e.g. `"telegram"`.
    pub channel: String,
    /// Chat/room id this message belongs to.
    pub channel_id: String,
    /// The peer that sent the message.
    pub sender: Sender,
    /// Where to reply (chat id / thread / email address).
    pub reply_target: Option<String>,
    /// Text + media refs, in order.
    pub content: Vec<ContentPart>,
    /// Platform thread anchor (Slack `ts`, Discord thread id, ...).
    pub thread_ts: Option<String>,
    /// Full media attachment list.
    pub attachments: Vec<MediaAttachment>,
    /// Platform-level @-mention observed.
    pub explicitly_addressed: bool,
    /// Epoch millis.
    pub ts: i64,
    /// Raw platform payload (diagnostics); kept off any internal routing.
    pub raw: Option<serde_json::Value>,
}

/// The peer that sent a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sender {
    /// Normalized peer handle.
    pub id: String,
    /// Display name, when known.
    pub name: Option<String>,
    /// Platform username/handle, when known.
    pub username: Option<String>,
    /// Avatar URL, when known.
    pub avatar_url: Option<String>,
}

/// One ordered part of a message body: text or a media reference.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ContentPart {
    /// Plain text.
    Text(String),
    /// A media reference (image/audio/video/file/sticker).
    Media(MediaAttachment),
}

/// A media attachment. Bytes are optional: ship `data` (base64-encoded by the
/// JSON-RPC layer) for small files; use `url` refs / temp files for large ones.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaAttachment {
    /// Media classification.
    pub kind: MediaKind,
    /// Remote URL, when the media is referenced rather than inline.
    pub url: Option<String>,
    /// MIME type, when known.
    pub mime: Option<String>,
    /// Inline bytes (small files only).
    pub data: Option<Vec<u8>>,
    /// Caption / alt text.
    pub caption: Option<String>,
}

/// Media classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaKind {
    /// Image.
    Image,
    /// Audio (voice notes, music).
    Audio,
    /// Video.
    Video,
    /// Generic file/document.
    File,
    /// Sticker.
    Sticker,
}

/// A normalized outbound message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SendMessage {
    /// Chat/room id to deliver to.
    pub channel_id: String,
    /// Text body.
    pub text: String,
    /// Platform message id this replies to, if any.
    pub reply_to: Option<String>,
    /// Attachments to send alongside the text.
    pub attachments: Vec<MediaAttachment>,
}

/// A normalized send confirmation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendReceipt {
    /// Platform message id of the sent message.
    pub message_id: String,
    /// Epoch millis when the platform accepted it.
    pub ts: i64,
}

impl SendMessage {
    /// Convenience constructor for a plain text message.
    pub fn new(channel_id: impl Into<String>, text: impl Into<String>) -> Self {
        SendMessage {
            channel_id: channel_id.into(),
            text: text.into(),
            reply_to: None,
            attachments: Vec::new(),
        }
    }
}

impl MediaAttachment {
    /// Convenience constructor for an inline-bytes attachment.
    pub fn inline(kind: MediaKind, mime: impl Into<String>, data: Vec<u8>) -> Self {
        MediaAttachment {
            kind,
            url: None,
            mime: Some(mime.into()),
            data: Some(data),
            caption: None,
        }
    }
}
