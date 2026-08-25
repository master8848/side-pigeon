//! End-to-end test: spawn the actual `pc` sidecar binary and drive it over
//! stdio JSON-RPC (the same contract a host process sees).
//!
//! `CARGO_BIN_EXE_pc` is set by cargo for integration tests in the same
//! package; `--features telegram,discord` are NOT enabled here so the demo
//! provider (default feature) is what gets exercised.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

/// Send one JSON-RPC request line, then read lines until the response with
/// the matching id arrives (skipping `event.*` notifications that providers
/// can emit before/during the response).
fn rpc(
    child: &mut std::process::Child,
    writer: &mut impl Write,
    reader: &mut impl BufRead,
    id: u64,
    method: &str,
    params: Option<&serde_json::Value>,
) -> serde_json::Value {
    let mut req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
    });
    if let Some(p) = params {
        req["params"] = p.clone();
    }
    writeln!(writer, "{req}").expect("write request");
    writer.flush().expect("flush request");
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).expect("read response");
        let value: serde_json::Value = serde_json::from_str(&line).expect("parse response");
        if value.get("id") == Some(&serde_json::json!(id)) {
            return value;
        }
        // notification (event.*) — skip
        assert_eq!(
            value["method"].as_str().map(|m| m.starts_with("event.")),
            Some(true)
        );
    }
}

#[test]
fn e2e_stdio_initialize_listen_send_shutdown() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_pc"))
        .env("PC_PROVIDERS", "demo")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn pc");

    let stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let mut writer = stdin;
    let mut reader = BufReader::new(stdout);

    // initialize
    let res = rpc(&mut child, &mut writer, &mut reader, 1, "initialize", None);
    assert_eq!(res["jsonrpc"], "2.0");
    let caps = &res["result"];
    assert_eq!(
        caps["protocolVersion"].as_str().unwrap(),
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(caps["methods"][0], "initialize");
    assert_eq!(caps["transport"][0], "stdio");

    // capabilities must be honest: features advertised must exist.
    assert_eq!(caps["features"], serde_json::json!(["send"]));
    assert_eq!(
        caps["notifications"],
        serde_json::json!(["event.message", "event.error"])
    );
    assert_eq!(caps["providers"][0], "demo");

    // listen (starts the demo provider)
    let res = rpc(&mut child, &mut writer, &mut reader, 2, "listen", None);
    assert_eq!(res["result"]["started"][0], "demo");

    // send through the demo provider (echoes back as event.message)
    let params = serde_json::json!({
        "provider": "demo",
        "message": { "channel_id": "c1", "text": "hello e2e" }
    });
    let res = rpc(
        &mut child,
        &mut writer,
        &mut reader,
        3,
        "send",
        Some(&params),
    );
    let receipt_id = res["result"]["message_id"].as_str().unwrap().to_string();
    assert!(receipt_id.starts_with("demo-"), "receipt id: {receipt_id}");

    // the echo event.message notification arrives after the response
    let mut line = String::new();
    reader.read_line(&mut line).expect("read notification");
    let notif: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(notif["method"], "event.message");
    assert_eq!(notif["params"]["content"][0]["Text"], "echo: hello e2e");
    assert_eq!(notif["params"]["id"], receipt_id);

    // shutdown
    let res = rpc(&mut child, &mut writer, &mut reader, 4, "shutdown", None);
    assert_eq!(res["result"], serde_json::Value::Null);

    let status = child.wait().expect("wait pc");
    assert!(status.success(), "pc exited with {status}");
}

#[test]
fn e2e_stdio_unknown_method_and_bad_params() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_pc"))
        .env("PC_PROVIDERS", "demo")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn pc");
    let stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let mut writer = stdin;
    let mut reader = BufReader::new(stdout);

    let res = rpc(
        &mut child,
        &mut writer,
        &mut reader,
        1,
        "no_such_method",
        None,
    );
    assert_eq!(res["error"]["code"], -32601);

    // unknown provider -> protocol error (-32004), not a params error
    let res = rpc(
        &mut child,
        &mut writer,
        &mut reader,
        2,
        "send",
        Some(&serde_json::json!({ "provider": "nope", "message": {} })),
    );
    assert_eq!(res["error"]["code"], -32004);
    assert_eq!(res["error"]["data"]["kind"], "Protocol");

    let res = rpc(&mut child, &mut writer, &mut reader, 3, "shutdown", None);
    assert_eq!(res["result"], serde_json::Value::Null);
    let status = child.wait().expect("wait pc");
    assert!(status.success());
}
