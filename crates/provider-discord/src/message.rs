//! MESSAGE_CREATE normalization -> [`ChannelMessage`](provider_core::ChannelMessage).
//!
//! Pure parsing (no network), so the transport and tests can exercise it with
//! fixture JSON.

use provider_core::{ChannelMessage, ContentPart, MediaAttachment, MediaKind, Sender};
use serde::Deserialize;

/// The `d` payload of a MESSAGE_CREATE dispatch event (fields we model).
/// Discord wire shape (fields mirror the API; some retained for future use).
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub(crate) struct MessageCreate {
    pub id: String,
    pub channel_id: String,
    #[serde(default)]
    pub guild_id: Option<String>,
    pub author: User,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub message_reference: Option<MessageReference>,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    #[serde(default)]
    pub mentions: Vec<User>,
    #[serde(default)]
    pub timestamp: Option<String>,
}

/// Discord wire shape (fields mirror the API; some retained for future use).
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub(crate) struct User {
    pub id: String,
    pub username: String,
    #[serde(default)]
    pub global_name: Option<String>,
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub bot: Option<bool>,
}

/// Discord wire shape (fields mirror the API; some retained for future use).
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub(crate) struct MessageReference {
    #[serde(default)]
    pub message_id: Option<String>,
    #[serde(default)]
    pub channel_id: Option<String>,
    #[serde(default)]
    pub guild_id: Option<String>,
}

/// Discord wire shape (fields mirror the API; some retained for future use).
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub(crate) struct Attachment {
    pub id: String,
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
}

/// Discord epoch (2015-01-01T00:00:00Z) in epoch millis.
const DISCORD_EPOCH_MS: i64 = 1_420_070_400_000;

/// Discord snowflake -> creation time in epoch millis.
pub(crate) fn snowflake_ts(id: &str) -> Option<i64> {
    let n: u64 = id.parse().ok()?;
    Some((n >> 22) as i64 + DISCORD_EPOCH_MS)
}

fn media_kind(mime: Option<&str>, filename: &str) -> MediaKind {
    match mime {
        Some(m) if m.starts_with("image/") => MediaKind::Image,
        Some(m) if m.starts_with("audio/") => MediaKind::Audio,
        Some(m) if m.starts_with("video/") => MediaKind::Video,
        _ => {
            let f = filename.to_ascii_lowercase();
            if f.ends_with(".png")
                || f.ends_with(".jpg")
                || f.ends_with(".jpeg")
                || f.ends_with(".gif")
                || f.ends_with(".webp")
            {
                MediaKind::Image
            } else if f.ends_with(".mp3") || f.ends_with(".wav") || f.ends_with(".ogg") {
                MediaKind::Audio
            } else if f.ends_with(".mp4") || f.ends_with(".webm") || f.ends_with(".mov") {
                MediaKind::Video
            } else {
                MediaKind::File
            }
        }
    }
}

fn avatar_url(user_id: &str, hash: &str) -> String {
    let ext = if hash.starts_with("a_") { "gif" } else { "png" };
    format!("https://cdn.discordapp.com/avatars/{user_id}/{hash}.{ext}")
}

