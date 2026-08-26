//! Discord provider for `provider-connect`.
//!
//! Hand-rolled [Discord Gateway v10](https://discord.com/developers/docs/topics/gateway)
//! WebSocket client on `tokio-tungstenite` (JSON encoding) + REST messaging on
//! `reqwest` — **no `serenity` SDK**, per the ZeroClaw pattern.
//!
//! * Inbound: connect `wss://gateway.discord.gg/?v=10&encoding=json`, IDENTIFY
//!   with intents `GUILDS | GUILD_MESSAGES | DIRECT_MESSAGES | MESSAGE_CONTENT`,
//!   heartbeat every `heartbeat_interval` (immediate first beat, ACK tracking
//!   with reconnect on >3 missed beats), resume on reconnect (`RESUME` with the
//!   cached session id + sequence), handle `RECONNECT` / `INVALID_SESSION`.
//!   `READY`/`GUILD_CREATE` populate minimal cached state (session, guild
//!   id->name, bot user id); `MESSAGE_CREATE` is normalized into a
//!   [`ChannelMessage`] and delivered to the [`ProviderEvents`] sink.
//! * Outbound: `send()` -> REST `POST /channels/{id}/messages` with
//!   `Authorization: Bot <token>` -> [`SendReceipt`].
//! * Errors: gateway close 4004 -> `Auth` (fatal), HTTP 401/403 -> `Auth`,
//!   429 -> `RateLimit`, protocol violations -> `Protocol`, network failures ->
//!   `Network` (reconnect with capped backoff).
//!
//! ## Example
//!
//! ```ignore
//! use std::sync::Arc;
//! use provider_core::{ChatProvider, ProviderEvents, ChannelMessage};
//! use provider_discord::DiscordProvider;
//!
//! struct Sink;
//! impl ProviderEvents for Sink {
//!     fn on_message(&self, _msg: ChannelMessage) { /* forward to transport */ }
//! }
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let mut provider = DiscordProvider::new("YOUR_BOT_TOKEN".to_string(), Arc::new(Sink));
//!     provider.start().await?;
//!     // ... run agent loop ...
//!     provider.stop().await?;
//!     Ok(())
//! }
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::{SinkExt, Stream, StreamExt};
use provider_core::{
    ChatProvider, ProviderError, ProviderEvents, SendMessage, SendReceipt,
    TRANSIENT_ERROR_EVENT_THRESHOLD,
};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{debug, error, info, trace, warn};

mod gateway;
mod heartbeat;
mod message;

pub use heartbeat::Heartbeat;
pub use message::parse_message_create;

use gateway::{
    heartbeat_payload, identify_payload, resume_payload, GatewayPayload, Hello, Ready,
    DEFAULT_INTENTS, OP_DISPATCH, OP_HEARTBEAT, OP_HEARTBEAT_ACK, OP_HELLO, OP_INVALID_SESSION,
    OP_RECONNECT,
};
use message::snowflake_ts;

/// How the current gateway connection ended.
enum RunOutcome {
    /// Shutdown requested.
    Shutdown,
    /// Permanent failure — do not reconnect.
    Fatal(ProviderError),
    /// Disconnected; reconnect (resuming if a session is cached).
    Reconnect { healthy: bool },
}

/// Cached session state needed to resume after a disconnect.
#[derive(Debug, Clone)]
struct SessionState {
    session_id: String,
    resume_url: String,
    seq: u64,
}

/// Discord provider: Gateway v10 WebSocket inbound, REST outbound.
///
/// `start()` spawns the gateway task (connect -> IDENTIFY/RESUME -> heartbeat
/// -> dispatch); inbound messages are delivered through the
/// [`ProviderEvents`] sink supplied at construction. `stop()` closes the
/// connection and cancels the task. The provider may be restarted after
/// `stop()`; the session is kept in-process so a restart resumes cleanly.
pub struct DiscordProvider {
    token: String,
    gateway_url: String,
    rest_base: String,
    intents: u64,
    client: reqwest::Client,
    events: Arc<dyn ProviderEvents>,

    // gateway runtime state
    session: Arc<Mutex<Option<SessionState>>>,
    guilds: Arc<Mutex<HashMap<String, String>>>,
    self_user_id: Arc<Mutex<Option<String>>>,
    last_error: Arc<Mutex<Option<ProviderError>>>,
    task: Option<JoinHandle<()>>,
    shutdown_tx: Option<watch::Sender<bool>>,
}

