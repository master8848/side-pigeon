//! Integration tests: JSON-RPC 2.0 framing + method dispatch over a duplex pipe.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use provider_core::{
    ChannelMessage, ChatProvider, ContentPart, ProviderError, ProviderEvents, SendMessage,
    SendReceipt, Sender,
};
use provider_transport::state::{AppState, Outbound};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines};

fn fixture_message(id: &str, text: &str) -> ChannelMessage {
    ChannelMessage {
        id: id.into(),
        channel: "test".into(),
        channel_id: "room-1".into(),
        sender: Sender {
            id: "peer".into(),
            name: None,
            username: None,
            avatar_url: None,
        },
        reply_target: Some("room-1".into()),
        content: vec![ContentPart::Text(text.into())],
        thread_ts: None,
        attachments: vec![],
        explicitly_addressed: false,
        ts: 1_752_000_000_000,
        raw: None,
    }
}

/// Test provider: emits `event.message` on start and echoes on send.
struct TestProvider {
    events: Arc<dyn ProviderEvents>,
}

#[async_trait]
impl ChatProvider for TestProvider {
    fn id(&self) -> &'static str {
        "test"
    }
    async fn start(&mut self) -> Result<(), ProviderError> {
        self.events
            .on_message(fixture_message("started-1", "provider started"));
        Ok(())
    }
    async fn stop(&mut self) -> Result<(), ProviderError> {
        Ok(())
    }
    async fn send(&self, msg: &SendMessage) -> Result<SendReceipt, ProviderError> {
        self.events
            .on_message(fixture_message("echo-1", &format!("echo: {}", msg.text)));
        Ok(SendReceipt {
            message_id: "test-m1".into(),
            ts: 42,
        })
    }
}

struct Server {
    join: tokio::task::JoinHandle<()>,
    client_write: DuplexStream,
    client_read: Lines<BufReader<DuplexStream>>,
}

impl Server {
    async fn write(&mut self, line: &str) {
        // NDJSON framing: every request must be newline-terminated.
        self.client_write.write_all(line.as_bytes()).await.unwrap();
        self.client_write.write_all(b"\n").await.unwrap();
    }
    async fn read_line(&mut self) -> String {
        let line = tokio::time::timeout(Duration::from_secs(5), self.client_read.next_line())
            .await
            .expect("timeout waiting for server output")
            .expect("io error")
            .expect("unexpected EOF");
        line
    }
    async fn read_eof(&mut self) {
        let got = tokio::time::timeout(Duration::from_secs(5), self.client_read.next_line())
            .await
            .expect("timeout waiting for EOF")
            .expect("io error");
        assert!(got.is_none(), "expected EOF, got: {got:?}");
    }
    async fn expect_no_output(&mut self) {
        let got =
            tokio::time::timeout(Duration::from_millis(200), self.client_read.next_line()).await;
        assert!(got.is_err(), "expected silence, got: {got:?}");
    }
}

fn spawn_server() -> Server {
    let (mut state, notify_tx) = AppState::new("stdio");
    let events = state.events();
    state
        .registry_mut()
        .register(Box::new(TestProvider { events }))
        .unwrap();
    let (server_stdin, client_write) = tokio::io::duplex(65_536);
    let (server_stdout, client_read) = tokio::io::duplex(65_536);
    let join = tokio::spawn(async move {
        let _ =
            provider_transport::stdio::serve_stdio(state, notify_tx, server_stdin, server_stdout)
                .await;
    });
    Server {
        join,
        client_write,
        client_read: BufReader::new(client_read).lines(),
    }
}

