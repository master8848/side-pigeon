//! Telegram provider for `provider-connect`.
//!
//! Hand-rolled [Telegram Bot API](https://core.telegram.org/bots/api) client on
//! `reqwest` (no `teloxide`/`telegram-bot` SDK), following the ZeroClaw pattern.
//!
//! * Inbound: `getUpdates` long-polling with an offset cursor (default 30 s long
//!   poll, 1 s idle poll interval). Each `update.message` is normalized into a
//!   [`ChannelMessage`](provider_core::ChannelMessage) and handed to the
//!   [`ProviderEvents`](provider_core::ProviderEvents) sink.
//! * Outbound: `sendMessage` -> [`SendReceipt`](provider_core::SendReceipt).
//! * Errors are mapped onto [`ProviderError`](provider_core::ProviderError)
//!   variants: HTTP 401 -> `Auth`, 409 -> `Protocol` (conflicting long-poll),
//!   429 -> `RateLimit` (honoring `retry_after`), network failures -> `Network`.
//!
//! ## Example
//!
//! ```no_run
//! use std::sync::Arc;
//! use provider_core::{ChatProvider, ProviderEvents, ChannelMessage};
//! use provider_telegram::TelegramProvider;
//!
//! struct Sink;
//! impl ProviderEvents for Sink {
//!     fn on_message(&self, _msg: ChannelMessage) { /* forward to transport */ }
//! }
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let mut provider = TelegramProvider::new("YOUR_BOT_TOKEN".to_string(), Arc::new(Sink));
//!     provider.start().await?;
//!     // ... run agent loop ...
//!     provider.stop().await?;
//!     Ok(())
//! }
//! ```

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use provider_core::{
    ChannelMessage, ChatProvider, ContentPart, MediaAttachment, MediaKind, ProviderError,
    ProviderEvents, SendMessage, SendReceipt, Sender,
};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::{debug, error, trace, warn};

// ---------------------------------------------------------------------------
// Telegram wire types (serde, snake_case) — the subset we need.
// ---------------------------------------------------------------------------

#[derive(Debug, serde::Deserialize)]
struct Update {
    update_id: i64,
    #[serde(default)]
    message: Option<Message>,
}

#[derive(Debug, serde::Deserialize)]
struct Message {
    message_id: i64,
    date: i64,
    chat: Chat,
    #[serde(default)]
    from: Option<User>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    caption: Option<String>,
    #[serde(default)]
    reply_to_message: Option<Box<Message>>,
    #[serde(default)]
    photo: Option<Vec<PhotoSize>>,
    #[serde(default)]
    document: Option<Document>,
    #[serde(default)]
    voice: Option<Voice>,
    #[serde(default)]
    audio: Option<Audio>,
    #[serde(default)]
    video: Option<Video>,
    #[serde(default)]
    sticker: Option<Sticker>,
}

#[derive(Debug, serde::Deserialize)]
struct Chat {
    id: i64,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    username: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct User {
    id: i64,
    #[serde(default)]
    first_name: Option<String>,
    #[serde(default)]
    username: Option<String>,
}

/// Telegram wire shapes. Fields mirror the Bot API schema; some (e.g.
/// `file_id`) are retained for future `getFile`-based media fetch even though
/// v0.1 does not read them — the full raw JSON is preserved on `raw` anyway.
#[allow(dead_code)]
#[derive(Debug, serde::Deserialize)]
struct PhotoSize {
    file_id: String,
    width: i64,
    height: i64,
}

#[allow(dead_code)]
#[derive(Debug, serde::Deserialize)]
struct Document {
    file_id: String,
    #[serde(default)]
    file_name: Option<String>,
    #[serde(default)]
    mime_type: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, serde::Deserialize)]
struct Voice {
    file_id: String,
    #[serde(default)]
    mime_type: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, serde::Deserialize)]
struct Audio {
    file_id: String,
    #[serde(default)]
    mime_type: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, serde::Deserialize)]
struct Video {
    file_id: String,
    #[serde(default)]
    mime_type: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, serde::Deserialize)]
struct Sticker {
    file_id: String,
}