/// Normalize one raw MESSAGE_CREATE payload into a [`ChannelMessage`].
///
/// * `id` = message snowflake, `channel` = `"discord"`
/// * `channel_id` = channel snowflake
/// * `sender` from `author{id, username, global_name, avatar}`
/// * `content` = message content (text); attachments become `MediaAttachment`s
/// * `reply_target` / `thread_ts` from `message_reference.message_id`
/// * `ts` = snowflake timestamp (ISO-8601 fallback)
/// * `explicitly_addressed` = the bot's own id appears in `mentions`
/// * `raw` = full MESSAGE_CREATE payload
pub fn parse_message_create(
    value: &serde_json::Value,
    self_user_id: Option<&str>,
) -> Option<ChannelMessage> {
    let m: MessageCreate = serde_json::from_value(value.clone()).ok()?;

    let mut content = Vec::new();
    if !m.content.is_empty() {
        content.push(ContentPart::Text(m.content.clone()));
    }
    let mut attachments = Vec::new();
    for a in &m.attachments {
        attachments.push(MediaAttachment {
            kind: media_kind(a.content_type.as_deref(), &a.filename),
            url: a.url.clone(),
            mime: a.content_type.clone(),
            data: None,
            caption: None,
        });
    }

    let thread = m
        .message_reference
        .as_ref()
        .and_then(|r| r.message_id.clone());
    // Snowflake ids embed a millisecond timestamp and are authoritative for
    // MESSAGE_CREATE; the ISO-8601 fallback (hand-rolled civil-date math) was
    // removed per review — snowflake parsing is the single source of truth.
    let ts = snowflake_ts(&m.id).unwrap_or(0);

    Some(ChannelMessage {
        id: m.id.clone(),
        channel: "discord".to_string(),
        channel_id: m.channel_id,
        sender: Sender {
            id: m.author.id.clone(),
            name: m
                .author
                .global_name
                .clone()
                .or_else(|| Some(m.author.username.clone())),
            username: Some(m.author.username.clone()),
            avatar_url: m
                .author
                .avatar
                .as_deref()
                .map(|h| avatar_url(&m.author.id, h)),
        },
        reply_target: thread.clone(),
        content,
        thread_ts: thread,
        attachments,
        explicitly_addressed: self_user_id
            .is_some_and(|uid| m.mentions.iter().any(|u| u.id == uid)),
        ts,
        raw: Some(value.clone()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use provider_core::ContentPart;

    const FIXTURE: &str = r#"{
        "id": "1196552072724480000",
        "channel_id": "991234567890123456",
        "guild_id": "881234567890123456",
        "author": {
            "id": "551234567890123456",
            "username": "ada",
            "global_name": "Ada Lovelace",
            "avatar": "a_1234567890abcdef",
            "bot": false
        },
        "content": "hello from discord <@1001>",
        "timestamp": "2024-01-15T20:30:45.123000+00:00",
        "message_reference": {
            "message_id": "1107462566582882300",
            "channel_id": "991234567890123456",
            "guild_id": "881234567890123456"
        },
        "mentions": [{"id": "1001", "username": "mybot"}],
        "attachments": [
            {"id": "1", "filename": "cat.png", "url": "https://cdn.discordapp.com/attachments/1/cat.png", "content_type": "image/png"},
            {"id": "2", "filename": "notes.txt", "url": "https://cdn.discordapp.com/attachments/1/notes.txt", "content_type": "text/plain"}
        ]
    }"#;

    #[test]
    fn parse_message_create_fixture() {
        let v: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        let msg = parse_message_create(&v, Some("1001")).expect("parses");

        assert_eq!(msg.channel, "discord");
        assert_eq!(msg.channel_id, "991234567890123456");
        assert_eq!(msg.sender.id, "551234567890123456");
        assert_eq!(msg.sender.name.as_deref(), Some("Ada Lovelace"));
        assert_eq!(msg.sender.username.as_deref(), Some("ada"));
        assert_eq!(
            msg.sender.avatar_url.as_deref(),
            Some("https://cdn.discordapp.com/avatars/551234567890123456/a_1234567890abcdef.gif")
        );
        assert_eq!(msg.content.len(), 1);
        assert!(
            matches!(&msg.content[0], ContentPart::Text(t) if t == "hello from discord <@1001>")
        );
        assert_eq!(
            msg.reply_target.as_deref(),
            Some("1107462566582882300"),
            "reply_target from message_reference"
        );
        assert_eq!(
            msg.thread_ts.as_deref(),
            Some("1107462566582882300"),
            "thread from message_reference"
        );
        assert!(msg.explicitly_addressed, "bot id 1001 is mentioned");
        assert_eq!(msg.attachments.len(), 2);
        assert!(matches!(msg.attachments[0].kind, MediaKind::Image));
        assert_eq!(
            msg.attachments[0].url.as_deref(),
            Some("https://cdn.discordapp.com/attachments/1/cat.png")
        );
        assert_eq!(msg.attachments[0].mime.as_deref(), Some("image/png"));
        assert!(matches!(msg.attachments[1].kind, MediaKind::File));
        assert!(msg.raw.is_some());

        // snowflake ts: id >> 22 + 1420070400000 (authoritative; the ISO
        // fallback was removed — see review)
        assert_eq!(msg.ts, snowflake_ts("1196552072724480000").unwrap());
    }

    #[test]
    fn explicitly_addressed_false_when_bot_not_mentioned() {
        let v: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        let msg = parse_message_create(&v, Some("9999")).unwrap();
        assert!(!msg.explicitly_addressed);
    }

    #[test]
    fn garbage_does_not_parse() {
        let v = serde_json::json!({"not": "a message create"});
        assert!(parse_message_create(&v, None).is_none());
    }

    #[test]
    fn snowflake_timestamps() {
        assert_eq!(snowflake_ts("175928847299117063"), Some(1_462_015_105_796));
        assert_eq!(snowflake_ts("not-a-snowflake"), None);
    }
}
