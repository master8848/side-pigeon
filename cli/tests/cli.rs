//! End-to-end tests for pc-connect: spawn the real binary and exercise the
//! contract surface (send / listen / check) against the built-in `demo`
//! provider. The demo provider needs no network.
//!
//! NOTE on round-trips: each pc-connect invocation embeds its own providers
//! in-process, so a `send` in one process cannot be observed by a `listen` in
//! another via the demo provider (the demo echo stays inside the sending
//! process). The send→listen round-trip is therefore covered in-process by
//! the unit tests in src/ops.rs; real providers (telegram/discord) deliver
//! cross-process through the platform.
//!
//! `CARGO_BIN_EXE_pc-connect` is set by cargo for integration tests of the
//! same package; only the default feature set (demo) is exercised.

use std::io::Write;
use std::process::{Command, Stdio};

fn bin() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_pc-connect"));
    // Deterministic config: only the demo provider, no inherited PC_* noise.
    cmd.env("PC_PROVIDERS", "demo")
        .env_remove("PC_CONFIG")
        .env_remove("PC_DEMO_CONFIG");
    cmd
}

/// Run pc-connect, capture stdout/stderr/exit code.
fn run(args: &[&str], stdin: Option<&str>) -> (String, String, i32) {
    let mut cmd = bin();
    cmd.args(args);
    let mut child = cmd
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn pc-connect");
    if let Some(input) = stdin {
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(input.as_bytes())
            .expect("write stdin");
    }
    let out = child.wait_with_output().expect("wait for pc-connect");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.code().unwrap_or(-1),
    )
}

#[test]
fn send_prints_receipt_json_and_exits_zero() {
    let (stdout, stderr, code) = run(
        &[
            "send",
            "--provider",
            "demo",
            "--chat",
            "room-1",
            "--text",
            "hello cli",
        ],
        None,
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    let receipt: serde_json::Value = serde_json::from_str(stdout.trim()).expect("receipt JSON");
    assert!(receipt["message_id"].is_string());
    assert!(receipt["ts"].is_i64());
    assert!(receipt.get("error").is_none());
}

#[test]
fn send_reads_text_from_stdin_via_text_file_dash() {
    let (stdout, stderr, code) = run(
        &[
            "send",
            "--provider",
            "demo",
            "--chat",
            "room-1",
            "--text-file",
            "-",
        ],
        Some("piped body\n"),
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    let receipt: serde_json::Value = serde_json::from_str(stdout.trim()).expect("receipt JSON");
    assert!(receipt["message_id"].is_string());
}

#[test]
fn send_with_unknown_provider_fails_with_error_json() {
    let (stdout, _stderr, code) = run(
        &["send", "--provider", "nope", "--chat", "r", "--text", "t"],
        None,
    );
    assert_ne!(code, 0);
    let err: serde_json::Value = serde_json::from_str(stdout.trim()).expect("error JSON");
    assert_eq!(err["error"]["code"], -32004); // protocol error
    assert!(err["error"]["message"].as_str().unwrap().contains("nope"));
}

#[test]
fn send_without_text_is_a_usage_error() {
    let (_stdout, stderr, code) = run(&["send", "--provider", "demo", "--chat", "r"], None);
    assert_eq!(code, 2);
    assert!(stderr.contains("--text"));
}

#[test]
fn check_demo_exits_zero() {
    let (stdout, stderr, code) = run(&["check"], None);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("demo: OK"), "stdout: {stdout}");
    assert!(stdout.contains("protocol"), "stdout: {stdout}");
}

#[test]
fn check_json_reports_ok() {
    let (stdout, stderr, code) = run(&["check", "--json"], None);
    assert_eq!(code, 0, "stderr: {stderr}");
    let report: serde_json::Value = serde_json::from_str(stdout.trim()).expect("report JSON");
    assert_eq!(report["ok"], true);
    assert_eq!(report["providers"][0]["provider"], "demo");
    assert_eq!(report["providers"][0]["ok"], true);
    assert!(report["protocolVersion"].is_string());
}

#[test]
fn check_unknown_provider_fails() {
    let (stdout, _stderr, code) = run(&["check", "--provider", "nope"], None);
    assert_ne!(code, 0);
    let err: serde_json::Value = serde_json::from_str(stdout.trim()).expect("error JSON");
    assert!(err["error"]["code"].is_i64());
}

#[test]
fn listen_once_receives_demo_announcement() {
    let (stdout, stderr, code) = run(
        &["listen", "--providers", "demo", "--once", "--timeout", "10"],
        None,
    );
    assert_eq!(code, 0, "stderr: {stderr}");
    let line = stdout.lines().next().expect("one event line");
    let event: serde_json::Value = serde_json::from_str(line).expect("event JSON");
    assert_eq!(event["event"], "message");
    assert_eq!(event["message"]["channel"], "demo");
    assert!(event["message"]["content"][0]["Text"].as_str().is_some());
}

#[test]
fn listen_without_configured_providers_fails() {
    // Nothing configured is a misconfiguration, not a silent no-op.
    let mut cmd = bin();
    cmd.env("PC_PROVIDERS", "")
        .args(["listen", "--timeout", "1"]);
    let out = cmd.output().expect("run listen");
    assert_ne!(out.status.code(), Some(0));
    let err: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("error JSON");
    assert!(err["error"]["message"]
        .as_str()
        .unwrap()
        .contains("no providers"));
}

#[test]
fn listen_unknown_provider_fails_with_error_json() {
    let (stdout, _stderr, code) = run(&["listen", "--providers", "nope", "--timeout", "1"], None);
    assert_ne!(code, 0);
    let err: serde_json::Value = serde_json::from_str(stdout.trim()).expect("error JSON");
    assert_eq!(err["error"]["code"], -32004);
}

#[test]
fn config_file_is_accepted() {
    let dir = std::env::temp_dir().join(format!("pc-connect-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let path = dir.join("config.json");
    std::fs::write(&path, r#"{"providers":[{"id":"demo"}]}"#).expect("write config");
    let (stdout, stderr, code) = run(&["check", "--config", path.to_str().unwrap()], None);
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(code, 0, "stderr: {stderr}");
    assert!(stdout.contains("demo: OK"), "stdout: {stdout}");
}
