//! WebSocket transport tests (feature "ws"): JSON-RPC over tokio-tungstenite.

#![cfg(feature = "ws")]

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use provider_transport::state::AppState;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;

async fn spawn_ws_server() -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (state, notify_tx) = AppState::new("ws");
    let events = state.events();
    // No provider registered: just test the method surface over WS.
    drop(events);
    let state = Arc::new(Mutex::new(state));
    let join = tokio::spawn(async move {
        let _ = provider_transport::ws::serve_ws(state, notify_tx, listener).await;
    });
    (format!("ws://{addr}"), join)
}

#[tokio::test]
async fn ws_initialize_and_shutdown() {
    let (url, join) = spawn_ws_server().await;
    let (mut ws, _resp) = tokio_tungstenite::connect_async(&url).await.unwrap();

    ws.send(Message::Text(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#.into(),
    ))
    .await
    .unwrap();
    let text = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("timeout")
        .unwrap()
        .unwrap();
    let resp: serde_json::Value = serde_json::from_str(text.to_text().unwrap()).unwrap();
    assert_eq!(resp["id"], 1);
    assert_eq!(resp["result"]["protocolVersion"], env!("CARGO_PKG_VERSION"));
    assert_eq!(resp["result"]["transport"], serde_json::json!(["ws"]));

    ws.send(Message::Text(
        r#"{"jsonrpc":"2.0","id":2,"method":"shutdown","params":{}}"#.into(),
    ))
    .await
    .unwrap();
    let text = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("timeout")
        .unwrap()
        .unwrap();
    let resp: serde_json::Value = serde_json::from_str(text.to_text().unwrap()).unwrap();
    assert_eq!(resp["id"], 2);
    assert!(resp["result"].is_null());
    join.abort();
}
