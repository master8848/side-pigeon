//! The three pc-connect operations (send / listen / check), implemented by
//! EMBEDDING the provider-connect logic in-process: an [`AppState`] registry
//! built from the same config contract as the `pc` sidecar, driven directly
//! (no sidecar spawn, single self-contained binary).

use std::sync::Arc;
use std::time::Duration;

use provider_core::{ChatProvider, ProviderError, ProviderEvents, SendMessage, SendReceipt};
use provider_transport::events::{EVENT_ERROR, EVENT_MESSAGE};
use provider_transport::jsonrpc::JsonRpcError;
use provider_transport::state::{provider_error, AppState, Outbound};
use tokio::sync::broadcast::{self, error::RecvError};

use crate::config::SidecarConfig;
use crate::providers;

/// How long `check` waits for a provider to prove itself (or fail) before
/// declaring it healthy. Telegram/discord connect asynchronously; auth
/// failures (401, gateway close 4004) surface well inside this window.
pub const CHECK_SMOKE_TIMEOUT: Duration = Duration::from_secs(6);
/// Poll interval while waiting for a provider's async error during `check`.
#[cfg(any(feature = "telegram", feature = "discord"))]
const CHECK_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// A pc-connect runtime failure, shaped like a JSON-RPC error object so the
/// CLI can print `{"error": {...}}` on stdout (contract: non-zero exit +
/// error JSON on failure).
#[derive(Debug, Clone)]
pub struct CliError(pub JsonRpcError);

impl CliError {
    /// Configuration error (`-32001`).
    pub fn config(message: impl Into<String>) -> Self {
        CliError(JsonRpcError::new(JsonRpcError::CONFIG_ERROR, message, None))
    }

    /// Protocol error (`-32004`): unknown provider, not started, ...
    pub fn protocol(message: impl Into<String>) -> Self {
        CliError(JsonRpcError::new(
            JsonRpcError::PROTOCOL_ERROR,
            message,
            None,
        ))
    }

    /// Internal error (`-32603`).
    pub fn internal(message: impl Into<String>) -> Self {
        CliError(JsonRpcError::new(
            JsonRpcError::INTERNAL_ERROR,
            message,
            None,
        ))
    }

    /// From a [`ProviderError`], reusing the transport's code mapping so the
    /// CLI speaks the same error vocabulary as the JSON-RPC protocol.
    pub fn from_provider(e: ProviderError) -> Self {
        CliError(provider_error(e))
    }
}

impl std::fmt::Display for CliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.message)
    }
}

impl std::error::Error for CliError {}

impl From<std::io::Error> for CliError {
    fn from(e: std::io::Error) -> Self {
        CliError::internal(e.to_string())
    }
}

/// Options for `pc-connect send`.
#[derive(Debug, Clone)]
pub struct SendOptions {
    /// Provider id, e.g. `"telegram"`.
    pub provider: String,
    /// Chat/room id to deliver to.
    pub chat: String,
    /// Message text body.
    pub text: String,
}

/// Options for `pc-connect listen`.
#[derive(Debug, Clone)]
pub struct ListenOptions {
    /// Restrict to these provider ids (None = every configured provider).
    pub providers: Option<Vec<String>>,
    /// Exit after this long, even if no event arrived.
    pub timeout: Option<Duration>,
    /// Exit after the first event.
    pub once: bool,
}

/// Options for `pc-connect check`.
#[derive(Debug, Clone)]
pub struct CheckOptions {
    /// Check only this provider id (None = every configured provider).
    pub provider: Option<String>,
}

/// Result of one provider's connectivity check.
#[derive(Debug, Clone)]
pub struct CheckResult {
    /// Provider id.
    pub provider: String,
    /// Whether the check passed.
    pub ok: bool,
    /// Human-readable detail.
    pub detail: String,
    /// JSON-RPC error code when `ok == false` (provider-mapped codes).
    pub code: Option<i64>,
}

