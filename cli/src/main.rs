//! `pc-connect` — the thin connection CLI for provider-connect.
//!
//! One self-contained binary that EMBEDS the same provider logic as the `pc`
//! sidecar (via `provider-core` + `provider-transport` + the feature-gated
//! provider crates) and exposes the three operations agents need:
//!
//! ```text
//! pc-connect send --provider <id> --chat <chat-id> [--text <text> | --text-file -]
//! pc-connect listen [--providers a,b] [--timeout <secs>] [--once]
//! pc-connect check [--provider <id>]
//! ```
//!
//! Config contract is identical to `pc`: `PC_PROVIDERS`, `PC_<ID>_TOKEN`,
//! `PC_<ID>_CONFIG`, `PC_CONFIG` / `--config`. Logs go to stderr; stdout is
//! reserved for the JSON output (receipts, event lines, check report).

mod demo;
mod ops;
mod providers;

use std::io::Read;
use std::process::ExitCode;

use ops::{CheckOptions, CliError, ListenOptions, SendOptions};

const USAGE: &str = r#"pc-connect — thin connection CLI for provider-connect

USAGE:
    pc-connect send --provider <id> --chat <chat-id> [--text <text> | --text-file <path|- >] [--json]
    pc-connect listen [--providers <a,b>] [--timeout <secs>] [--once] [--json]
    pc-connect check [--provider <id>] [--json]
    pc-connect -h | --help
    pc-connect -V | --version

COMMANDS:
    send     Send one message. Prints the SendReceipt JSON {"message_id", "ts"}
             on stdout; exits 0 on success, non-zero with {"error": {...}} on
             failure.
    listen   Start providers and stream one JSON object per line to stdout:
             {"event":"message","message":{...}} and
             {"event":"error","error":{...}}. Exits after --timeout, after the
             first event with --once, or on Ctrl-C.
    check    Connectivity check: initialize + capabilities + a listen smoke
             per provider. Exit 0 when every checked provider is healthy, 1
             otherwise.

OPTIONS:
    --provider <id>     Provider id: demo (built-in), telegram, discord
                        (feature-gated)
    --chat <chat-id>    Chat/room id to deliver to (send only)
    --text <text>       Message text (send only; mutually exclusive with
                        --text-file)
    --text-file <path>  Read message text from a file; use "-" for stdin
                        (send only). A single trailing newline is stripped.
    --providers <a,b>   Comma-separated provider ids to start (listen only)
    --timeout <secs>    Exit after N seconds even if no event arrived
                        (listen only)
    --once              Exit after the first event (listen only)
    --json              Machine-readable JSON output (the default output
                        format for send/listen; forces it for check)
    -c, --config <path> JSON config file (same shape as `pc`)
    -h, --help          Print this help and exit
    -V, --version       Print version and exit

CONFIG (same env contract as `pc`; see cli/README.md):
    PC_PROVIDERS=demo,telegram
    PC_TELEGRAM_TOKEN=123:abc              # per-provider token
    PC_TELEGRAM_CONFIG={"base_url":"..."}  # optional extra JSON (merged)

PROVIDER DATA-LOSS WARNING:
    pc-connect is NOT a continuous background service. Receiving only works
    while `listen` runs; messages sent while it is stopped are LOST (per
    provider: telegram long-poll, discord gateway, demo local-only). For
    reliable receiving run the `pc` sidecar, or pc-connect listen in a
    supervised loop. See cli/README.md for the full matrix.
"#;

/// Parsed command line.
#[derive(Debug, PartialEq)]
enum Action {
    Help,
    Version,
    Send(SendArgs),
    Listen(ListenArgs),
    Check(CheckArgs),
}

#[derive(Debug, PartialEq)]
struct SendArgs {
    provider: String,
    chat: String,
    text: Option<String>,
    text_file: Option<String>,
    json: bool,
    config: Option<String>,
}

#[derive(Debug, PartialEq)]
struct ListenArgs {
    providers: Option<Vec<String>>,
    timeout_secs: Option<u64>,
    once: bool,
    json: bool,
    config: Option<String>,
}