fn media(kind: MediaKind, caption: Option<String>) -> MediaAttachment {
    MediaAttachment {
        kind,
        url: None, // URLs require a getFile round-trip; file_ids stay in `raw`
        mime: None,
        data: None,
        caption,
    }
}

/// Normalize one raw Telegram `update` JSON object into a [`ChannelMessage`].
///
/// Only `update.message` is mapped (per the provider contract). Messages with
/// neither text/caption nor media are skipped (Telegram service messages such
/// as member-join/pin notifications carry no agent-relevant content).
///
/// * `id` = `"<update_id>/<message_id>"` (both halves are Telegram ids)
/// * `ts` = `message.date` (unix seconds) x 1000 -> epoch millis
/// * `reply_target` = `reply_to_message.message_id` when present
/// * `raw` = the full update JSON, so the transport can access fields we do not
///   model (file_ids, entities, forward origin, …).
pub fn message_from_update(value: &serde_json::Value) -> Option<ChannelMessage> {
    let update: Update = serde_json::from_value(value.clone()).ok()?;
    let message = update.message.as_ref()?;

    let mut content: Vec<ContentPart> = Vec::new();
    let mut attachments: Vec<MediaAttachment> = Vec::new();

    if let Some(text) = message.text.as_deref().filter(|t| !t.is_empty()) {
        content.push(ContentPart::Text(text.to_string()));
    }
    if let Some(caption) = message.caption.as_deref().filter(|c| !c.is_empty()) {
        // Surface the caption as text when the message has no body text.
        if content.is_empty() {
            content.push(ContentPart::Text(caption.to_string()));
        }
    }

    if message.photo.is_some() {
        attachments.push(media(MediaKind::Image, message.caption.clone()));
    }
    if let Some(doc) = &message.document {
        let mut att = media(MediaKind::File, message.caption.clone());
        att.mime = doc.mime_type.clone();
        attachments.push(att);
    }
    if let Some(voice) = &message.voice {
        let mut att = media(MediaKind::Audio, message.caption.clone());
        att.mime = voice.mime_type.clone();
        attachments.push(att);
    }
    if let Some(audio) = &message.audio {
        let mut att = media(MediaKind::Audio, message.caption.clone());
        att.mime = audio.mime_type.clone();
        attachments.push(att);
    }
    if let Some(video) = &message.video {
        let mut att = media(MediaKind::Video, message.caption.clone());
        att.mime = video.mime_type.clone();
        attachments.push(att);
    }
    if message.sticker.is_some() {
        attachments.push(media(MediaKind::Sticker, None));
    }

    if content.is_empty() && attachments.is_empty() {
        return None; // service message / no agent-relevant content
    }

    let sender = message.from.as_ref().map(|from| Sender {
        id: from.id.to_string(),
        name: from.first_name.clone(),
        username: from.username.clone(),
        avatar_url: None,
    });

    Some(ChannelMessage {
        id: format!("{}/{}", update.update_id, message.message_id),
        channel: "telegram".to_string(),
        channel_id: message.chat.id.to_string(),
        sender: sender.unwrap_or_else(|| Sender {
            id: message.chat.id.to_string(),
            name: message.chat.title.clone(),
            username: message.chat.username.clone(),
            avatar_url: None,
        }),
        reply_target: message
            .reply_to_message
            .as_ref()
            .map(|r| r.message_id.to_string()),
        content,
        thread_ts: None, // Telegram has no thread anchor in this contract
        attachments,
        explicitly_addressed: false, // no @mention handling for Telegram v0.1
        ts: message.date * 1000,
        raw: Some(value.clone()),
    })
}

// ---------------------------------------------------------------------------
// Polling failure classification
// ---------------------------------------------------------------------------

/// How a `getUpdates` round failed.
enum PollFailure {
    /// Transient — retry, optionally after `after`.
    Retry {
        error: ProviderError,
        after: Option<Duration>,
    },
    /// Permanent — stop polling (auth, conflicting long-poll).
    Fatal(ProviderError),
}

/// Extract `{error_code, description}` from a Telegram error body.
fn error_body(text: &str) -> (i64, String) {
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(v) => {
            let code = v["error_code"].as_i64().unwrap_or(0);
            let desc = v["description"]
                .as_str()
                .unwrap_or("unknown telegram api error")
                .to_string();
            (code, desc)
        }
        Err(_) => (0, text.chars().take(200).collect()),
    }
}