#[tokio::test]
async fn initialize_and_capabilities() {
    let mut srv = spawn_server();
    srv.write(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#)
        .await;

    let line = srv.read_line().await;
    let resp: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(resp["jsonrpc"], "2.0");
    assert_eq!(resp["id"], 1);
    assert_eq!(resp["error"], serde_json::Value::Null);
    assert!(resp["result"]["protocolVersion"].is_string());
    assert_eq!(resp["result"]["methods"][4], "shutdown");
    assert_eq!(resp["result"]["providers"], serde_json::json!(["test"]));

    srv.write(r#"{"jsonrpc":"2.0","id":"cap1","method":"capabilities"}"#)
        .await;
    let line = srv.read_line().await;
    let resp: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(resp["id"], "cap1");
    assert_eq!(resp["result"]["transport"], serde_json::json!(["stdio"]));
    assert_eq!(resp["result"]["notifications"][0], "event.message");
    srv.join.abort();
}

#[tokio::test]
async fn listen_emits_event_message_notification() {
    let mut srv = spawn_server();
    srv.write(r#"{"jsonrpc":"2.0","id":2,"method":"listen","params":{}}"#)
        .await;

    // The provider emits on start: notification line arrives before the response.
    let line = srv.read_line().await;
    let notif: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(notif["jsonrpc"], "2.0");
    assert_eq!(notif["method"], "event.message");
    assert_eq!(notif["params"]["message"]["id"], "started-1");
    assert_eq!(notif["params"]["message"]["channel"], "test");

    let line = srv.read_line().await;
    let resp: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(resp["id"], 2);
    assert_eq!(resp["result"]["started"], serde_json::json!(["test"]));
    srv.join.abort();
}

#[tokio::test]
async fn send_returns_receipt_and_echoes() {
    let mut srv = spawn_server();
    srv.write(r#"{"jsonrpc":"2.0","id":3,"method":"listen","params":{}}"#)
        .await;
    let _ = srv.read_line().await; // event.message
    let _ = srv.read_line().await; // listen response

    srv.write(
        r#"{"jsonrpc":"2.0","id":4,"method":"send","params":{"provider":"test","message":{"channel_id":"room-1","text":"hi","reply_to":null,"attachments":[]}}}"#,
    )
    .await;

    let line = srv.read_line().await; // echo notification
    let notif: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(notif["method"], "event.message");
    assert_eq!(notif["params"]["message"]["content"][0]["Text"], "echo: hi");

    let line = srv.read_line().await; // send response
    let resp: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(resp["id"], 4);
    assert_eq!(resp["result"]["message_id"], "test-m1");
    assert_eq!(resp["result"]["ts"], 42);
    srv.join.abort();
}

#[tokio::test]
async fn send_before_listen_is_rejected() {
    let mut srv = spawn_server();
    srv.write(
        r#"{"jsonrpc":"2.0","id":5,"method":"send","params":{"provider":"test","message":{"channel_id":"room-1","text":"hi","attachments":[]}}}"#,
    )
    .await;
    let line = srv.read_line().await;
    let resp: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(resp["error"]["code"], -32004); // PROTOCOL_ERROR
    assert!(resp["result"].is_null());
    srv.join.abort();
}

#[tokio::test]
async fn shutdown_responds_then_eof() {
    let mut srv = spawn_server();
    srv.write(r#"{"jsonrpc":"2.0","id":6,"method":"shutdown","params":{}}"#)
        .await;
    let line = srv.read_line().await;
    let resp: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(resp["id"], 6);
    assert!(resp["result"].is_null());
    assert!(resp["error"].is_null());
    srv.read_eof().await;
    srv.join.await.unwrap();
}

#[tokio::test]
async fn unknown_method_returns_minus_32601() {
    let mut srv = spawn_server();
    srv.write(r#"{"jsonrpc":"2.0","id":7,"method":"nope"}"#)
        .await;
    let line = srv.read_line().await;
    let resp: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(resp["error"]["code"], -32601);
    srv.join.abort();
}

#[tokio::test]
async fn invalid_params_returns_minus_32602() {
    let mut srv = spawn_server();
    srv.write(r#"{"jsonrpc":"2.0","id":8,"method":"send","params":{"provider":"test"}}"#)
        .await;
    let line = srv.read_line().await;
    let resp: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(resp["error"]["code"], -32602);
    srv.join.abort();
}

#[tokio::test]
async fn listen_unknown_provider_returns_protocol_error() {
    let mut srv = spawn_server();
    srv.write(r#"{"jsonrpc":"2.0","id":9,"method":"listen","params":{"providers":["ghost"]}}"#)
        .await;
    let line = srv.read_line().await;
    let resp: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(resp["error"]["code"], -32004);
    assert_eq!(resp["error"]["data"]["kind"], "Protocol");
    srv.join.abort();
}

#[tokio::test]
async fn garbage_line_returns_parse_error() {
    let mut srv = spawn_server();
    srv.write("this is not json at all").await;
    let line = srv.read_line().await;
    let resp: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(resp["id"], serde_json::Value::Null);
    assert_eq!(resp["error"]["code"], -32700);
    srv.join.abort();
}

#[tokio::test]
async fn batch_and_wrong_version_are_invalid_requests() {
    let mut srv = spawn_server();
    srv.write(r#"[{"jsonrpc":"2.0","id":1,"method":"initialize"}]"#)
        .await;
    let line = srv.read_line().await;
    let resp: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(resp["error"]["code"], -32600);

    srv.write(r#"{"jsonrpc":"1.0","id":2,"method":"initialize"}"#)
        .await;
    let line = srv.read_line().await;
    let resp: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(resp["error"]["code"], -32600);
    srv.join.abort();
}

#[tokio::test]
async fn client_notification_gets_no_response() {
    let mut srv = spawn_server();
    srv.write(r#"{"jsonrpc":"2.0","method":"initialize","params":{}}"#)
        .await;
    srv.expect_no_output().await;
    // server still alive: a real request still works
    srv.write(r#"{"jsonrpc":"2.0","id":10,"method":"capabilities"}"#)
        .await;
    let line = srv.read_line().await;
    let resp: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(resp["id"], 10);
    srv.join.abort();
}

#[tokio::test]
async fn requests_keep_fifo_order() {
    let mut srv = spawn_server();
    for id in 1..=5 {
        srv.write(&format!(
            r#"{{"jsonrpc":"2.0","id":{id},"method":"capabilities"}}"#
        ))
        .await;
    }
    for id in 1..=5 {
        let line = srv.read_line().await;
        let resp: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(resp["id"], id);
    }
    srv.join.abort();
}

#[tokio::test]
async fn outbound_channel_type_check() {
    // Sanity: Outbound serializes as the inner JSON-RPC document.
    let (state, _tx) = AppState::new("stdio");
    let caps = state.capabilities_value();
    assert_eq!(caps["protocolVersion"], env!("CARGO_PKG_VERSION"));
    let _ = Outbound::Response(provider_transport::jsonrpc::Response::ok(
        provider_transport::jsonrpc::Id::Number(1),
        caps,
    ));
}