impl std::fmt::Debug for DiscordProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscordProvider")
            .field("gateway_url", &self.gateway_url)
            .field("rest_base", &self.rest_base)
            .field("intents", &self.intents)
            .field("started", &self.task.is_some())
            .finish_non_exhaustive()
    }
}

impl DiscordProvider {
    /// Create a provider for `token` that delivers inbound messages to `events`.
    ///
    /// Defaults: gateway `wss://gateway.discord.gg/?v=10&encoding=json`, REST
    /// `https://discord.com/api/v10`, intents = GUILDS | GUILD_MESSAGES |
    /// DIRECT_MESSAGES | MESSAGE_CONTENT. NOTE: `MESSAGE_CONTENT` is a
    /// *privileged* intent — it must be enabled in the Discord developer portal.
    pub fn new(token: impl Into<String>, events: Arc<dyn ProviderEvents>) -> Self {
        Self {
            token: token.into(),
            gateway_url: "wss://gateway.discord.gg/?v=10&encoding=json".to_string(),
            rest_base: "https://discord.com/api/v10".to_string(),
            intents: DEFAULT_INTENTS,
            // Default REST timeout mirrors telegram's 60 s (the review flagged
            // discord's reqwest::Client::new() no-timeout behavior).
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(60))
                .build()
                .expect("reqwest client build cannot fail"),

            events,
            session: Arc::new(Mutex::new(None)),
            guilds: Arc::new(Mutex::new(HashMap::new())),
            self_user_id: Arc::new(Mutex::new(None)),
            last_error: Arc::new(Mutex::new(None)),
            task: None,
            shutdown_tx: None,
        }
    }

    /// Override the gateway WebSocket URL (tests / proxies).
    pub fn with_gateway_url(mut self, url: impl Into<String>) -> Self {
        self.gateway_url = url.into();
        self
    }

    /// Override the REST API base (tests / self-hosted gateways).
    pub fn with_rest_base(mut self, base: impl Into<String>) -> Self {
        self.rest_base = base.into().trim_end_matches('/').to_string();
        self
    }

    /// Override the gateway intents bitmask (default [`DEFAULT_INTENTS`]).
    pub fn with_intents(mut self, intents: u64) -> Self {
        self.intents = intents;
        self
    }

    /// Set the per-request HTTP timeout for REST calls (default: reqwest's
    /// no-timeout behavior).
    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .expect("valid reqwest client");
        self
    }

    /// Take the last fatal error, if any. Consumes it (no `Clone` requirement
    /// on the caller side).
    pub fn take_last_error(&self) -> Option<ProviderError> {
        self.last_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    /// Cached guild state (id -> name) learned from GUILD_CREATE events.
    pub fn guilds(&self) -> HashMap<String, String> {
        self.guilds
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The bot's own user id, once READY has been received.
    pub fn self_user_id(&self) -> Option<String> {
        self.self_user_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Cached session id, if the gateway has reached READY.
    pub fn session_id(&self) -> Option<String> {
        self.session
            .lock()
            .unwrap()
            .as_ref()
            .map(|s| s.session_id.clone())
    }

    // ------------------------------------------------------------------
    // REST outbound
    // ------------------------------------------------------------------

    /// POST `/channels/{id}/messages`; returns message id + snowflake ts.
    async fn send_message(&self, msg: &SendMessage) -> Result<SendReceipt, ProviderError> {
        let url = format!("{}/channels/{}/messages", self.rest_base, msg.channel_id);
        let mut payload = serde_json::json!({ "content": msg.text });
        if let Some(reply_to) = &msg.reply_to {
            payload["message_reference"] = serde_json::json!({ "message_id": reply_to });
        }
        if !msg.attachments.is_empty() {
            warn!(
                count = msg.attachments.len(),
                "discord send() is text-only in v0.1; attachments ignored (use the raw REST API)"
            );
        }

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bot {}", self.token))
            .header("User-Agent", user_agent())
            .json(&payload)
            .send()
            .await
            .map_err(|e| ProviderError::Network(e.to_string()))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| ProviderError::Network(e.to_string()))?;

        match status.as_u16() {
            401 | 403 => return Err(ProviderError::Auth(api_error(&text, status))),
            429 => return Err(ProviderError::RateLimit(api_error(&text, status))),
            s if s >= 400 => {
                return Err(ProviderError::Protocol(format!(
                    "HTTP {s}: {}",
                    api_error(&text, status)
                )));
            }
            _ => {}
        }

        let v: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| ProviderError::Protocol(format!("invalid discord response: {e}")))?;
        let id = v["id"]
            .as_str()
            .ok_or_else(|| ProviderError::Protocol("message response missing id".into()))?
            .to_string();
        let ts = snowflake_ts(&id)
            .ok_or_else(|| ProviderError::Protocol("unparseable message id snowflake".into()))?;
        Ok(SendReceipt { message_id: id, ts })
    }

    // ------------------------------------------------------------------
    // Gateway task
    // ------------------------------------------------------------------

    /// Classify a gateway close code: `None` = reconnectable, `Some(reason)` =
    /// fatal (do not reconnect — retrying cannot succeed).
    ///
    /// Reconnectable: 1000/1001 (clean), 1006 (abnormal), 1012 (server
    /// restart), 4000-4003, 4005-4009 (unknown/decode/auth-order/seq/ratelimit/
    /// timeout). Fatal: 4004 (auth failed) and 4010-4014 (invalid shard,
    /// sharding required, invalid API version, invalid intents, disallowed
    /// intents) — all misconfiguration, per Discord gateway docs §Close Event.
    fn classify_close(code: u16) -> Option<&'static str> {
        match code {
            4004 => Some("authentication failed"),
            4010 => Some("invalid shard"),
            4011 => Some("sharding required"),
            4012 => Some("invalid API version"),
            4013 => Some("invalid intents"),
            4014 => Some("disallowed intents"),
            _ => None,
        }
    }

    /// Long-running gateway loop: (re)connect, resume/identify, heartbeat,
    /// dispatch, reconnect with capped backoff until shutdown or fatal error.
    async fn gateway_task(provider: Arc<Self>, mut shutdown: watch::Receiver<bool>) {
        let mut attempt: u32 = 0;
        loop {
            if *shutdown.borrow() {
                break;
            }
            match Self::run_connection(&provider, &mut shutdown).await {
                RunOutcome::Shutdown => break,
                RunOutcome::Fatal(e) => {
                    error!(error = %e, "discord gateway stopped (fatal)");
                    *provider
                        .last_error
                        .lock()
                        .unwrap_or_else(|e| e.into_inner()) = Some(e.clone());
                    // Fatal gateway errors (close 4004/4010-4014, auth) were
                    // invisible to hosts — emit the async error event first.
                    provider.events.on_error(provider.id(), &e);
                    break;
                }
                RunOutcome::Reconnect { healthy } => {
                    if healthy {
                        attempt = 0;
                    } else {
                        attempt += 1;
                        // Persistent degradation signal (like telegram): tell
                        // the host once the gateway has been failing a while.
                        if attempt == TRANSIENT_ERROR_EVENT_THRESHOLD {
                            provider.events.on_error(
                                provider.id(),
                                &ProviderError::Network(format!(
                                    "gateway reconnecting after {attempt} failed attempts"
                                )),
                            );
                        }
                    }
                    let wait = if healthy {
                        Duration::from_millis(250)
                    } else {
                        backoff(attempt)
                    };
                    debug!(wait_ms = wait.as_millis(), "discord gateway reconnecting");
                    tokio::select! {
                        _ = tokio::time::sleep(wait) => {}
                        _ = shutdown.changed() => break,
                    }
                }
            }
        }
    }

    /// One gateway connection: HELLO -> IDENTIFY/RESUME -> heartbeat + dispatch.
    async fn run_connection(
        provider: &Arc<Self>,
        shutdown: &mut watch::Receiver<bool>,
    ) -> RunOutcome {
        let session = provider
            .session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let url = session
            .as_ref()
            .map(|s| s.resume_url.clone())
            .unwrap_or_else(|| provider.gateway_url.clone());
        let ws = match tokio_tungstenite::connect_async(url.as_str()).await {
            Ok((ws, _)) => ws,
            Err(e) => {
                debug!(error = %e, "gateway connect failed");
                return RunOutcome::Reconnect { healthy: false };
            }
        };
        let (mut sink, mut stream) = ws.split();

        // IDENTIFY on first connect, RESUME when we have a cached session.
        if let Some(s) = &session {
            trace!("gateway resuming session {}", s.session_id);
            if let Err(e) = sink
                .send(WsMessage::Text(resume_payload(
                    &provider.token,
                    &s.session_id,
                    s.seq,
                )))
                .await
            {
                debug!(error = %e, "resume send failed");
                return RunOutcome::Reconnect { healthy: false };
            }
        } else if let Err(e) = sink
            .send(WsMessage::Text(identify_payload(
                &provider.token,
                provider.intents,
            )))
            .await
        {
            debug!(error = %e, "identify send failed");
            return RunOutcome::Reconnect { healthy: false };
        }

        // HELLO carries the heartbeat interval.
        let hello = match read_hello(&mut stream).await {
            Some(h) => h,
            None => return RunOutcome::Reconnect { healthy: false },
        };
        debug!(
            interval_ms = hello.heartbeat_interval,
            "gateway HELLO received"
        );
        let mut heartbeat = Heartbeat::new(Duration::from_millis(hello.heartbeat_interval));
        let mut seq: u64 = session.as_ref().map(|s| s.seq).unwrap_or(0);
        let mut beats_since_ack: u32 = 0;
        let mut healthy = false;

        loop {
            tokio::select! {
                biased;
                _ = shutdown.changed() => {
                    let _ = sink.close().await;
                    return RunOutcome::Shutdown;
                }
                _ = heartbeat.tick() => {
                    if beats_since_ack > 3 {
                        warn!(beats_since_ack, "no heartbeat ack; reconnecting");
                        return RunOutcome::Reconnect { healthy };
                    }
                    if let Err(e) = sink.send(WsMessage::Text(heartbeat_payload(seq))).await {
                        debug!(error = %e, "heartbeat send failed");
                        return RunOutcome::Reconnect { healthy };
                    }
                    beats_since_ack += 1;
                }
                frame = stream.next() => {
                    let frame = match frame {
                        Some(Ok(f)) => f,
                        Some(Err(e)) => {
                            debug!(error = %e, "gateway stream error");
                            return RunOutcome::Reconnect { healthy };
                        }
                        None => {
                            debug!("gateway stream closed");
                            return RunOutcome::Reconnect { healthy };
                        }
                    };
                    match frame {
                        WsMessage::Text(text) => {
                            let payload: GatewayPayload = match serde_json::from_str(&text) {
                                Ok(p) => p,
                                Err(e) => {
                                    warn!(error = %e, "unparseable gateway payload");
                                    continue;
                                }
                            };
                            // Track the sequence number for heartbeats + resume.
                            if let Some(s) = payload.s {
                                seq = s;
                                if let Some(sess) = provider.session.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
                                    sess.seq = s;
                                }
                            }
                            match payload.op {
                                OP_DISPATCH => {
                                    let event = payload.t.as_deref().unwrap_or("");
                                    match event {
                                        "READY" => {
                                            if let Some(d) = &payload.d {
                                                if let Ok(ready) = serde_json::from_value::<Ready>(d.clone()) {
                                                    let bot_id = ready.user.as_ref().map(|u| u.id.clone());
                                                    if let Some(id) = &bot_id {
                                                        *provider.self_user_id.lock().unwrap_or_else(|e| e.into_inner()) = Some(id.clone());
                                                    }
                                                    *provider.session.lock().unwrap_or_else(|e| e.into_inner()) = Some(SessionState {
                                                        session_id: ready.session_id.clone(),
                                                        resume_url: ready.resume_gateway_url.clone(),
                                                        seq,
                                                    });
                                                    info!(session_id = %ready.session_id, "discord gateway ready");
                                                    healthy = true;
                                                }
                                            }
                                        }
                                        "GUILD_CREATE" => {
                                            if let Some(d) = &payload.d {
                                                if let (Some(gid), Some(name)) = (d["id"].as_str(), d["name"].as_str()) {
                                                    trace!(guild_id = gid, guild = name, "guild cached");
                                                    provider.guilds.lock().unwrap_or_else(|e| e.into_inner()).insert(gid.to_string(), name.to_string());
                                                }
                                            }
                                        }
                                        "MESSAGE_CREATE" => {
                                            if let Some(d) = &payload.d {
                                                if let Some(msg) = parse_message_create(
                                                    d,
                                                    provider.self_user_id.lock().unwrap_or_else(|e| e.into_inner()).as_deref(),
                                                ) {
                                                    trace!(id = %msg.id, "dispatching discord message");
                                                    provider.events.on_message(msg);
                                                }
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                                OP_HEARTBEAT => {
                                    // Server requested an out-of-band heartbeat.
                                    if let Err(e) = sink.send(WsMessage::Text(heartbeat_payload(seq))).await {
                                        debug!(error = %e, "server heartbeat send failed");
                                        return RunOutcome::Reconnect { healthy };
                                    }
                                }
                                OP_RECONNECT => {
                                    debug!("gateway requested reconnect");
                                    let _ = sink.close().await;
                                    return RunOutcome::Reconnect { healthy: true };
                                }
                                OP_INVALID_SESSION => {
                                    let resumable = payload.d.as_ref().and_then(|v| v.as_bool()).unwrap_or(false);
                                    debug!(resumable, "invalid session");
                                    if !resumable {
                                        provider.session.lock().unwrap_or_else(|e| e.into_inner()).take();
                                    }
                                    let _ = sink.close().await;
                                    return RunOutcome::Reconnect { healthy: true };
                                }
                                OP_HELLO => {
                                    // Post-resume HELLO: refresh the heartbeat interval.
                                    if let Some(d) = &payload.d {
                                        if let Ok(h) = serde_json::from_value::<Hello>(d.clone()) {
                                            heartbeat = Heartbeat::new(Duration::from_millis(h.heartbeat_interval));
                                        }
                                    }
                                }
                                OP_HEARTBEAT_ACK => {
                                    beats_since_ack = 0;
                                    healthy = true;
                                }
                                _ => {}
                            }
                        }
                        WsMessage::Ping(data) => {
                            let _ = sink.send(WsMessage::Pong(data)).await;
                        }
                        WsMessage::Close(Some(frame)) => {
                            let code = u16::from(frame.code);
                            match Self::classify_close(code) {
                                Some(fatal) => {
                                    // 4004 auth failed; 4010 invalid shard;
                                    // 4011 sharding required; 4012 invalid API
                                    // version; 4013 invalid intents; 4014
                                    // disallowed intents — retrying can never
                                    // succeed (misconfiguration), so stop.
                                    return RunOutcome::Fatal(ProviderError::Auth(format!(
                                        "gateway close {code} ({fatal}): {}",
                                        frame.reason
                                    )));
                                }
                                None => {
                                    debug!(code, reason = %frame.reason, "gateway closed");
                                    return RunOutcome::Reconnect { healthy };
                                }
                            }
                        }
                        WsMessage::Close(None) => {
                            debug!("gateway closed (no close frame)");
                            return RunOutcome::Reconnect { healthy };
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

/// Read frames until HELLO (op 10); returns the heartbeat interval.
async fn read_hello<S>(stream: &mut S) -> Option<Hello>
where
    S: Stream<Item = Result<WsMessage, tokio_tungstenite::tungstenite::Error>> + Unpin,
{
    while let Some(frame) = stream.next().await {
        match frame {
            Ok(WsMessage::Text(text)) => {
                if let Ok(payload) = serde_json::from_str::<GatewayPayload>(&text) {
                    if payload.op == OP_HELLO {
                        if let Some(d) = payload.d {
                            if let Ok(h) = serde_json::from_value::<Hello>(d) {
                                return Some(h);
                            }
                        }
                    }
                }
            }
            Ok(WsMessage::Close(_)) | Err(_) => return None,
            _ => {}
        }
    }
    None
}

fn user_agent() -> String {
    format!(
        "DiscordBot (https://github.com/lib-prj/provider-connect, {}; rust)",
        env!("CARGO_PKG_VERSION")
    )
}

fn api_error(text: &str, status: reqwest::StatusCode) -> String {
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(v) => v["message"]
            .as_str()
            .map(|m| format!("HTTP {}: {}", status.as_u16(), m))
            .unwrap_or_else(|| format!("HTTP {}", status.as_u16())),
        Err(_) => format!(
            "HTTP {}: {}",
            status.as_u16(),
            text.chars().take(200).collect::<String>()
        ),
    }
}

fn backoff(attempt: u32) -> Duration {
    let ms = 500u64.saturating_mul(1 << attempt.min(6));
    Duration::from_millis(ms.min(30_000))
}

#[async_trait]
impl ChatProvider for DiscordProvider {
    fn id(&self) -> &'static str {
        "discord"
    }

    async fn start(&mut self) -> Result<(), ProviderError> {
        if self.task.is_some() {
            return Err(ProviderError::Config(
                "discord provider already started".into(),
            ));
        }
        let (tx, rx) = watch::channel(false);
        let provider = Arc::new(Self {
            token: self.token.clone(),
            gateway_url: self.gateway_url.clone(),
            rest_base: self.rest_base.clone(),
            intents: self.intents,
            client: self.client.clone(),
            events: self.events.clone(),
            session: self.session.clone(),
            guilds: self.guilds.clone(),
            self_user_id: self.self_user_id.clone(),
            last_error: self.last_error.clone(),
            task: None,
            shutdown_tx: None,
        });
        let task = tokio::spawn(Self::gateway_task(provider, rx));
        self.task = Some(task);
        self.shutdown_tx = Some(tx);
        debug!("discord provider started");
        Ok(())
    }

    async fn stop(&mut self) -> Result<(), ProviderError> {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(true);
        }
        if let Some(mut task) = self.task.take() {
            let grace = tokio::time::sleep(Duration::from_millis(500));
            tokio::pin!(grace);
            tokio::select! {
                _ = &mut grace => {
                    task.abort();
                    let _ = task.await; // reap
                }
                _ = &mut task => {}
            }
        }
        debug!("discord provider stopped");
        Ok(())
    }

    async fn send(&self, msg: &SendMessage) -> Result<SendReceipt, ProviderError> {
        self.send_message(msg).await
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use provider_core::{ChannelMessage, ProviderEvents, SendMessage};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::mpsc;

    struct TestSink {
        tx: mpsc::Sender<ChannelMessage>,
    }
    impl ProviderEvents for TestSink {
        fn on_message(&self, msg: ChannelMessage) {
            let _ = self.tx.try_send(msg);
        }
    }

    /// Hand-rolled mock Discord REST API: one JSON response per request,
    /// recording `(status_line, headers, body)` on `requests`.
    async fn mock_rest(
        responses: Vec<(u16, &'static str)>,
    ) -> (
        String,
        mpsc::Receiver<(u16, String, String)>,
        JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (req_tx, req_rx) = mpsc::channel(64);
        let task = tokio::spawn(async move {
            let mut idx = 0usize;
            loop {
                let (mut sock, _) = match listener.accept().await {
                    Ok(x) => x,
                    Err(_) => break,
                };
                let mut req = Vec::new();
                let mut buf = [0u8; 8192];
                loop {
                    let n = match sock.read(&mut buf).await {
                        Ok(0) => break,
                        Ok(n) => n,
                        Err(_) => break,
                    };
                    req.extend_from_slice(&buf[..n]);
                    if req.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let mut status: u16 = 500;
                let mut body = String::new();
                if let Some(pos) = req.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&req[..pos]).to_string();
                    status = head
                        .split_whitespace()
                        .nth(1)
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(500);
                    let clen: usize = head
                        .lines()
                        .find_map(|l| {
                            let mut it = l.split(':');
                            if it
                                .next()
                                .map(|k| k.eq_ignore_ascii_case("content-length"))
                                .unwrap_or(false)
                            {
                                it.next().and_then(|v| v.trim().parse().ok())
                            } else {
                                None
                            }
                        })
                        .unwrap_or(0);
                    while req.len() < pos + 4 + clen {
                        let n = sock.read(&mut buf).await.unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        req.extend_from_slice(&buf[..n]);
                    }
                    body = String::from_utf8_lossy(&req[pos + 4..]).to_string();
                }
                let _ = req_tx
                    .send((status, String::from_utf8_lossy(&req).to_string(), body))
                    .await;

                let (rstatus, rbody) = responses[idx.min(responses.len() - 1)];
                idx += 1;
                let reason = match rstatus {
                    200 => "OK",
                    401 => "Unauthorized",
                    403 => "Forbidden",
                    429 => "Too Many Requests",
                    _ => "Error",
                };
                let resp = format!(
                    "HTTP/1.1 {rstatus} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    rbody.len(),
                    rbody
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        (format!("http://{addr}"), req_rx, task)
    }

    #[tokio::test]
    async fn send_posts_to_rest_with_bot_auth() {
        let resp =
            r#"{"id":"1107462566582882336","channel_id":"991234567890123456","content":"hi"}"#;
        let (base, mut reqs, server) = mock_rest(vec![(200, resp)]).await;
        let (tx, _rx) = mpsc::channel(8);
        let provider =
            DiscordProvider::new("BOT_TOKEN", Arc::new(TestSink { tx })).with_rest_base(base);

        let receipt = provider
            .send(&SendMessage {
                channel_id: "991234567890123456".into(),
                text: "hi there".into(),
                reply_to: Some("1107462566582882300".into()),
                attachments: vec![],
            })
            .await
            .unwrap();
        assert_eq!(receipt.message_id, "1107462566582882336");
        assert_eq!(receipt.ts, snowflake_ts("1107462566582882336").unwrap());

        let (_status, head, body) = reqs.recv().await.expect("request recorded");
        let lower = head.to_lowercase();
        assert!(
            lower.contains("authorization: bot bot_token"),
            "missing bot auth header: {head}"
        );
        assert!(
            lower.contains("user-agent: discordbot"),
            "missing UA: {head}"
        );
        assert!(head.contains("POST /channels/991234567890123456/messages"));
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["content"], "hi there");
        assert_eq!(v["message_reference"]["message_id"], "1107462566582882300");
        server.abort();
    }

    #[tokio::test]
    async fn send_maps_403_to_auth_error() {
        let body = r#"{"message": "Missing Permissions", "code": 50013}"#;
        let (base, _reqs, server) = mock_rest(vec![(403, body)]).await;
        let (tx, _rx) = mpsc::channel(8);
        let provider =
            DiscordProvider::new("BOT_TOKEN", Arc::new(TestSink { tx })).with_rest_base(base);
        let err = provider
            .send(&SendMessage {
                channel_id: "1".into(),
                text: "x".into(),
                reply_to: None,
                attachments: vec![],
            })
            .await
            .unwrap_err();
        assert!(matches!(err, ProviderError::Auth(_)), "got {err:?}");
        server.abort();
    }

    #[tokio::test]
    async fn send_maps_429_to_rate_limit() {
        let body = r#"{"message": "You are being rate limited.", "retry_after": 1.2}"#;
        let (base, _reqs, server) = mock_rest(vec![(429, body)]).await;
        let (tx, _rx) = mpsc::channel(8);
        let provider =
            DiscordProvider::new("BOT_TOKEN", Arc::new(TestSink { tx })).with_rest_base(base);
        let err = provider
            .send(&SendMessage {
                channel_id: "1".into(),
                text: "x".into(),
                reply_to: None,
                attachments: vec![],
            })
            .await
            .unwrap_err();
        assert!(matches!(err, ProviderError::RateLimit(_)), "got {err:?}");
        server.abort();
    }

    #[tokio::test]
    async fn stop_terminates_reconnect_loop() {
        // Point the gateway at a dead local port: connect fails, the task
        // enters the backoff/reconnect loop. stop() must end it promptly.
        let (tx, _rx) = mpsc::channel(8);
        let mut provider = DiscordProvider::new("TOK", Arc::new(TestSink { tx }))
            .with_gateway_url("ws://127.0.0.1:1/?v=10&encoding=json");
        provider.start().await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await; // let it fail + back off
        let t0 = tokio::time::Instant::now();
        provider.stop().await.unwrap();
        assert!(t0.elapsed() < Duration::from_secs(3));
    }

    #[tokio::test]
    async fn double_start_returns_config_error() {
        let (tx, _rx) = mpsc::channel(8);
        let mut provider = DiscordProvider::new("TOK", Arc::new(TestSink { tx }))
            .with_gateway_url("ws://127.0.0.1:1/?v=10&encoding=json");
        provider.start().await.unwrap();
        let err = provider.start().await.unwrap_err();
        assert!(matches!(err, ProviderError::Config(_)), "got {err:?}");
        provider.stop().await.unwrap();
    }
}