/// `parameters.retry_after` (seconds, may be fractional) -> [`Duration`], capped.
fn retry_after(text: &str) -> Option<Duration> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let secs = v["parameters"]["retry_after"].as_f64()?;
    Some(Duration::from_secs_f64(secs.min(60.0)))
}

fn backoff(base: Duration, consecutive_errors: u32) -> Duration {
    let mult = 2u32.saturating_pow(consecutive_errors.min(6));
    base.saturating_mul(mult).min(Duration::from_secs(30))
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

/// Telegram provider: `getUpdates` long-poll inbound, `sendMessage` outbound.
///
/// `start()` spawns a background polling task; inbound messages are delivered
/// through the [`ProviderEvents`] sink supplied at construction. `stop()`
/// cancels polling. The provider may be restarted after `stop()`; the update
/// offset cursor persists in-process across restarts.
pub struct TelegramProvider {
    token: String,
    base_url: String,
    poll_interval: Duration,
    long_poll_timeout_secs: u64,
    client: reqwest::Client,
    events: Arc<dyn ProviderEvents>,

    // runtime state
    offset: Arc<AtomicI64>,
    last_error: Arc<Mutex<Option<ProviderError>>>,
    task: Option<JoinHandle<()>>,
    shutdown_tx: Option<watch::Sender<bool>>,
}

impl std::fmt::Debug for TelegramProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TelegramProvider")
            .field("base_url", &self.base_url)
            .field("poll_interval", &self.poll_interval)
            .field("long_poll_timeout_secs", &self.long_poll_timeout_secs)
            .field("started", &self.task.is_some())
            .finish_non_exhaustive()
    }
}

impl TelegramProvider {
    /// Create a provider for `token` that delivers inbound messages to `events`.
    ///
    /// Defaults: base URL `https://api.telegram.org`, 1 s poll interval, 30 s
    /// long-poll timeout, 60 s HTTP request timeout.
    pub fn new(token: impl Into<String>, events: Arc<dyn ProviderEvents>) -> Self {
        Self {
            token: token.into(),
            base_url: "https://api.telegram.org".to_string(),
            poll_interval: Duration::from_secs(1),
            long_poll_timeout_secs: 30,
            client: build_client(Duration::from_secs(60)),
            events,
            offset: Arc::new(AtomicI64::new(0)),
            last_error: Arc::new(Mutex::new(None)),
            task: None,
            shutdown_tx: None,
        }
    }

