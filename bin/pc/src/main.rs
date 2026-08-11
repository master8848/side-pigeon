//! `pc` — the provider-connect sidecar binary.
//!
//! Reads config (JSON file or env), loads the feature-gated providers into a
//! registry, and serves the JSON-RPC 2.0 protocol over stdio
//! (newline-delimited). All logging goes to **stderr** — stdout is reserved
//! for the JSON-RPC channel.

mod config;

#[cfg(feature = "demo")]
mod demo;

use std::process::ExitCode;
use std::sync::Arc;

use provider_core::{ChatProvider, ProviderEvents};
use provider_transport::state::AppState;
use tracing_subscriber::EnvFilter;

const USAGE: &str = r#"pc — provider-connect sidecar (JSON-RPC 2.0 over stdio)

USAGE:
    pc [OPTIONS]

OPTIONS:
    -c, --config <PATH>   Path to a JSON config file
    -h, --help            Print this help and exit
    -V, --version         Print version and exit

CONFIG:
    JSON file (default: --config, else $PC_CONFIG):
      {
        "providers": [
          { "id": "telegram", "config": { "token": "123:abc" } },
          { "id": "demo" }
        ]
      }
    Environment fallback (no file given):
      PC_PROVIDERS=demo,telegram
      PC_TELEGRAM_TOKEN=123:abc        # per-provider token
      PC_TELEGRAM_CONFIG={"base_url":"..."}   # optional extra JSON (merged)

    Provider ids: "demo" (built-in echo provider), "telegram", "discord"
    (compile-time gated behind the cargo features `telegram` / `discord`).

PROTOCOL (stdout, one JSON document per line):
    Requests:  initialize, capabilities, listen, send, shutdown
    Notifications: event.message, event.draft, event.choice, event.error
"#;

enum Action {
    Run(Option<String>),
    Help,
    Version,
}

struct Cli {
    action: Action,
}

fn main() -> ExitCode {
    match parse_args() {
        Err(message) => {
            eprintln!("pc: {message}");
            eprintln!();
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
        Ok(cli) => match cli.action {
            Action::Help => {
                println!("{USAGE}");
                ExitCode::SUCCESS
            }
            Action::Version => {
                println!("pc {}", env!("CARGO_PKG_VERSION"));
                ExitCode::SUCCESS
            }
            Action::Run(config_path) => run(config_path),
        },
    }
}

fn parse_args() -> Result<Cli, String> {
    let mut args = std::env::args().skip(1);
    let mut config_path: Option<String> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                return Ok(Cli {
                    action: Action::Help,
                })
            }
            "-V" | "--version" => {
                return Ok(Cli {
                    action: Action::Version,
                })
            }
            "-c" | "--config" => {
                let path = args
                    .next()
                    .ok_or_else(|| format!("{arg} requires a path argument"))?;
                config_path = Some(path);
            }
            other if other.starts_with("--config=") => {
                config_path = Some(other["--config=".len()..].to_string());
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(Cli {
        action: Action::Run(config_path),
    })
}

fn run(config_path: Option<String>) -> ExitCode {
    init_tracing();
    let config = match config::load(config_path) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("pc: failed to load config: {e}");
            return ExitCode::FAILURE;
        }
    };

    let (mut state, notify_tx) = AppState::new("stdio");
    let events = state.events();
    let mut failures: Vec<String> = Vec::new();
    for provider in &config.providers {
        match build_provider(&provider.id, &provider.config, events.clone()) {
            Ok(boxed) => {
                if let Err(e) = state.registry_mut().register(boxed) {
                    failures.push(format!("{}: {e}", provider.id));
                }
            }
            Err(e) => failures.push(format!("{}: {e}", provider.id)),
        }
    }
    if !failures.is_empty() {
        eprintln!(
            "pc: failed to load {} provider(s):\n  {}",
            failures.len(),
            failures.join("\n  ")
        );
        return ExitCode::FAILURE;
    }
    tracing::info!(providers = ?state.registry().ids(), "providers registered");

    // The providers hold their own clones of the event sink; drop our local
    // handle so the broadcast channel can close on shutdown (otherwise the
    // transport writer task never observes Close and the process hangs).
    drop(events);

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
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

/// Feature-gated provider construction. Each branch depends only on
/// `provider-core` types plus the provider crate's own constructor.
///
/// Constructor contract (matches provider-telegram / provider-discord):
/// `Provider::new(token: String, events: Arc<dyn ProviderEvents>)`.
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
            let token = config_token(id, config)?;
            Ok(Box::new(provider_telegram::TelegramProvider::new(
                token, events,
            )))
        }
        #[cfg(feature = "discord")]
        "discord" => {
            let token = config_token(id, config)?;
            Ok(Box::new(provider_discord::DiscordProvider::new(
                token, events,
            )))
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

/// Provider ids compiled into this binary.
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