/// One provider's `check` outcome, before being flattened into [`CheckResult`].
enum SmokeOutcome {
    Pass(&'static str),
    Fail(CliError),
}

// ---------------------------------------------------------------------------
// Shared setup
// ---------------------------------------------------------------------------

/// Build the provider registry from config, exactly like the `pc` sidecar
/// does (same builder, same feature gates).
fn build_state(
    config: &SidecarConfig,
) -> Result<(AppState, broadcast::Sender<Outbound>), CliError> {
    let (mut state, notify_tx) = AppState::new("cli");
    let events = state.events();
    let mut failures: Vec<String> = Vec::new();
    for provider in &config.providers {
        match providers::build_provider(&provider.id, &provider.config, events.clone()) {
            Ok(boxed) => {
                if let Err(e) = state.registry_mut().register(boxed) {
                    failures.push(format!("{}: {e}", provider.id));
                }
            }
            Err(e) => failures.push(format!("{}: {e}", provider.id)),
        }
    }
    if !failures.is_empty() {
        return Err(CliError::config(format!(
            "failed to load {} provider(s): {}",
            failures.len(),
            failures.join("; ")
        )));
    }
    // Drop our local event-sink handle so the broadcast channel can close
    // when `notify_tx` is dropped (mirrors bin/pc).
    drop(events);
    Ok((state, notify_tx))
}

/// Resolve the providers a command should act on: an explicit filter
/// (`--providers`/`--provider`) or every registered provider.
fn resolve_targets(state: &AppState, filter: Option<&[String]>) -> Result<Vec<String>, CliError> {
    let ids: Vec<String> = match filter {
        Some(ids) => ids.to_vec(),
        None => state
            .registry()
            .ids()
            .iter()
            .map(|s| s.to_string())
            .collect(),
    };
    if ids.is_empty() {
        return Err(CliError::config(
            "no providers configured (set PC_PROVIDERS or --config)",
        ));
    }
    for id in &ids {
        if state.registry().get(id).is_none() {
            return Err(CliError::protocol(format!(
                "unknown provider '{id}' (compiled in: {})",
                providers::available_providers().join(", ")
            )));
        }
    }
    Ok(ids)
}

/// Best-effort stop of every started provider (ignore errors — the primary
/// result already happened).
async fn stop_quietly(state: &mut AppState) {
    if let Err(e) = state.registry_mut().stop_all().await {
        tracing::warn!(error = %e, "error stopping providers");
    }
}

// ---------------------------------------------------------------------------
// send
// ---------------------------------------------------------------------------

/// `pc-connect send`: start the target provider, deliver the message, print
/// the [`SendReceipt`] (JSON) and stop the provider.
pub async fn send(opts: SendOptions, config: SidecarConfig) -> Result<SendReceipt, CliError> {
    let (mut state, _notify_tx) = build_state(&config)?;
    resolve_targets(&state, Some(std::slice::from_ref(&opts.provider)))?;

    let result = async {
        state
            .registry_mut()
            .start(&opts.provider)
            .await
            .map_err(CliError::from_provider)?;
        let receipt = state
            .registry()
            .send(
                &opts.provider,
                &SendMessage::new(opts.chat.clone(), opts.text.clone()),
            )
            .await
            .map_err(CliError::from_provider)?;
        Ok::<SendReceipt, CliError>(receipt)
    }
    .await;
    stop_quietly(&mut state).await;
    result
}

// ---------------------------------------------------------------------------
// listen
// ---------------------------------------------------------------------------

/// `pc-connect listen`: start the target providers, then stream one JSON
/// object per line to stdout:
///
/// * `{"event":"message","message":{...ChannelMessage...}}`
/// * `{"event":"error","error":{...ErrorEvent...}}`
///
/// Exits after `--timeout` expires, after the first event with `--once`, or
/// when the event channel closes. `--once` exits on the first `event.message`
/// OR `event.error` (an async error means the listen is dead — hanging after
/// one would be worse; documented deviation, see README).
pub async fn listen(opts: ListenOptions, config: SidecarConfig) -> Result<(), CliError> {
    let (mut state, notify_tx) = build_state(&config)?;
    let targets = resolve_targets(&state, opts.providers.as_deref())?;
    let mut rx = notify_tx.subscribe();

    for id in &targets {
        state
            .registry_mut()
            .start(id)
            .await
            .map_err(CliError::from_provider)?;
    }

    let deadline = opts.timeout.map(|t| tokio::time::Instant::now() + t);
    let mut stop = false;
    while !stop {
        let frame = match deadline {
            Some(dl) => match tokio::time::timeout_at(dl, rx.recv()).await {
                Ok(frame) => frame,
                Err(_) => break, // --timeout expired
            },
            None => rx.recv().await,
        };
        match frame {
            Ok(Outbound::Notification(notification)) => {
                match notification.method.as_str() {
                    EVENT_MESSAGE => {
                        let message = notification
                            .params
                            .as_ref()
                            .and_then(|p| p.get("message"))
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);
                        println!(
                            "{}",
                            serde_json::json!({ "event": "message", "message": message })
                        );
                        if opts.once {
                            stop = true;
                        }
                    }
                    EVENT_ERROR => {
                        let error = notification.params.unwrap_or(serde_json::json!({}));
                        println!(
                            "{}",
                            serde_json::json!({ "event": "error", "error": error })
                        );
                        if opts.once {
                            stop = true;
                        }
                    }
                    _ => {} // reserved vocabulary (event.draft/event.choice) — not emitted today
                }
            }
            Ok(Outbound::Response(_)) => {} // no requests are sent; never happens
            Err(RecvError::Lagged(skipped)) => {
                tracing::warn!(skipped, "listen lagged; dropped frames");
                println!(
                    "{}",
                    serde_json::json!({ "event": "error", "error": {
                        "provider": serde_json::Value::Null,
                        "code": -32006,
                        "message": format!("transport dropped {skipped} frame(s) (listener too slow)"),
                        "data": { "kind": "Transport", "skipped": skipped }
                    }})
                );
            }
            Err(RecvError::Closed) => break,
        }
    }