    /// Override the API base URL (used by tests and self-hosted bot API servers).
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').to_string();
        self
    }

    /// Set the idle delay between `getUpdates` rounds (default 1 s).
    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    /// Set the `getUpdates` long-poll `timeout` parameter (default 30 s).
    pub fn with_long_poll_timeout_secs(mut self, secs: u64) -> Self {
        self.long_poll_timeout_secs = secs;
        self
    }

    /// Set the per-request HTTP timeout (default 60 s).
    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.client = build_client(timeout);
        self
    }

    /// Take the last polling error, if any (e.g. `RateLimit`/`Auth` after a
    /// fatal stop). Consumes it so callers can match on the variant without
    /// requiring `ProviderError: Clone`.
    pub fn take_last_error(&self) -> Option<ProviderError> {
        self.last_error.lock().unwrap().take()
    }

    /// Current `getUpdates` offset cursor (last confirmed update_id + 1).
    pub fn offset(&self) -> i64 {
        self.offset.load(Ordering::SeqCst)
    }

    fn method_url(&self, method: &str) -> String {
        format!("{}/bot{}/{}", self.base_url, self.token, method)
    }

    /// One `getUpdates` round. Returns raw update objects (full JSON retained).
    async fn get_updates(&self, offset: i64) -> Result<Vec<serde_json::Value>, PollFailure> {
        let url = self.method_url("getUpdates");
        let payload = serde_json::json!({
            "offset": offset,
            "timeout": self.long_poll_timeout_secs,
            "allowed_updates": ["message"],
        });
        let resp = self
            .client
            .post(&url)
            .json(&payload)
            .send()
            .await
            .map_err(|e| PollFailure::Retry {
                error: ProviderError::Network(e.to_string()),
                after: None,
            })?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| PollFailure::Retry {
                error: ProviderError::Network(e.to_string()),
                after: None,
            })?;

        // HTTP-level error mapping: 401 auth, 409 conflicting long-poll, 429 rate limit.
        match status.as_u16() {
            401 => return Err(PollFailure::Fatal(ProviderError::Auth(error_body(&text).1))),
            409 => return Err(PollFailure::Fatal(ProviderError::Protocol(error_body(&text).1))),
            429 => {
                return Err(PollFailure::Retry {
                    error: ProviderError::RateLimit(error_body(&text).1),
                    after: retry_after(&text),
                });
            }
            s if s >= 400 => {
                return Err(PollFailure::Retry {
                    error: ProviderError::Protocol(format!("HTTP {s}: {}", error_body(&text).1)),
                    after: None,
                });
            }
            _ => {}
        }

        let body: serde_json::Value = serde_json::from_str(&text).map_err(|e| PollFailure::Retry {
            error: ProviderError::Protocol(format!("invalid telegram response: {e}")),
            after: None,
        })?;
        if body.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            let (code, desc) = error_body(&text);
            return match code {
                401 => Err(PollFailure::Fatal(ProviderError::Auth(desc))),
                409 => Err(PollFailure::Fatal(ProviderError::Protocol(desc))),
                429 => Err(PollFailure::Retry {
                    error: ProviderError::RateLimit(desc),
                    after: retry_after(&text),
                }),
                _ => Err(PollFailure::Retry {
                    error: ProviderError::Protocol(desc),
                    after: None,
                }),
            };
        }
        let updates = body["result"].as_array().cloned().unwrap_or_default();
        trace!(count = updates.len(), "getUpdates returned");
        Ok(updates)
    }

    /// Long-running poll loop: `getUpdates` -> dispatch -> advance cursor.
    async fn poll_loop(provider: Arc<Self>, shutdown: watch::Receiver<bool>) {
        let mut offset = provider.offset.load(Ordering::SeqCst);
        let mut consecutive_errors: u32 = 0;
        loop {
            if *shutdown.borrow() {
                debug!("telegram poll loop shutting down");
                break;
            }
            match provider.get_updates(offset).await {
                Ok(updates) => {
                    consecutive_errors = 0;
                    for update in &updates {
                        let update_id = update["update_id"].as_i64();
                        if let Some(msg) = message_from_update(update) {
                            // Advance the cursor *after* dispatch so a crash between
                            // delivery and ack re-delivers rather than drops.
                            offset = update_id.map(|u| u + 1).unwrap_or(offset + 1);
                            provider.offset.store(offset, Ordering::SeqCst);
                            trace!(update_id, message_id = %msg.id, "dispatching telegram message");
                            provider.events.on_message(msg);
                        } else if let Some(uid) = update_id {
                            // Non-message update (allowed_updates restricts to message,
                            // but never wedge the cursor): advance past it.
                            offset = uid + 1;
                            provider.offset.store(offset, Ordering::SeqCst);
                        }
                    }
                    tokio::time::sleep(provider.poll_interval).await;
                }
                Err(PollFailure::Retry { error, after }) => {
                    *provider.last_error.lock().unwrap() = Some(error);
                    consecutive_errors += 1;
                    let wait =
                        after.unwrap_or_else(|| backoff(provider.poll_interval, consecutive_errors));
                    debug!(wait_ms = wait.as_millis(), "telegram poll transient error; retrying");
                    tokio::time::sleep(wait).await;
                }
                Err(PollFailure::Fatal(error)) => {
                    error!(error = %error, "telegram polling stopped (fatal error)");
                    *provider.last_error.lock().unwrap() = Some(error);
                    break;
                }
            }
        }
    }

    /// POST `sendMessage`; returns the Telegram message_id + unix-ms timestamp.
    async fn send_message(&self, msg: &SendMessage) -> Result<SendReceipt, ProviderError> {
        let url = self.method_url("sendMessage");
        let mut payload = serde_json::json!({
            "chat_id": msg.channel_id,
            "text": msg.text,
        });
        if let Some(reply_to) = &msg.reply_to {
            let id: i64 = reply_to.parse().map_err(|_| {
                ProviderError::Config(format!(
                    "telegram reply_to must be a numeric message_id, got {reply_to:?}"
                ))
            })?;
            payload["reply_to_message_id"] = serde_json::json!(id);
        }
        if !msg.attachments.is_empty() {
            warn!(
                count = msg.attachments.len(),
                "telegram send() uses sendMessage (text only); attachments ignored (use the raw API)"
            );
        }

        let resp = self
            .client
            .post(&url)
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
            401 => return Err(ProviderError::Auth(error_body(&text).1)),
            429 => {
                return Err(ProviderError::RateLimit(format!(
                    "sendMessage: {}",
                    error_body(&text).1
                )));
            }
            400 => {
                return Err(ProviderError::Protocol(format!(
                    "sendMessage rejected: {}",
                    error_body(&text).1
                )));
            }
            s if s >= 400 => {
                return Err(ProviderError::Protocol(format!("HTTP {s}: {}", error_body(&text).1)));
            }
            _ => {}
        }

        let body: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| ProviderError::Protocol(format!("invalid telegram response: {e}")))?;
        if body.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            return Err(ProviderError::Protocol(error_body(&text).1));
        }
        let result = &body["result"];
        let message_id = result["message_id"]
            .as_i64()
            .ok_or_else(|| ProviderError::Protocol("sendMessage result missing message_id".into()))?
            .to_string();
        let date = result["date"]
            .as_i64()
            .ok_or_else(|| ProviderError::Protocol("sendMessage result missing date".into()))?;
        Ok(SendReceipt {
            message_id,
            ts: date * 1000,
        })
    }
}

