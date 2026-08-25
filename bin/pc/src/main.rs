//! `pc` — the provider-connect sidecar + CLI binary.
//!
//! ```text
//! pc                          # sidecar stdio (default, back-compat)
//! pc sidecar [--config path]  # explicit sidecar
//! pc send --provider id --chat id --text ...  [--config path]
//! pc listen [--providers a,b] [--once] [--timeout 5] [--config path]
//! pc check [--provider id] [--config path]
//! pc serve [--ws :8787] [--http :8788] [--config path]  (stub)
//! pc init                     (stub)
//! ```
//!
//! Reads config (JSON file or env), loads the feature-gated providers into a
//! registry, and serves either the JSON-RPC 2.0 protocol over stdio
//! (sidecar) or the one-shot `send`/`listen`/`check` operations.
//! All logging goes to **stderr** — stdout is reserved for JSON-RPC / JSON output.

#[cfg(feature = "demo")]
mod demo;

use std::io::Read;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use provider_core::{ChatProvider, ProviderEvents, ProviderRegistry, SendMessage, SendReceipt};
use provider_transport::events::{EVENT_ERROR, EVENT_MESSAGE};
use provider_transport::jsonrpc::JsonRpcError;
use provider_transport::state::{provider_error, AppState, Outbound};
use tokio::sync::broadcast;
use tracing_subscriber::EnvFilter;

// ---------------------------------------------------------------------------
// CLI (clap)
// ---------------------------------------------------------------------------

#[derive(Parser)]
#[command(name = "pc", version, about = "provider-connect sidecar (JSON-RPC 2.0 over stdio)")]
struct Cli {
    /// Path to a JSON config file
    #[arg(long, short = 'c', global = true, value_name = "PATH")]
    config: Option<String>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the stdio sidecar (default when no subcommand is given)
    Sidecar,
    /// Send a message through a provider
    Send {
        /// Provider id: demo (built-in), telegram, discord (feature-gated)
        #[arg(long)]
        provider: String,
        /// Chat/room id to deliver to
        #[arg(long)]
        chat: String,
        /// Message text (mutually exclusive with --text-file)
        #[arg(long, conflicts_with = "text_file")]
        text: Option<String>,
        /// Read message text from a file; use "-" for stdin
        #[arg(long, value_name = "PATH")]
        text_file: Option<String>,
        /// Machine-readable JSON output (default for send)
        #[arg(long)]
        json: bool,
    },
    /// Start providers and stream events as JSON lines
    Listen {
        /// Comma-separated provider ids to start (default: all configured)
        #[arg(long, value_delimiter = ',', value_name = "a,b")]
        providers: Option<Vec<String>>,
        /// Exit after N seconds even if no event arrived
        #[arg(long, value_name = "SECS")]
        timeout: Option<u64>,
        /// Exit after the first event
        #[arg(long)]
        once: bool,
        /// Machine-readable JSON output (default for listen)
        #[arg(long)]
        json: bool,
    },
    /// Connectivity check: initialize + capabilities + listen smoke per provider
    Check {
        /// Check only this provider id (default: all configured)
        #[arg(long)]
        provider: Option<String>,
        /// Machine-readable JSON output
        #[arg(long)]
        json: bool,
    },
    /// Serve over WebSocket / HTTP (daemon: holds provider connections once, fans out to clients)
    Serve {
        /// WebSocket listen address (e.g. :8787 or 127.0.0.1:8787)
        #[arg(long, value_name = "ADDR")]
        ws: Option<String>,
        /// HTTP listen address (e.g. :8788 or 127.0.0.1:8788)
        #[arg(long, value_name = "ADDR")]
        http: Option<String>,
        /// Watch pc.config.* for changes (polls every 1s; hot-reload stub)
        #[arg(long)]
        watch: bool,
        /// SQLite file for durable event log (enables replay via ?since=)
        #[arg(long, value_name = "PATH")]
        persist: Option<String>,
        /// Disable SQLite persistence (stay in-memory even with persist feature)
        #[arg(long)]
        no_persist: bool,
    },
    /// Dev server — alias for `serve --watch` (Rsbuild analog)
    Dev {
        /// WebSocket listen address (e.g. :8787 or 127.0.0.1:8787)
        #[arg(long, value_name = "ADDR")]
        ws: Option<String>,
        /// HTTP listen address (e.g. :8788 or 127.0.0.1:8788)
        #[arg(long, value_name = "ADDR")]
        http: Option<String>,
        /// SQLite file for durable event log
        #[arg(long, value_name = "PATH")]
        persist: Option<String>,
        /// Disable SQLite persistence
        #[arg(long)]
        no_persist: bool,
    },
    /// Scaffold a new provider-connect config (stub)
    Init,
}