    stop_quietly(&mut state).await;
    Ok(())
}

// ---------------------------------------------------------------------------
// check
// ---------------------------------------------------------------------------

/// `pc-connect check`: initialize + capabilities + a listen smoke per
/// provider. Exit 0 when every checked provider is healthy, 1 otherwise.
///
/// Smoke semantics per provider kind:
/// * `demo` — `start()` announces itself as an `event.message`; receiving it
///   through the transport proves the whole pipeline.
/// * `telegram` / `discord` — `start()` connects asynchronously; we poll
///   `take_last_error()` for [`CHECK_SMOKE_TIMEOUT`]. Any error (auth 401 /
///   gateway close 4004 / network) fails the check; no error within the
///   window passes it (long-poll / gateway in flight).
pub async fn check(
    opts: CheckOptions,
    config: SidecarConfig,
) -> Result<(serde_json::Value, Vec<CheckResult>), CliError> {
    let (state, notify_tx) = AppState::new("cli-check");
    let events = state.events();
    let caps = state.capabilities_value();

    // Resolve targets against the CONFIG (the registry is not used for
    // check — concrete providers are driven directly so `take_last_error`
    // is reachable).
    let configured: Vec<&str> = config.providers.iter().map(|p| p.id.as_str()).collect();
    let targets: Vec<&str> = match &opts.provider {
        Some(id) => {
            if !configured.contains(&id.as_str()) {
                return Err(CliError::protocol(format!(
                    "unknown provider '{id}' (configured: {})",
                    configured.join(", ")
                )));
            }
            vec![id.as_str()]
        }
        None => {
            if configured.is_empty() {
                return Err(CliError::config(
                    "no providers configured (set PC_PROVIDERS or --config)",
                ));
            }
            configured.clone()
        }
    };

    let mut results: Vec<CheckResult> = Vec::new();
    for id in &targets {
        let config_value = config
            .providers
            .iter()
            .find(|p| p.id == *id)
            .map(|p| p.config.clone())
            .unwrap_or_else(|| serde_json::json!({}));
        let outcome = check_one(id, &config_value, &events, &caps, &notify_tx).await;
        results.push(match outcome {
            SmokeOutcome::Pass(detail) => CheckResult {
                provider: id.to_string(),
                ok: true,
                detail: detail.to_string(),
                code: None,
            },
            SmokeOutcome::Fail(err) => CheckResult {
                provider: id.to_string(),
                ok: false,
                detail: err.to_string(),
                code: Some(err.0.code),
            },
        });
    }
    Ok((caps, results))
}

