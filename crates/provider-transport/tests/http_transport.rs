//! HTTP transport tests (feature "http"): JSON-RPC over a minimal hyper server.

#![cfg(feature = "http")]

use std::sync::Arc;
use std::time::Duration;

use provider_transport::state::AppState;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

async fn spawn_http_server() -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (state, _notify_tx) = AppState::new("http");
    let state = Arc::new(Mutex::new(state));
    let join = tokio::spawn(async move {
        let _ = provider_transport::http::serve_http(state, listener).await;
    });
    (addr, join)
}

async fn rpc_call(addr: std::net::SocketAddr, body: &str) -> String {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let request = format!(
        "POST /rpc HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut buf = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut buf))
        .await
        .unwrap();
    String::from_utf8_lossy(&buf).into_owned()
}

#[tokio::test]
async fn http_initialize_and_send_flow() {
    let (addr, join) = spawn_http_server().await;

    let raw = rpc_call(
        addr,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
    )
    .await;
    let body = raw.split("\r\n\r\n").nth(1).unwrap();
    let resp: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(resp["id"], 1);
    assert_eq!(resp["result"]["transport"], serde_json::json!(["http"]));

    // error path over http
    let raw = rpc_call(addr, r#"{"jsonrpc":"2.0","id":2,"method":"bogus"}"#).await;
    let body = raw.split("\r\n\r\n").nth(1).unwrap();
    let resp: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(resp["error"]["code"], -32601);

    // method not allowed (GET)
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(b"GET /rpc HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut buf = Vec::new();
    let _ = stream.read_to_end(&mut buf).await.unwrap();
    let raw = String::from_utf8_lossy(&buf).into_owned();
    assert!(raw.starts_with("HTTP/1.1 405"), "got: {raw}");
    join.abort();
}