// ---------------------------------------------------------------------------
// Entry
// ---------------------------------------------------------------------------

fn main() -> ExitCode {
    let cli = Cli::parse();
    let config_path = cli.config.clone();
    match cli.command {
        None => run_sidecar(config_path),
        Some(Commands::Sidecar) => run_sidecar(config_path),
        Some(Commands::Send {
            provider,
            chat,
            text,
            text_file,
            json: _,
        }) => dispatch_send(SendArgs {
            provider,
            chat,
            text,
            text_file,
            config: config_path,
        }),
        Some(Commands::Listen {
            providers,
            timeout,
            once,
            json: _,
        }) => dispatch_listen(ListenArgs {
            providers,
            timeout_secs: timeout,
            once,
            config: config_path,
        }),
        Some(Commands::Check { provider, json }) => dispatch_check(CheckArgs {
            provider,
            json,
            config: config_path,
        }),
        Some(Commands::Serve {
            ws,
            http,
            watch,
            persist,
            no_persist,
        }) => run_serve(ServeArgs {
            config_path,
            ws,
            http,
            watch,
            persist,
            no_persist,
        }),
        Some(Commands::Dev {
            ws,
            http,
            persist,
            no_persist,
        }) => run_serve(ServeArgs {
            config_path,
            ws,
            http,
            watch: true,
            persist,
            no_persist,
        }),
        Some(Commands::Init) => run_init(),
    }
}

// ---------------------------------------------------------------------------
// pc init — scaffold a minimal config file
// ---------------------------------------------------------------------------