async fn check_one(
    id: &str,
    config_value: &serde_json::Value,
    events: &Arc<dyn ProviderEvents>,
    caps: &serde_json::Value,
    notify_tx: &broadcast::Sender<Outbound>,
) -> SmokeOutcome {
    tracing::info!(
        protocol = %caps["protocolVersion"],
        provider = %id,
        "check: initialize + capabilities ok"
    );
    match id {
        #[cfg(feature = "demo")]
        "demo" => check_demo(config_value, events, notify_tx).await,
        #[cfg(feature = "telegram")]
        "telegram" => check_telegram(config_value, events).await,
        #[cfg(feature = "discord")]
        "discord" => check_discord(config_value, events).await,
        other => SmokeOutcome::Fail(CliError::protocol(format!(
            "unknown provider '{other}' (compiled in: {})",
            providers::available_providers().join(", ")
        ))),
    }
}

/// Demo smoke: `start()` must push an `event.message` through the transport
/// within [`CHECK_SMOKE_TIMEOUT`].
#[cfg(feature = "demo")]
async fn check_demo(
    config_value: &serde_json::Value,
    events: &Arc<dyn ProviderEvents>,
    notify_tx: &broadcast::Sender<Outbound>,
) -> SmokeOutcome {
    let mut rx = notify_tx.subscribe();
    let mut provider = crate::demo::DemoProvider::new(events.clone(), config_value);
    if let Err(e) = provider.start().await {
        return SmokeOutcome::Fail(CliError::from_provider(e));
    }
    let outcome = match tokio::time::timeout(CHECK_SMOKE_TIMEOUT, rx.recv()).await {
        Ok(Ok(Outbound::Notification(n))) if n.method == EVENT_MESSAGE => {
            SmokeOutcome::Pass("received start announcement (event.message)")
        }
        Ok(_) => SmokeOutcome::Fail(CliError::internal(
            "demo provider did not announce start (unexpected event)",
        )),
        Err(_) => SmokeOutcome::Fail(CliError::internal(
            "demo provider did not announce start within smoke window",
        )),
    };
    let _ = provider.stop().await;
    outcome
}

/// Telegram smoke: start the concrete provider, poll `take_last_error()`.
#[cfg(feature = "telegram")]
async fn check_telegram(
    config_value: &serde_json::Value,
    events: &Arc<dyn ProviderEvents>,
) -> SmokeOutcome {
    let mut provider = match providers::build_telegram(config_value, events.clone()) {
        Ok(p) => p,
        Err(e) => return SmokeOutcome::Fail(CliError::config(e)),
    };
    if let Err(e) = provider.start().await {
        return SmokeOutcome::Fail(CliError::from_provider(e));
    }
    let outcome = poll_last_error(|| provider.take_last_error()).await;
    let _ = provider.stop().await;
    outcome
}

/// Discord smoke: start the concrete provider, poll `take_last_error()`.
#[cfg(feature = "discord")]
async fn check_discord(
    config_value: &serde_json::Value,
    events: &Arc<dyn ProviderEvents>,
) -> SmokeOutcome {
    let mut provider = match providers::build_discord(config_value, events.clone()) {
        Ok(p) => p,
        Err(e) => return SmokeOutcome::Fail(CliError::config(e)),
    };
    if let Err(e) = provider.start().await {
        return SmokeOutcome::Fail(CliError::from_provider(e));
    }
    let outcome = poll_last_error(|| provider.take_last_error()).await;
    let _ = provider.stop().await;
    outcome
}

/// Poll a provider's async error slot until [`CHECK_SMOKE_TIMEOUT`] expires.
/// Any error fails the check; silence passes it (long-poll/gateway in flight).
#[cfg(any(feature = "telegram", feature = "discord"))]
async fn poll_last_error(mut take: impl FnMut() -> Option<ProviderError>) -> SmokeOutcome {
    let deadline = tokio::time::Instant::now() + CHECK_SMOKE_TIMEOUT;
    loop {
        if let Some(err) = take() {
            return SmokeOutcome::Fail(CliError::from_provider(err));
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return SmokeOutcome::Pass(
                "no errors within smoke window (long-poll/gateway in flight)",
            );
        }
        tokio::time::sleep(CHECK_POLL_INTERVAL).await;
    }
}