fn build_client(timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .expect("valid reqwest client")
}

impl ChatProvider for TelegramProvider {
    fn id(&self) -> &'static str {
        "telegram"
    }

    async fn start(&mut self) -> Result<(), ProviderError> {
        if self.task.is_some() {
            return Err(ProviderError::Config(
                "telegram provider already started".into(),
            ));
        }
        let (tx, rx) = watch::channel(false);
        let provider = Arc::new(Self {
            token: self.token.clone(),
            base_url: self.base_url.clone(),
            poll_interval: self.poll_interval,
            long_poll_timeout_secs: self.long_poll_timeout_secs,
            client: self.client.clone(),
            events: self.events.clone(),
            offset: self.offset.clone(),
            last_error: self.last_error.clone(),
            task: None,
            shutdown_tx: None,
        });
        let task = tokio::spawn(Self::poll_loop(provider, rx));
        self.task = Some(task);
        self.shutdown_tx = Some(tx);
        debug!("telegram provider started");
        Ok(())
    }

    async fn stop(&mut self) -> Result<(), ProviderError> {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(true);
        }
        if let Some(mut task) = self.task.take() {
            // Grace window: the poll loop checks the shutdown flag between
            // rounds; an in-flight 30 s long-poll is aborted after the window.
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
        debug!("telegram provider stopped");
        Ok(())
    }

    async fn send(&self, msg: &SendMessage) -> Result<SendReceipt, ProviderError> {
        self.send_message(msg).await
    }
}