fn run_init() -> ExitCode {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let json_path = cwd.join("pc.config.json");
    let ts_path = cwd.join("pc.config.ts");
    if json_path.exists() || ts_path.exists() {
        let existing = if json_path.exists() { &json_path } else { &ts_path };
        eprintln!("pc init: config already exists at {} — leaving it untouched", existing.display());
        return ExitCode::SUCCESS;
    }
    let content = "{\n  \"providers\": [{ \"id\": \"demo\", \"config\": {} }]\n}\n";
    match std::fs::write(&json_path, content) {
        Ok(()) => {
            println!("pc init: wrote {}", json_path.display());
            println!("hint: pc check --config {}  |  pc serve", json_path.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("pc init: failed to write {}: {e}", json_path.display());
            ExitCode::FAILURE
        }
    }
}

// ---------------------------------------------------------------------------
// Sidecar
// ---------------------------------------------------------------------------

fn run_sidecar(config_path: Option<String>) -> ExitCode {
    init_tracing();
    let config = match provider_config::load(config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("pc: failed to load config: {e}");
            return ExitCode::FAILURE;
        }
    };

    let (state, notify_tx) = match build_app_state(&config, "stdio") {
        Ok(v) => v,
        Err(e) => {
            eprintln!("pc: {e}");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(providers = ?state.registry().ids(), "providers registered");

    // Drop local event-sink handle so broadcast can close on shutdown.
    drop(state.events().clone());

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("pc: failed to build tokio runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(provider_transport::stdio::serve_stdio(
        state,
        notify_tx,
        tokio::io::stdin(),
        tokio::io::stdout(),
    )) {
        Ok(()) => {
            tracing::info!("sidecar shut down cleanly");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("pc: transport error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    // Logs go to stderr; stdout is the JSON-RPC channel.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

// ---------------------------------------------------------------------------
// DRY provider wiring helper
// ---------------------------------------------------------------------------

/// Register providers from `config` into `registry`, collecting failures.
/// Shared helper so sidecar and send/listen/check use identical wiring.
fn register_providers(
    config: &provider_config::SidecarConfig,
    registry: &mut ProviderRegistry,
    events: &Arc<dyn ProviderEvents>,
) -> Result<(), String> {
    let mut failures: Vec<String> = Vec::new();
    for provider in &config.providers {
        match build_provider(&provider.id, &provider.config, events.clone()) {
            Ok(boxed) => {
                if let Err(e) = registry.register(boxed) {
                    failures.push(format!("{}: {e}", provider.id));
                }
            }
            Err(e) => failures.push(format!("{}: {e}", provider.id)),
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "failed to load {} provider(s): {}",
            failures.len(),
            failures.join("; ")
        ))
    }
}

/// Build the provider registry from config. Used by both sidecar and ops.
fn build_app_state(
    config: &provider_config::SidecarConfig,
    transport: &str,
) -> Result<(AppState, broadcast::Sender<Outbound>), String> {
    build_app_state_with_transports(config, vec![transport.to_string()])
}

fn build_app_state_with_transports(
    config: &provider_config::SidecarConfig,
    transports: Vec<String>,
) -> Result<(AppState, broadcast::Sender<Outbound>), String> {
    let (mut state, notify_tx) = AppState::new_with_transports(transports);
    let events = state.events();
    register_providers(config, state.registry_mut(), &events).map_err(|e| e)?;
    drop(events);
    Ok((state, notify_tx))
}

struct ServeArgs {
    config_path: Option<String>,
    ws: Option<String>,
    http: Option<String>,
    watch: bool,
    persist: Option<String>,
    no_persist: bool,
}

/// Convenience helper returning just the registry (spec name).
#[allow(dead_code)]
fn build_registry(config: &provider_config::SidecarConfig) -> Result<ProviderRegistry, String> {
    let (state, _) = AppState::new("stdio");
    let events = state.events();
    let mut registry = ProviderRegistry::new(events.clone());
    register_providers(config, &mut registry, &events)?;
    Ok(registry)
}

// ---------------------------------------------------------------------------
// send / listen / check ops (ported from cli/src/ops.rs)
// ---------------------------------------------------------------------------

struct SendArgs {
    provider: String,
    chat: String,
    text: Option<String>,
    text_file: Option<String>,
    config: Option<String>,
}

struct ListenArgs {
    providers: Option<Vec<String>>,
    timeout_secs: Option<u64>,
    once: bool,
    config: Option<String>,
}

struct CheckArgs {
    provider: Option<String>,
    json: bool,
    config: Option<String>,
}

fn runtime() -> Result<tokio::runtime::Runtime, CliError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| CliError::internal(format!("failed to build tokio runtime: {e}")))
}

fn fail(err: &CliError) -> ExitCode {
    println!("{}", serde_json::json!({ "error": &err.0 }));
    eprintln!("pc: {err}");
    ExitCode::FAILURE
}

fn dispatch_send(args: SendArgs) -> ExitCode {
    init_tracing();
    let config = match provider_config::load(args.config.clone()) {
        Ok(c) => c,
        Err(e) => return fail(&CliError::config(format!("failed to load config: {e}"))),
    };
    let text = match read_text(&args.text, &args.text_file) {
        Ok(t) => t,
        Err(e) => return fail(&e),
    };
    let opts = SendOptions {
        provider: args.provider,
        chat: args.chat,
        text,
    };
    let rt = match runtime() {
        Ok(rt) => rt,
        Err(e) => return fail(&e),
    };
    match rt.block_on(op_send(opts, config)) {
        Ok(receipt) => match serde_json::to_string(&receipt) {
            Ok(line) => {
                println!("{line}");
                ExitCode::SUCCESS
            }
            Err(e) => fail(&CliError::internal(format!("serialize receipt: {e}"))),
        },
        Err(e) => fail(&e),
    }
}

fn dispatch_listen(args: ListenArgs) -> ExitCode {
    init_tracing();
    let config = match provider_config::load(args.config.clone()) {
        Ok(c) => c,
        Err(e) => return fail(&CliError::config(format!("failed to load config: {e}"))),
    };
    let opts = ListenOptions {
        providers: args.providers,
        timeout: args.timeout_secs.map(Duration::from_secs),
        once: args.once,
    };
    let rt = match runtime() {
        Ok(rt) => rt,
        Err(e) => return fail(&e),
    };
    match rt.block_on(op_listen(opts, config)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(&e),
    }
}

fn dispatch_check(args: CheckArgs) -> ExitCode {
    init_tracing();
    let config = match provider_config::load(args.config.clone()) {
        Ok(c) => c,
        Err(e) => return fail(&CliError::config(format!("failed to load config: {e}"))),
    };
    let opts = CheckOptions {
        provider: args.provider,
    };
    let rt = match runtime() {
        Ok(rt) => rt,
        Err(e) => return fail(&e),
    };
    match rt.block_on(op_check(opts, config)) {
        Ok((caps, results)) => {
            let all_ok = results.iter().all(|r| r.ok);
            if args.json {
                let providers: Vec<serde_json::Value> = results
                    .iter()
                    .map(|r| {
                        serde_json::json!({
                            "provider": r.provider,
                            "ok": r.ok,
                            "detail": r.detail,
                            "code": r.code,
                        })
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::json!({
                        "ok": all_ok,
                        "protocolVersion": caps["protocolVersion"],
                        "methods": caps["methods"],
                        "notifications": caps["notifications"],
                        "providers": providers,
                    })
                );
            } else {
                println!(
                    "pc: check: protocol {} (methods: {})",
                    caps["protocolVersion"].as_str().unwrap_or("?"),
                    caps["methods"]
                        .as_array()
                        .map(|m| m
                            .iter()
                            .filter_map(|v| v.as_str())
                            .collect::<Vec<_>>()
                            .join(", "))
                        .unwrap_or_default()
                );
                for r in &results {
                    let verdict = if r.ok { "OK" } else { "FAIL" };
                    println!("pc: check: {}: {verdict} — {}", r.provider, r.detail);
                }
            }
            if all_ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(e) => fail(&e),
    }
}

fn read_text(text: &Option<String>, text_file: &Option<String>) -> Result<String, CliError> {
    match (text, text_file) {
        (Some(t), _) => Ok(t.clone()),
        (None, Some(path)) => {
            let mut buf = String::new();
            if path == "-" {
                std::io::stdin()
                    .read_to_string(&mut buf)
                    .map_err(CliError::from)?;
            } else {
                buf = std::fs::read_to_string(path).map_err(|e| {
                    CliError::config(format!("failed to read --text-file {path:?}: {e}"))
                })?;
            }
            Ok(strip_trailing_newline(buf))
        }
        (None, None) => Err(CliError::config(
            "send requires --text <text> or --text-file <path|- >",
        )),
    }
}

fn strip_trailing_newline(mut s: String) -> String {
    if s.ends_with('\n') {
        s.pop();
        if s.ends_with('\r') {
            s.pop();
        }
    }
    s
}

// --- CliError + option structs (mirrors cli/src/ops.rs) ---

#[derive(Debug, Clone)]
struct CliError(JsonRpcError);

impl CliError {
    fn config(message: impl Into<String>) -> Self {
        CliError(JsonRpcError::new(JsonRpcError::CONFIG_ERROR, message, None))
    }
    fn protocol(message: impl Into<String>) -> Self {
        CliError(JsonRpcError::new(
            JsonRpcError::PROTOCOL_ERROR,
            message,
            None,
        ))
    }
    fn internal(message: impl Into<String>) -> Self {
        CliError(JsonRpcError::new(
            JsonRpcError::INTERNAL_ERROR,
            message,
            None,
        ))
    }
    fn from_provider(e: provider_core::ProviderError) -> Self {
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

#[derive(Debug, Clone)]
struct SendOptions {
    provider: String,
    chat: String,
    text: String,
}

#[derive(Debug, Clone)]
struct ListenOptions {
    providers: Option<Vec<String>>,
    timeout: Option<Duration>,
    once: bool,
}

#[derive(Debug, Clone)]
struct CheckOptions {
    provider: Option<String>,
}

#[derive(Debug, Clone)]
struct CheckResult {
    provider: String,
    ok: bool,
    detail: String,
    code: Option<i64>,
}

enum SmokeOutcome {
    Pass(&'static str),
    Fail(CliError),
}

const CHECK_SMOKE_TIMEOUT: Duration = Duration::from_secs(6);
#[cfg(any(feature = "telegram", feature = "discord"))]
const CHECK_POLL_INTERVAL: Duration = Duration::from_millis(100);

fn build_state_for_ops(
    config: &provider_config::SidecarConfig,
) -> Result<(AppState, broadcast::Sender<Outbound>), CliError> {
    build_app_state(config, "cli").map_err(CliError::config)
}

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
                available_providers().join(", ")
            )));
        }
    }
    Ok(ids)
}

async fn stop_quietly(state: &mut AppState) {
    if let Err(e) = state.registry_mut().stop_all().await {
        tracing::warn!(error = %e, "error stopping providers");
    }
}

// -- send

async fn op_send(opts: SendOptions, config: provider_config::SidecarConfig) -> Result<SendReceipt, CliError> {
    let (mut state, _notify_tx) = build_state_for_ops(&config)?;
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

// -- listen

async fn op_listen(opts: ListenOptions, config: provider_config::SidecarConfig) -> Result<(), CliError> {
    let (mut state, notify_tx) = build_state_for_ops(&config)?;
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
                Err(_) => break,
            },
            None => rx.recv().await,
        };
        match frame {
            Ok(Outbound::Notification(notification)) => match notification.method.as_str() {
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
                _ => {}
            },
            Ok(Outbound::Response(_)) => {}
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
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
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }

    stop_quietly(&mut state).await;
    Ok(())
}