#[derive(Debug, PartialEq)]
struct CheckArgs {
    provider: Option<String>,
    json: bool,
    config: Option<String>,
}

fn which_pc() -> Result<std::path::PathBuf, String> {
    let path = std::env::var_os("PATH").ok_or("no PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join("pc");
        if candidate.is_file() {
            return Ok(candidate);
        }
        // Windows extension
        let candidate_exe = dir.join("pc.exe");
        if candidate_exe.is_file() {
            return Ok(candidate_exe);
        }
    }
    Err("pc not found on PATH".into())
}

fn main() -> ExitCode {
    // Shim for Phase 04: pc-connect is now `pc send/listen/check`. Keep
    // this binary working for one release but nudge users to the single
    // `pc` binary. If `pc` is on PATH we delegate to it (best-effort).
    if std::env::var_os("PC_CONNECT_QUIET_DEPRECATION").is_none() {
        eprintln!(
            "pc-connect: deprecated — use `pc send` / `pc listen` / `pc check` instead (single `pc` binary, Phase 04); pc-connect will be removed in a future release"
        );
    }
    if let Ok(pc_bin) = which_pc() {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if matches!(
            args.first().map(|s| s.as_str()),
            Some("send") | Some("listen") | Some("check")
        ) {
            if let Ok(status) = std::process::Command::new(&pc_bin).args(&args).status() {
                return ExitCode::from(status.code().unwrap_or(1) as u8);
            }
        }
    }
    match parse_args(std::env::args().skip(1).collect()) {
        Err(message) => {
            eprintln!("pc-connect: {message}");
            eprintln!();
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
        Ok(Action::Help) => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Ok(Action::Version) => {
            println!("pc-connect {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Ok(Action::Send(args)) => dispatch_send(args),
        Ok(Action::Listen(args)) => dispatch_listen(args),
        Ok(Action::Check(args)) => dispatch_check(args),
    }
}

// ---------------------------------------------------------------------------
// Argument parsing (pure; unit-tested below)
// ---------------------------------------------------------------------------

fn parse_args(args: Vec<String>) -> Result<Action, String> {
    let mut iter = args.iter().peekable();
    // Global flags before the subcommand.
    if let Some(arg) = iter.peek() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Action::Help),
            "-V" | "--version" => return Ok(Action::Version),
            _ if arg.starts_with('-') => {
                return Err(format!(
                    "unknown argument: {arg} (expected a subcommand first)"
                ));
            }
            _ => {}
        }
    }
    let command = iter
        .next()
        .ok_or_else(|| "missing subcommand (expected send, listen, or check)".to_string())?;
    match command.as_str() {
        "send" => parse_send(iter.cloned().collect()),
        "listen" => parse_listen(iter.cloned().collect()),
        "check" => parse_check(iter.cloned().collect()),
        "-h" | "--help" => Ok(Action::Help),
        "-V" | "--version" => Ok(Action::Version),
        other => Err(format!(
            "unknown subcommand: {other} (expected send, listen, or check)"
        )),
    }
}