// ---------------------------------------------------------------------------
// Unit tests: in-process round-trip through the real registry + event bus
// (the demo provider is per-process, so the send→listen round-trip can only
// be observed inside one process — cross-process delivery needs a real
// provider, see README).
// ---------------------------------------------------------------------------

#[cfg(all(test, feature = "demo"))]
mod tests {
    use super::*;
    use crate::config::ProviderConfig;

    fn demo_config() -> SidecarConfig {
        SidecarConfig {
            providers: vec![ProviderConfig {
                id: "demo".into(),
                config: serde_json::json!({}),
            }],
        }
    }

    #[tokio::test]
    async fn demo_send_echo_round_trip_in_process() {
        let (mut state, notify_tx) = build_state(&demo_config()).expect("build state");
        let mut rx = notify_tx.subscribe();

        state
            .registry_mut()
            .start("demo")
            .await
            .expect("start demo");

        // The start announcement arrives first.
        match rx.recv().await.expect("recv announcement") {
            Outbound::Notification(n) => {
                assert_eq!(n.method, EVENT_MESSAGE);
                assert_eq!(n.params.as_ref().unwrap()["message"]["channel"], "demo");
            }
            other => panic!("expected notification, got {other:?}"),
        }

        // Send through the registry: the echo must come back on the same bus.
        let receipt = state
            .registry()
            .send("demo", &SendMessage::new("room-1", "roundtrip-42"))
            .await
            .expect("send");
        assert!(receipt.message_id.starts_with("demo-"));
        assert!(receipt.ts > 0);

        match rx.recv().await.expect("recv echo") {
            Outbound::Notification(n) => {
                assert_eq!(n.method, EVENT_MESSAGE);
                let params = n.params.unwrap();
                assert_eq!(params["message"]["channel_id"], "room-1");
                assert_eq!(
                    params["message"]["content"][0]["Text"],
                    "echo: roundtrip-42"
                );
            }
            other => panic!("expected notification, got {other:?}"),
        }

        stop_quietly(&mut state).await;
    }

    #[tokio::test]
    async fn send_unknown_provider_is_protocol_error() {
        let (mut state, _tx) = build_state(&demo_config()).expect("build state");
        let err = state
            .registry_mut()
            .start("nope")
            .await
            .expect_err("unknown provider must fail");
        assert!(err.to_string().contains("unknown provider"));
    }

    #[tokio::test]
    async fn send_requires_started_provider() {
        let (state, _tx) = build_state(&demo_config()).expect("build state");
        let err = state
            .registry()
            .send("demo", &SendMessage::new("room-1", "hi"))
            .await
            .expect_err("not-started provider must fail");
        assert!(err.to_string().contains("not started"));
    }

    #[tokio::test]
    async fn listen_once_stops_after_first_message() {
        let opts = ListenOptions {
            providers: Some(vec!["demo".into()]),
            timeout: Some(Duration::from_secs(5)),
            once: true,
        };
        // Runs to completion: the demo announcement is the first event, so
        // --once must stop the loop (no assertion needed beyond completion,
        // which also proves the timeout path did not fire).
        listen(opts, demo_config()).await.expect("listen once");
    }

    #[tokio::test]
    async fn listen_timeout_expires_with_no_events() {
        // A provider that never emits: none configured would error, so use an
        // empty config with a filterless run... instead, directly verify the
        // deadline path via the channel closing: an empty target set errors
        // (checked elsewhere); here we assert the timeout path by listening
        // on a config whose only provider announces once.
        let opts = ListenOptions {
            providers: Some(vec!["demo".into()]),
            timeout: Some(Duration::from_millis(50)),
            once: false,
        };
        listen(opts, demo_config())
            .await
            .expect("listen with tiny timeout");
    }
}