// ---------------------------------------------------------------------------
// Tests — hand-rolled mock Telegram API on a local tokio TcpListener.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use provider_core::{ProviderEvents, SendMessage};
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

    /// Hand-rolled mock Telegram API server: serves `responses` (status, body)
    /// in order (last one repeats), recording each request body on `requests`.
    async fn mock_api(
        responses: Vec<(u16, &'static str)>,
    ) -> (String, mpsc::Receiver<String>, JoinHandle<()>) {
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
                // Read headers (up to CRLFCRLF).
                let mut req = Vec::new();
                let mut buf = [0u8; 4096];
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
                // Read the body per Content-Length.
                if let Some(pos) = req.windows(4).position(|w| w == b"\r\n\r\n") {
                    let head = String::from_utf8_lossy(&req[..pos]).to_string();
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
                    let body = String::from_utf8_lossy(&req[pos + 4..]).to_string();
                    let _ = req_tx.send(body).await;
                }
                let (status, body) = responses[idx.min(responses.len() - 1)];
                idx += 1;
                let reason = match status {
                    200 => "OK",
                    401 => "Unauthorized",
                    409 => "Conflict",
                    429 => "Too Many Requests",
                    _ => "Error",
                };
                let resp = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });
        (format!("http://{addr}"), req_rx, task)
    }

    fn provider_with(base: String) -> (mpsc::Sender<ChannelMessage>, mpsc::Receiver<ChannelMessage>, TelegramProvider) {
        let (tx, rx) = mpsc::channel(8);
        let provider = TelegramProvider::new("TESTTOKEN", Arc::new(TestSink { tx: tx.clone() }))
            .with_base_url(base)
            .with_poll_interval(Duration::from_millis(50))
            .with_long_poll_timeout_secs(1);
        (tx, rx, provider)
    }

    #[tokio::test]
    async fn get_updates_maps_to_channel_message() {
        let body = r#"{"ok":true,"result":[{"update_id":100,"message":{"message_id":42,"date":1700000000,"chat":{"id":-100123,"type":"supergroup","title":"Test Group"},"from":{"id":7,"first_name":"Ada","username":"ada_l"},"text":"hello world","reply_to_message":{"message_id":41,"date":1699999999,"chat":{"id":-100123,"type":"supergroup"},"text":"orig"}}}]}"#;
        let (base, _reqs, server) = mock_api(vec![(200, body)]).await;
        let (_tx, mut rx, mut provider) = provider_with(base);
        provider.start().await.unwrap();

        let msg = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out waiting for message")
            .expect("channel closed");
        assert_eq!(msg.id, "100/42");
        assert_eq!(msg.channel, "telegram");
        assert_eq!(msg.channel_id, "-100123");
        assert_eq!(msg.sender.id, "7");
        assert_eq!(msg.sender.name.as_deref(), Some("Ada"));
        assert_eq!(msg.sender.username.as_deref(), Some("ada_l"));
        assert_eq!(msg.content.len(), 1);
        assert!(matches!(&msg.content[0], ContentPart::Text(t) if t == "hello world"));
        assert_eq!(msg.reply_target.as_deref(), Some("41"));
        assert_eq!(msg.thread_ts, None);
        assert_eq!(msg.ts, 1700000000 * 1000);
        assert!(msg.raw.is_some());
        assert_eq!(provider.offset(), 101);

        provider.stop().await.unwrap();
        server.abort();
    }

    #[tokio::test]
    async fn photo_message_maps_attachments() {
        let body = r#"{"ok":true,"result":[{"update_id":2,"message":{"message_id":9,"date":1700000001,"chat":{"id":5,"type":"private","username":"ada_l"},"from":{"id":7,"first_name":"Ada"},"caption":"check this","photo":[{"file_id":"AA1","width":100,"height":50},{"file_id":"AA2","width":800,"height":400}]}}]}"#;
        let (base, _reqs, server) = mock_api(vec![(200, body)]).await;
        let (_tx, mut rx, mut provider) = provider_with(base);
        provider.start().await.unwrap();

        let msg = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("timed out")
            .expect("channel closed");
        assert!(matches!(&msg.content[0], ContentPart::Text(t) if t == "check this"));
        assert_eq!(msg.attachments.len(), 1);
        assert!(matches!(msg.attachments[0].kind, MediaKind::Image));
        assert_eq!(msg.attachments[0].caption.as_deref(), Some("check this"));
        assert_eq!(msg.channel_id, "5");
        assert_eq!(msg.sender.name.as_deref(), Some("Ada"));

        provider.stop().await.unwrap();
        server.abort();
    }

    #[tokio::test]
    async fn service_message_is_skipped_but_cursor_advances() {
        // new_chat_members: no text/caption/media -> skipped, cursor still moves.
        let body = r#"{"ok":true,"result":[{"update_id":3,"message":{"message_id":1,"date":1700000002,"chat":{"id":5,"type":"private"},"new_chat_members":[{"id":7,"first_name":"Ada"}]}}]}"#;
        let (base, _reqs, server) = mock_api(vec![(200, body)]).await;
        let (_tx, mut rx, mut provider) = provider_with(base);
        provider.start().await.unwrap();

        assert!(tokio::time::timeout(Duration::from_millis(400), rx.recv())
            .await
            .is_err());
        assert_eq!(provider.offset(), 4);
        provider.stop().await.unwrap();
        server.abort();
    }

    #[tokio::test]
    async fn send_posts_sendmessage() {
        let resp = r#"{"ok":true,"result":{"message_id":99,"date":1700000100,"text":"hi"}}"#;
        let (base, mut reqs, server) = mock_api(vec![(200, resp)]).await;
        let (_tx, _rx, provider) = provider_with(base);

        let receipt = provider
            .send(&SendMessage {
                channel_id: "-100123".into(),
                text: "hi there".into(),
                reply_to: Some("41".into()),
                attachments: vec![],
            })
            .await
            .unwrap();
        assert_eq!(receipt.message_id, "99");
        assert_eq!(receipt.ts, 1700000100 * 1000);

        let req = reqs.recv().await.expect("request recorded");
        let v: serde_json::Value = serde_json::from_str(&req).unwrap();
        assert_eq!(v["chat_id"], "-100123");
        assert_eq!(v["text"], "hi there");
        assert_eq!(v["reply_to_message_id"], 41);
        server.abort();
    }

    #[tokio::test]
    async fn send_invalid_reply_to_is_config_error() {
        let (_tx, _rx, provider) = provider_with("http://127.0.0.1:1".into());
        let err = provider
            .send(&SendMessage {
                channel_id: "1".into(),
                text: "x".into(),
                reply_to: Some("not-a-number".into()),
                attachments: vec![],
            })
            .await
            .unwrap_err();
        assert!(matches!(err, ProviderError::Config(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn rate_limit_429_maps_to_rate_limited() {
        let body = r#"{"ok":false,"error_code":429,"description":"Too Many Requests: retry after 1","parameters":{"retry_after":0.05}}"#;
        let (base, _reqs, server) = mock_api(vec![(429, body)]).await;
        let (_tx, _rx, mut provider) = provider_with(base);
        provider.start().await.unwrap();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(err) = provider.take_last_error() {
                assert!(matches!(err, ProviderError::RateLimit(_)), "got {err:?}");
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for rate limit error"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        provider.stop().await.unwrap();
        server.abort();
    }

    #[tokio::test]
    async fn auth_401_stops_polling() {
        let body = r#"{"ok":false,"error_code":401,"description":"Unauthorized"}"#;
        let (base, _reqs, server) = mock_api(vec![(401, body)]).await;
        let (_tx, _rx, mut provider) = provider_with(base);
        provider.start().await.unwrap();

        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if provider.task.as_ref().unwrap().is_finished() {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "poll task did not stop after fatal auth error"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(matches!(
            provider.take_last_error(),
            Some(ProviderError::Auth(_))
        ));
        provider.stop().await.unwrap();
        server.abort();
    }

    #[tokio::test]
    async fn double_start_returns_config_error() {
        let body = r#"{"ok":true,"result":[]}"#;
        let (base, _reqs, server) = mock_api(vec![(200, body)]).await;
        let (_tx, _rx, mut provider) = provider_with(base);
        provider.start().await.unwrap();
        let err = provider.start().await.unwrap_err();
        assert!(matches!(err, ProviderError::Config(_)), "got {err:?}");
        provider.stop().await.unwrap();
        server.abort();
    }

    #[tokio::test]
    async fn stop_cancels_inflight_polling() {
        // Server accepts but never responds: the poll task sits in a 30 s
        // long-poll. stop() must still return promptly and end the task.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf).await; // read request, then hang
            tokio::time::sleep(Duration::from_secs(60)).await;
        });
        let (_tx, _rx, mut provider) = provider_with(format!("http://{addr}"));
        provider.start().await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await; // ensure in-flight
        let t0 = tokio::time::Instant::now();
        provider.stop().await.unwrap();
        assert!(
            t0.elapsed() < Duration::from_secs(5),
            "stop() took too long to cancel an in-flight poll"
        );
        server.abort();
    }
}
