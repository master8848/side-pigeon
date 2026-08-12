//! Discord Gateway v10 wire protocol: opcodes, payload types, frame builders.
//!
//! `encoding=json` only; hand-rolled on `tokio-tungstenite` (no `serenity`),
//! per the ZeroClaw pattern. See <https://discord.com/developers/docs/topics/gateway>.

use serde::Deserialize;

/// Dispatch (server event).
pub(crate) const OP_DISPATCH: u8 = 0;
/// Heartbeat (client -> server; `d` = last sequence number, or null).
pub(crate) const OP_HEARTBEAT: u8 = 1;
/// Identify (client -> server, first connection).
pub(crate) const OP_IDENTIFY: u8 = 2;
/// Resume (client -> server, after an unexpected disconnect).
pub(crate) const OP_RESUME: u8 = 6;
/// Server asks the client to reconnect.
pub(crate) const OP_RECONNECT: u8 = 7;
/// Server invalidated the session (resumable flag in `d`).
pub(crate) const OP_INVALID_SESSION: u8 = 9;
/// Hello (server -> client; carries `heartbeat_interval`).
pub(crate) const OP_HELLO: u8 = 10;
/// Heartbeat acknowledged.
pub(crate) const OP_HEARTBEAT_ACK: u8 = 11;

/// `GUILDS` intent.
pub const INTENTS_GUILDS: u64 = 1 << 0;
/// `GUILD_MESSAGES` intent.
pub const INTENTS_GUILD_MESSAGES: u64 = 1 << 9;
/// `DIRECT_MESSAGES` intent.
pub const INTENTS_DIRECT_MESSAGES: u64 = 1 << 12;
/// `MESSAGE_CONTENT` intent (privileged — enable in the Discord developer portal).
pub const INTENTS_MESSAGE_CONTENT: u64 = 1 << 15;

/// Default intents for this provider.
pub const DEFAULT_INTENTS: u64 =
    INTENTS_GUILDS | INTENTS_GUILD_MESSAGES | INTENTS_DIRECT_MESSAGES | INTENTS_MESSAGE_CONTENT;

/// One gateway frame: `{"op":..,"d":..,"s":..,"t":..}`.
#[derive(Debug, Deserialize)]
pub(crate) struct GatewayPayload {
    pub op: u8,
    #[serde(default)]
    pub d: Option<serde_json::Value>,
    #[serde(default)]
    pub s: Option<u64>,
    #[serde(default)]
    pub t: Option<String>,
}

/// HELLO payload (`d` of op 10).
#[derive(Debug, Deserialize)]
pub(crate) struct Hello {
    pub heartbeat_interval: u64,
}

/// READY payload (`d` of op 0, event "READY").
#[derive(Debug, Deserialize)]
pub(crate) struct Ready {
    pub session_id: String,
    pub resume_gateway_url: String,
    #[serde(default)]
    pub user: Option<ReadyUser>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ReadyUser {
    pub id: String,
}

/// Build the IDENTIFY (op 2) frame.
pub(crate) fn identify_payload(token: &str, intents: u64) -> String {
    serde_json::json!({
        "op": OP_IDENTIFY,
        "d": {
            "token": token,
            "intents": intents,
            "properties": {
                "os": std::env::consts::OS,
                "browser": "provider-connect",
                "device": "provider-connect",
            },
            "compress": false,
        },
    })
    .to_string()
}

/// Build the RESUME (op 6) frame.
pub(crate) fn resume_payload(token: &str, session_id: &str, seq: u64) -> String {
    serde_json::json!({
        "op": OP_RESUME,
        "d": {
            "token": token,
            "session_id": session_id,
            "seq": seq,
        },
    })
    .to_string()
}

/// Build a HEARTBEAT (op 1) frame; `d` is the last sequence number or null.
pub(crate) fn heartbeat_payload(seq: u64) -> String {
    let d = if seq == 0 {
        serde_json::Value::Null
    } else {
        serde_json::json!(seq)
    };
    serde_json::json!({ "op": OP_HEARTBEAT, "d": d }).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identify_payload_has_op_intents_and_token() {
        let p: serde_json::Value =
            serde_json::from_str(&identify_payload("tok", DEFAULT_INTENTS)).unwrap();
        assert_eq!(p["op"], 2);
        assert_eq!(p["d"]["token"], "tok");
        assert_eq!(p["d"]["intents"], 37377);
        assert_eq!(p["d"]["compress"], false);
        assert!(p["d"]["properties"]["os"].is_string());
    }

    #[test]
    fn resume_payload_has_session_and_seq() {
        let p: serde_json::Value =
            serde_json::from_str(&resume_payload("tok", "sess1", 42)).unwrap();
        assert_eq!(p["op"], 6);
        assert_eq!(p["d"]["session_id"], "sess1");
        assert_eq!(p["d"]["seq"], 42);
    }

    #[test]
    fn heartbeat_payload_uses_null_for_zero_seq() {
        let p: serde_json::Value = serde_json::from_str(&heartbeat_payload(0)).unwrap();
        assert_eq!(p["op"], 1);
        assert!(p["d"].is_null());
        let p: serde_json::Value = serde_json::from_str(&heartbeat_payload(7)).unwrap();
        assert_eq!(p["d"], 7);
    }
}
