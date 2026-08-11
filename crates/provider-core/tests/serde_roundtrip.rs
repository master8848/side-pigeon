//! Serde round-trip tests for the api-contract schema fixtures.

use provider_core::{
    ChannelMessage, ContentPart, MediaAttachment, MediaKind, SendMessage, SendReceipt, Sender,
};

fn fixture_message() -> ChannelMessage {
    ChannelMessage {
        id: "wamid.ABC123".into(),
        channel: "telegram".into(),
        channel_id: "-1001234567890".into(),
        sender: Sender {
            id: "user_42".into(),
            name: Some("Ada".into()),
            username: Some("ada_lovelace".into()),
            avatar_url: Some("https://cdn.example/ada.png".into()),
        },
        reply_target: Some("-1001234567890".into()),
        content: vec![
            ContentPart::Text("hello world".into()),
            ContentPart::Media(MediaAttachment {
                kind: MediaKind::Image,
                url: Some("https://cdn.example/photo.jpg".into()),
                mime: Some("image/jpeg".into()),
                data: None,
                caption: Some("a photo".into()),
            }),
        ],
        thread_ts: Some("1234567890.123456".into()),
        attachments: vec![MediaAttachment {
            kind: MediaKind::Image,
            url: Some("https://cdn.example/photo.jpg".into()),
            mime: Some("image/jpeg".into()),
            data: None,
            caption: Some("a photo".into()),
        }],
        explicitly_addressed: true,
        ts: 1_752_000_000_000,
        raw: Some(serde_json::json!({ "update_id": 7 })),
    }
}

#[test]
fn channel_message_round_trip() {
    let msg = fixture_message();
    let json = serde_json::to_string(&msg).expect("serialize");
    let back: ChannelMessage = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(msg, back);
    assert!(
        json.contains(r#""kind":"Image""#),
        "media kind should serialize as enum name: {json}"
    );
    assert!(json.contains(r#""explicitly_addressed":true"#));
    assert!(json.contains(r#""ts":1752000000000"#));
}

#[test]
fn channel_message_compact_shape() {
    // Optional fields serialize as null unless skipped: verify the raw
    // platform payload round-trips when present.
    let msg = fixture_message();
    let json = serde_json::to_value(&msg).unwrap();
    assert_eq!(json["raw"]["update_id"], 7);
    assert_eq!(json["attachments"][0]["kind"], "Image");
    assert_eq!(json["content"][1]["Media"]["kind"], "Image");
}

#[test]
fn send_message_round_trip() {
    let msg = SendMessage {
        channel_id: "-1001234567890".into(),
        text: "reply to you".into(),
        reply_to: Some("wamid.ABC123".into()),
        attachments: vec![MediaAttachment::inline(
            MediaKind::Audio,
            "audio/ogg",
            vec![1, 2, 3, 255],
        )],
    };
    let json = serde_json::to_string(&msg).unwrap();
    let back: SendMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(msg, back);
    let v = serde_json::to_value(&msg).unwrap();
    assert_eq!(
        v["attachments"][0]["data"],
        serde_json::json!([1, 2, 3, 255])
    );
    assert_eq!(v["attachments"][0]["kind"], "Audio");
}

#[test]
fn send_receipt_and_sender_round_trip() {
    let receipt = SendReceipt {
        message_id: "m42".into(),
        ts: 1_752_000_000_001,
    };
    let back: SendReceipt =
        serde_json::from_str(&serde_json::to_string(&receipt).unwrap()).unwrap();
    assert_eq!(receipt, back);

    let sender = Sender {
        id: "u".into(),
        name: None,
        username: None,
        avatar_url: None,
    };
    let back: Sender = serde_json::from_str(&serde_json::to_string(&sender).unwrap()).unwrap();
    assert_eq!(sender, back);
}

#[test]
fn media_kind_and_content_part_round_trip() {
    for kind in [
        MediaKind::Image,
        MediaKind::Audio,
        MediaKind::Video,
        MediaKind::File,
        MediaKind::Sticker,
    ] {
        let back: MediaKind = serde_json::from_str(&serde_json::to_string(&kind).unwrap()).unwrap();
        assert_eq!(kind, back);
    }
    let part = ContentPart::Text("hi".into());
    let back: ContentPart = serde_json::from_str(&serde_json::to_string(&part).unwrap()).unwrap();
    assert_eq!(part, back);
}