fn parse_send(mut args: Vec<String>) -> Result<Action, String> {
    let mut out = SendArgs {
        provider: String::new(),
        chat: String::new(),
        text: None,
        text_file: None,
        json: false,
        config: None,
    };
    while !args.is_empty() {
        let arg = args[0].clone();
        if matches!(arg.as_str(), "-h" | "--help") {
            return Ok(Action::Help);
        }
        match arg.as_str() {
            "--provider" | "--chat" | "--text" | "--text-file" => {
                let flag = args.remove(0);
                let value = args
                    .first()
                    .cloned()
                    .ok_or_else(|| format!("{flag} requires a value"))?;
                args.remove(0);
                match flag.as_str() {
                    "--provider" => out.provider = value,
                    "--chat" => out.chat = value,
                    "--text" => out.text = Some(value),
                    "--text-file" => out.text_file = Some(value),
                    _ => unreachable!(),
                }
            }
            "--json" => {
                args.remove(0);
                out.json = true;
            }
            "--config" | "-c" => {
                args.remove(0);
                let value = args
                    .first()
                    .cloned()
                    .ok_or_else(|| "-c/--config requires a value".to_string())?;
                args.remove(0);
                out.config = Some(value);
            }
            other if other.starts_with("--provider=") => {
                out.provider = other["--provider=".len()..].to_string();
                args.remove(0);
            }
            other if other.starts_with("--chat=") => {
                out.chat = other["--chat=".len()..].to_string();
                args.remove(0);
            }
            other if other.starts_with("--text=") => {
                out.text = Some(other["--text=".len()..].to_string());
                args.remove(0);
            }
            other if other.starts_with("--text-file=") => {
                out.text_file = Some(other["--text-file=".len()..].to_string());
                args.remove(0);
            }
            other if other.starts_with("--config=") => {
                out.config = Some(other["--config=".len()..].to_string());
                args.remove(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if out.provider.is_empty() {
        return Err("send requires --provider <id>".to_string());
    }
    if out.chat.is_empty() {
        return Err("send requires --chat <chat-id>".to_string());
    }
    match (&out.text, &out.text_file) {
        (Some(_), Some(_)) => Err("--text and --text-file are mutually exclusive".to_string()),
        (None, None) => Err("send requires --text <text> or --text-file <path|- >".to_string()),
        _ => Ok(Action::Send(out)),
    }
}

fn parse_listen(mut args: Vec<String>) -> Result<Action, String> {
    let mut out = ListenArgs {
        providers: None,
        timeout_secs: None,
        once: false,
        json: false,
        config: None,
    };
    while !args.is_empty() {
        let arg = args[0].clone();
        if matches!(arg.as_str(), "-h" | "--help") {
            return Ok(Action::Help);
        }
        match arg.as_str() {
            "--providers" | "--timeout" => {
                let flag = args.remove(0);
                let value = args
                    .first()
                    .cloned()
                    .ok_or_else(|| format!("{flag} requires a value"))?;
                args.remove(0);
                match flag.as_str() {
                    "--providers" => {
                        let ids: Vec<String> = value
                            .split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect();
                        if ids.is_empty() {
                            return Err(
                                "--providers requires a non-empty comma-separated list".to_string()
                            );
                        }
                        out.providers = Some(ids);
                    }
                    "--timeout" => {
                        out.timeout_secs = Some(value.parse::<u64>().map_err(|_| {
                            format!("--timeout must be a non-negative integer of seconds, got {value:?}")
                        })?);
                    }
                    _ => unreachable!(),
                }
            }
            "--once" => {
                args.remove(0);
                out.once = true;
            }
            "--json" => {
                args.remove(0);
                out.json = true;
            }
            "--config" | "-c" => {
                args.remove(0);
                let value = args
                    .first()
                    .cloned()
                    .ok_or_else(|| "-c/--config requires a value".to_string())?;
                args.remove(0);
                out.config = Some(value);
            }
            other if other.starts_with("--providers=") => {
                let value = other["--providers=".len()..].to_string();
                let ids: Vec<String> = value
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                if ids.is_empty() {
                    return Err("--providers requires a non-empty comma-separated list".to_string());
                }
                out.providers = Some(ids);
                args.remove(0);
            }
            other if other.starts_with("--timeout=") => {
                let value = other["--timeout=".len()..].to_string();
                out.timeout_secs = Some(value.parse::<u64>().map_err(|_| {
                    format!("--timeout must be a non-negative integer of seconds, got {value:?}")
                })?);
                args.remove(0);
            }
            other if other.starts_with("--config=") => {
                out.config = Some(other["--config=".len()..].to_string());
                args.remove(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(Action::Listen(out))
}

fn parse_check(mut args: Vec<String>) -> Result<Action, String> {
    let mut out = CheckArgs {
        provider: None,
        json: false,
        config: None,
    };
    while !args.is_empty() {
        let arg = args[0].clone();
        if matches!(arg.as_str(), "-h" | "--help") {
            return Ok(Action::Help);
        }
        match arg.as_str() {
            "--provider" => {
                args.remove(0);
                let value = args
                    .first()
                    .cloned()
                    .ok_or_else(|| "--provider requires a value".to_string())?;
                args.remove(0);
                out.provider = Some(value);
            }
            "--json" => {
                args.remove(0);
                out.json = true;
            }
            "--config" | "-c" => {
                args.remove(0);
                let value = args
                    .first()
                    .cloned()
                    .ok_or_else(|| "-c/--config requires a value".to_string())?;
                args.remove(0);
                out.config = Some(value);
            }
            other if other.starts_with("--provider=") => {
                out.provider = Some(other["--provider=".len()..].to_string());
                args.remove(0);
            }
            other if other.starts_with("--config=") => {
                out.config = Some(other["--config=".len()..].to_string());
                args.remove(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(Action::Check(out))
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// One shared current-thread tokio runtime (like `pc`); enables timers + io
/// for provider long-poll/gateway tasks.
fn runtime() -> Result<tokio::runtime::Runtime, CliError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| CliError::internal(format!("failed to build tokio runtime: {e}")))
}

fn init_tracing() {
    // Logs go to stderr; stdout is reserved for the JSON output. Default
    // level is `warn` (quiet CLI); raise with RUST_LOG=debug|info|trace.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

/// Print the contract error JSON on stdout + a human line on stderr.
fn fail(err: &CliError) -> ExitCode {
    println!("{}", serde_json::json!({ "error": &err.0 }));
    eprintln!("pc-connect: {err}");
    ExitCode::FAILURE
}

fn dispatch_send(args: SendArgs) -> ExitCode {
    init_tracing();
    let config = match provider_config::load(args.config.clone()) {
        Ok(c) => c,
        Err(e) => return fail(&CliError::config(format!("failed to load config: {e}"))),
    };
    // Read the text payload before doing any work.
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
    match rt.block_on(ops::send(opts, config)) {
        Ok(receipt) => {
            match serde_json::to_string(&receipt) {
                Ok(line) => println!("{line}"),
                Err(e) => return fail(&CliError::internal(format!("serialize receipt: {e}"))),
            }
            ExitCode::SUCCESS
        }
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
        timeout: args.timeout_secs.map(std::time::Duration::from_secs),
        once: args.once,
    };
    let rt = match runtime() {
        Ok(rt) => rt,
        Err(e) => return fail(&e),
    };
    match rt.block_on(ops::listen(opts, config)) {
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
    match rt.block_on(ops::check(opts, config)) {
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
                    "pc-connect: check: protocol {} (methods: {})",
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
                    println!(
                        "pc-connect: check: {}: {verdict} — {}",
                        r.provider, r.detail
                    );
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

/// Resolve the text payload: `--text` wins; `--text-file -` reads stdin;
/// `--text-file <path>` reads a file. A single trailing newline is stripped.
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
                std::fs::read_to_string(path).map_err(|e| {
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

/// Strip one trailing newline (and a preceding carriage return) — the
/// common case for `echo "hi" | pc-connect send --text-file -`.
fn strip_trailing_newline(mut s: String) -> String {
    if s.ends_with('\n') {
        s.pop();
        if s.ends_with('\r') {
            s.pop();
        }
    }
    s
}

// ---------------------------------------------------------------------------
// Unit tests: argument parsing + text reading
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn send_parses_all_flags() {
        let action = parse_args(args(&[
            "send",
            "--provider",
            "demo",
            "--chat",
            "room-1",
            "--text",
            "hello",
            "--json",
        ]))
        .unwrap();
        assert_eq!(
            action,
            Action::Send(SendArgs {
                provider: "demo".into(),
                chat: "room-1".into(),
                text: Some("hello".into()),
                text_file: None,
                json: true,
                config: None,
            })
        );
    }

    #[test]
    fn send_parses_equals_forms_and_text_file() {
        let action = parse_args(args(&[
            "send",
            "--provider=demo",
            "--chat=room-1",
            "--text-file",
            "-",
        ]))
        .unwrap();
        match action {
            Action::Send(a) => {
                assert_eq!(a.provider, "demo");
                assert_eq!(a.chat, "room-1");
                assert_eq!(a.text_file.as_deref(), Some("-"));
            }
            other => panic!("unexpected action: {other:?}"),
        }
    }

    #[test]
    fn send_requires_provider_chat_and_text() {
        assert!(parse_args(args(&["send"])).is_err());
        assert!(parse_args(args(&["send", "--provider", "demo"])).is_err());
        assert!(parse_args(args(&["send", "--provider", "demo", "--chat", "r"])).is_err());
        assert!(parse_args(args(&[
            "send",
            "--provider",
            "demo",
            "--chat",
            "r",
            "--text",
            "a",
            "--text-file",
            "-",
        ]))
        .is_err());
        assert!(parse_args(args(&[
            "send",
            "--provider",
            "demo",
            "--chat",
            "r",
            "--text",
            "a",
        ]))
        .is_ok());
    }

    #[test]
    fn listen_parses_providers_timeout_once() {
        let action = parse_args(args(&[
            "listen",
            "--providers",
            "demo, telegram",
            "--timeout",
            "5",
            "--once",
        ]))
        .unwrap();
        match action {
            Action::Listen(a) => {
                assert_eq!(a.providers, Some(vec!["demo".into(), "telegram".into()]));
                assert_eq!(a.timeout_secs, Some(5));
                assert!(a.once);
            }
            other => panic!("unexpected action: {other:?}"),
        }
    }

    #[test]
    fn listen_rejects_bad_timeout_and_empty_providers() {
        assert!(parse_args(args(&["listen", "--timeout", "abc"])).is_err());
        assert!(parse_args(args(&["listen", "--providers", ""])).is_err());
    }

    #[test]
    fn check_parses_provider_and_json() {
        let action = parse_args(args(&["check", "--provider", "telegram", "--json"])).unwrap();
        assert_eq!(
            action,
            Action::Check(CheckArgs {
                provider: Some("telegram".into()),
                json: true,
                config: None,
            })
        );
    }

    #[test]
    fn config_flag_forms_are_accepted() {
        assert!(parse_args(args(&[
            "send",
            "--provider",
            "demo",
            "--chat",
            "r",
            "--text",
            "t",
            "--config",
            "x.json"
        ]))
        .is_ok());
        assert!(parse_args(args(&[
            "send",
            "--provider",
            "demo",
            "--chat",
            "r",
            "--text",
            "t",
            "--config=x.json"
        ]))
        .is_ok());
    }

    #[test]
    fn unknown_subcommand_and_flags_are_rejected() {
        assert!(parse_args(args(&["frobnicate"])).is_err());
        assert!(parse_args(args(&[
            "send",
            "--provider",
            "demo",
            "--chat",
            "r",
            "--text",
            "t",
            "--bogus"
        ]))
        .is_err());
        assert!(parse_args(args(&["--bogus"])).is_err());
    }

    #[test]
    fn help_and_version_short_circuit() {
        assert_eq!(parse_args(args(&["--help"])).unwrap(), Action::Help);
        assert_eq!(parse_args(args(&["send", "--help"])).unwrap(), Action::Help);
        assert_eq!(parse_args(args(&["-V"])).unwrap(), Action::Version);
    }

    #[test]
    fn strip_one_trailing_newline_only() {
        assert_eq!(
            strip_trailing_newline(
                "hi
"
                .into()
            ),
            "hi"
        );
        assert_eq!(
            strip_trailing_newline(
                "hi
"
                .into()
            ),
            "hi"
        );
        assert_eq!(
            strip_trailing_newline(
                "hi

"
                .into()
            ),
            "hi
"
        );
        assert_eq!(strip_trailing_newline("hi".into()), "hi");
    }
}