// -- check

async fn op_check(
    opts: CheckOptions,
    config: provider_config::SidecarConfig,
) -> Result<(serde_json::Value, Vec<CheckResult>), CliError> {
    let (state, notify_tx) = AppState::new("cli-check");
    let events = state.events();
    let caps = state.capabilities_value();

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
            available_providers().join(", ")
        ))),
    }
}

#[cfg(feature = "demo")]
async fn check_demo(
    config_value: &serde_json::Value,
    events: &Arc<dyn ProviderEvents>,
    notify_tx: &broadcast::Sender<Outbound>,
) -> SmokeOutcome {
    let mut rx = notify_tx.subscribe();
    let mut provider = demo::DemoProvider::new(events.clone(), config_value);
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

#[cfg(feature = "telegram")]
async fn check_telegram(
    config_value: &serde_json::Value,
    events: &Arc<dyn ProviderEvents>,
) -> SmokeOutcome {
    let mut provider = match build_telegram_concrete(config_value, events.clone()) {
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

#[cfg(feature = "discord")]
async fn check_discord(
    config_value: &serde_json::Value,
    events: &Arc<dyn ProviderEvents>,
) -> SmokeOutcome {
    let mut provider = match build_discord_concrete(config_value, events.clone()) {
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

#[cfg(any(feature = "telegram", feature = "discord"))]
async fn poll_last_error(
    mut take: impl FnMut() -> Option<provider_core::ProviderError>,
) -> SmokeOutcome {
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

#[cfg(feature = "telegram")]
fn build_telegram_concrete(
    config: &serde_json::Value,
    events: Arc<dyn ProviderEvents>,
) -> Result<provider_telegram::TelegramProvider, String> {
    let token = config_token("telegram", config)?;
    let mut provider = provider_telegram::TelegramProvider::new(token, events);
    if let Some(base) = config_str("telegram", config, "base_url")? {
        provider = provider.with_base_url(base);
    }
    if let Some(secs) = config_u64("telegram", config, "poll_interval_secs")? {
        provider = provider.with_poll_interval(Duration::from_secs(secs));
    }
    if let Some(secs) = config_u64("telegram", config, "long_poll_timeout_secs")? {
        provider = provider.with_long_poll_timeout_secs(secs);
    }
    if let Some(secs) = config_u64("telegram", config, "request_timeout_secs")? {
        provider = provider.with_request_timeout(Duration::from_secs(secs));
    }
    Ok(provider)
}

#[cfg(feature = "discord")]
fn build_discord_concrete(
    config: &serde_json::Value,
    events: Arc<dyn ProviderEvents>,
) -> Result<provider_discord::DiscordProvider, String> {
    let token = config_token("discord", config)?;
    let mut provider = provider_discord::DiscordProvider::new(token, events);
    if let Some(url) = config_str("discord", config, "gateway_url")? {
        provider = provider.with_gateway_url(url);
    }
    if let Some(base) = config_str("discord", config, "rest_base")? {
        provider = provider.with_rest_base(base);
    }
    if let Some(intents) = config_u64("discord", config, "intents")? {
        provider = provider.with_intents(intents);
    }
    if let Some(secs) = config_u64("discord", config, "request_timeout_secs")? {
        provider = provider.with_request_timeout(Duration::from_secs(secs));
    }
    Ok(provider)
}

// ---------------------------------------------------------------------------
// Feature-gated provider construction (shared by sidecar + ops)
// ---------------------------------------------------------------------------

fn build_provider(
    id: &str,
    config: &serde_json::Value,
    events: Arc<dyn ProviderEvents>,
) -> Result<Box<dyn ChatProvider>, String> {
    match id {
        #[cfg(feature = "demo")]
        "demo" => Ok(Box::new(demo::DemoProvider::new(events, config))),
        #[cfg(feature = "telegram")]
        "telegram" => {
            let p = build_telegram_concrete(config, events)?;
            Ok(Box::new(p))
        }
        #[cfg(feature = "discord")]
        "discord" => {
            let p = build_discord_concrete(config, events)?;
            Ok(Box::new(p))
        }
        other => Err(format!(
            "unknown provider '{other}' (compiled in: {})",
            available_providers().join(", ")
        )),
    }
}

#[cfg(any(feature = "telegram", feature = "discord"))]
fn config_token(id: &str, config: &serde_json::Value) -> Result<String, String> {
    config
        .get("token")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("provider '{id}' requires config.token"))
}

#[cfg(any(feature = "telegram", feature = "discord"))]
fn config_str(id: &str, config: &serde_json::Value, key: &str) -> Result<Option<String>, String> {
    match config.get(key) {
        None => Ok(None),
        Some(v) => v
            .as_str()
            .map(|s| Some(s.to_string()))
            .ok_or_else(|| format!("provider '{id}' config.{key} must be a string")),
    }
}

#[cfg(any(feature = "telegram", feature = "discord"))]
fn config_u64(id: &str, config: &serde_json::Value, key: &str) -> Result<Option<u64>, String> {
    match config.get(key) {
        None => Ok(None),
        Some(v) => v
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("provider '{id}' config.{key} must be a non-negative integer")),
    }
}

fn available_providers() -> Vec<&'static str> {
    let mut ids = Vec::new();
    #[cfg(feature = "demo")]
    ids.extend(["demo"]);
    #[cfg(feature = "telegram")]
    ids.extend(["telegram"]);
    #[cfg(feature = "discord")]
    ids.extend(["discord"]);
    ids
}

// ---------------------------------------------------------------------------
// pc serve (Phase 06): bod server — HTTP + WS daemon
// ---------------------------------------------------------------------------

fn parse_listen_addr(raw: &str) -> Result<std::net::SocketAddr, String> {
    let s = if raw.starts_with(':') {
        format!("0.0.0.0{raw}")
    } else {
        raw.to_string()
    };
    s.parse::<std::net::SocketAddr>()
        .map_err(|e| format!("invalid listen addr {raw:?}: {e} (try :8788 or 127.0.0.1:8788)"))
}

fn run_serve(args: ServeArgs) -> ExitCode {
    let ServeArgs {
        config_path,
        ws,
        http,
        watch,
        persist,
        no_persist,
    } = args;
    init_tracing();
    let (ws_addr, http_addr) = match (ws, http) {
        (None, None) => (Some(":8787".to_string()), Some(":8788".to_string())),
        (w, h) => (w, h),
    };
    let watch_config_path = config_path.clone();
    if watch {
        tracing::info!("watch: monitoring pc.config.* for changes");
        eprintln!("watch: monitoring pc.config.* for changes");
    }
    let config = match provider_config::load(config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("pc: failed to load config: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut transports: Vec<String> = Vec::new();
    if ws_addr.is_some() {
        transports.push("ws".to_string());
    }
    if http_addr.is_some() {
        transports.push("http".to_string());
    }
    if !transports.contains(&"stdio".to_string()) {
        transports.push("stdio".to_string());
    }
    let (state, notify_tx) = match build_app_state_with_transports(&config, transports) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("pc: {e}");
            return ExitCode::FAILURE;
        }
    };
    // Wire SQLite persistence when available and not disabled.
    #[allow(unused_mut)]
    let mut state = state;
    #[cfg(feature = "persist")]
    {
        if no_persist {
            tracing::info!("persist disabled via --no-persist (in-memory only)");
        } else {
            let path = persist
                .or_else(|| std::env::var("PC_PERSIST_PATH").ok())
                .unwrap_or_else(|| "./pc-events.db".to_string());
            match state.with_persist(&path) {
                Ok(s) => {
                    tracing::info!(path = %path, "persist enabled (WAL sqlite)");
                    eprintln!("persist: sqlite at {}", path);
                    state = s;
                }
                Err(e) => {
                    eprintln!("pc: persist failed for {}: {e}", path);
                    return ExitCode::FAILURE;
                }
            }
        }
    }
    #[cfg(not(feature = "persist"))]
    {
        if persist.is_some() {
            eprintln!("pc: --persist requires building with --features persist");
            return ExitCode::FAILURE;
        }
        if std::env::var("PC_PERSIST_PATH").is_ok() {
            eprintln!("pc: PC_PERSIST_PATH set but binary built without --features persist — ignoring");
        }
    }
    tracing::info!(providers = ?state.registry().ids(), "providers registered for serve");
    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => { eprintln!("pc: failed to build tokio runtime: {e}"); return ExitCode::FAILURE; }
    };
    let state = std::sync::Arc::new(tokio::sync::Mutex::new(state));
    #[cfg(feature = "ws")]
    let notify_for_ws = notify_tx.clone();
    #[cfg(not(feature = "ws"))]
    let _notify_for_ws = notify_tx.clone();
    let watch_enabled = watch;
    let watch_path = watch_config_path.clone();
    let result: Result<(), String> = runtime.block_on(async move {
        let http_listener: Option<tokio::net::TcpListener> = match http_addr {
            Some(raw) => {
                let addr = parse_listen_addr(&raw).map_err(|e| e)?;
                let l = tokio::net::TcpListener::bind(addr).await.map_err(|e| format!("bind http {raw}: {e}"))?;
                tracing::info!(addr = %l.local_addr().map_err(|e| e.to_string())?, "http listening");
                Some(l)
            }
            None => None,
        };
        let ws_listener: Option<tokio::net::TcpListener> = match ws_addr {
            Some(raw) => {
                let addr = parse_listen_addr(&raw).map_err(|e| e)?;
                let l = tokio::net::TcpListener::bind(addr).await.map_err(|e| format!("bind ws {raw}: {e}"))?;
                tracing::info!(addr = %l.local_addr().map_err(|e| e.to_string())?, "ws listening");
                Some(l)
            }
            None => None,
        };
        if http_listener.is_none() && ws_listener.is_none() {
            return Err("pc serve: no listeners (pass --http or --ws)".to_string());
        }
        let mut handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();
        if let Some(listener) = http_listener {
            #[cfg(feature = "http")]
            {
                let state = state.clone();
                handles.push(tokio::spawn(async move {
                    if let Err(e) = provider_transport::http::serve_http(state, listener).await {
                        tracing::error!(error = %e, "http server exited");
                    }
                }));
            }
            #[cfg(not(feature = "http"))]
            {
                let _ = (state.clone(), listener);
                return Err("http requested but pc was built without --features http".to_string());
            }
        }
        if let Some(listener) = ws_listener {
            #[cfg(feature = "ws")]
            {
                let state = state.clone();
                let notify_tx = notify_for_ws.clone();
                handles.push(tokio::spawn(async move {
                    if let Err(e) = provider_transport::ws::serve_ws(state, notify_tx, listener).await {
                        tracing::error!(error = %e, "ws server exited");
                    }
                }));
            }
            #[cfg(not(feature = "ws"))]
            {
                let _ = (state.clone(), listener);
                return Err("ws requested but pc was built without --features ws".to_string());
            }
        }
        // --watch: simple mtime poll every 1s (no notify crate)
        let _watch_handle: Option<tokio::task::JoinHandle<()>> = if watch_enabled {
            let watch_path_owned = watch_path.clone();
            Some(tokio::spawn(async move {
                let candidates: Vec<std::path::PathBuf> = if let Some(p) = watch_path_owned {
                    vec![std::path::PathBuf::from(p)]
                } else {
                    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                    vec![cwd.join("pc.config.json"), cwd.join("pc.config.ts"), cwd.join("pc.config.js")]
                };
                let mut last_mtimes: Vec<Option<std::time::SystemTime>> = candidates.iter().map(|p| std::fs::metadata(p).ok().and_then(|m| m.modified().ok())).collect();
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    for (i, path) in candidates.iter().enumerate() {
                        let cur = std::fs::metadata(path).ok().and_then(|m| m.modified().ok());
                        if cur != last_mtimes[i] {
                            last_mtimes[i] = cur;
                            if cur.is_some() || last_mtimes[i].is_some() {
                                tracing::info!(path = %path.display(), "watch: config changed — restart pc to apply (hot-reload stub)");
                                eprintln!("watch: {} changed — restart pc to apply", path.display());
                            }
                        }
                    }
                }
            }))
        } else {
            None
        };
        tracing::info!("pc serve ready (Ctrl-C to stop)");
        tokio::signal::ctrl_c().await.map_err(|e| format!("signal error: {e}"))?;
        tracing::info!("pc serve shutting down");
        if let Some(h) = _watch_handle { h.abort(); }
        for h in handles { h.abort(); }
        let mut guard = state.lock().await;
        if let Err(e) = guard.registry_mut().stop_all().await {
            tracing::warn!(error = %e, "error stopping providers on serve shutdown");
        }
        Ok(())
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => { eprintln!("pc serve: {e}"); ExitCode::FAILURE }
    }
}

