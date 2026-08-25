//! Optional minimal HTTP/1.1 JSON-RPC + REST server (feature `http`, hand-rolled on hyper).
//!
//! Routes:
//! ```text
//! GET  /health                     -> capabilities_value() (k8s / pc check)
//! POST /api/providers/:id/send     -> AppState::send (typed SendMessage JSON)
//! GET  /api/events                 -> SSE subscription to broadcast::Sender<Outbound>
//! POST /rpc                        -> JSON-RPC dispatch (same as stdio/ws handle_request)
//! ```
//! Keeps stdio primary; enabled via `pc serve --http :8788 --ws :8787`.

use std::sync::Arc;

use bytes::Bytes;
use http_body_util::{BodyExt, Full, StreamBody, combinators::BoxBody};
use hyper::body::{Frame, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request as HttpRequest, Response as HttpResponse, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, broadcast};

use crate::error::TransportError;
use crate::jsonrpc::{Id, JsonRpcError};
use crate::jsonrpc::parse_request;
use crate::state::{AppState, DispatchOutcome, Outbound, provider_error};
use crate::state::dropped_frames_notification;

/// Accept HTTP connections on `listener` and serve REST + JSON-RPC.
pub async fn serve_http(
    state: Arc<Mutex<AppState>>,
    listener: TcpListener,
) -> Result<(), TransportError> {
    loop {
        let (stream, peer) = listener.accept().await?;
        let state = state.clone();
        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            let service = service_fn(move |request| {
                let state = state.clone();
                async move { handle_http(state, request).await }
            });
            if let Err(e) = http1::Builder::new().serve_connection(io, service).await {
                tracing::warn!(peer = %peer, error = %e, "http connection error");
            }
        });
    }
}

type BoxBodyType = BoxBody<Bytes, std::convert::Infallible>;

fn json_body(status: StatusCode, value: Value) -> HttpResponse<BoxBodyType> {
    let text = serde_json::to_string(&value).unwrap_or_else(|_| "{}".to_string());
    HttpResponse::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(text)).boxed())
        .expect("valid response")
}

fn text_body(status: StatusCode, text: String) -> HttpResponse<BoxBodyType> {
    HttpResponse::builder()
        .status(status)
        .header("content-type", "text/plain")
        .body(Full::new(Bytes::from(text)).boxed())
        .expect("valid response")
}

async fn handle_http(
    state: Arc<Mutex<AppState>>,
    req: HttpRequest<Incoming>,
) -> Result<HttpResponse<BoxBodyType>, hyper::Error> {
    let path = req.uri().path().to_owned();
    let method = req.method().clone();

    // GET /health
    if method == Method::GET && path == "/health" {
        let caps = {
            let guard = state.lock().await;
            guard.capabilities_value()
        };
        return Ok(json_body(StatusCode::OK, caps));
    }

    // GET /api/events -> SSE
    if method == Method::GET && path == "/api/events" {
        let notify_tx: broadcast::Sender<Outbound> = {
            let guard = state.lock().await;
            guard.notify().clone()
        };
        let rx = notify_tx.subscribe();
        let stream = futures_util::stream::unfold(rx, |mut rx| async move {
            loop {
                match rx.recv().await {
                    Ok(Outbound::Notification(n)) => {
                        let json = serde_json::to_string(&n).unwrap_or_default();
                        let sse = format!("data: {}\n\n", json);
                        return Some((
                            Ok::<Frame<Bytes>, std::convert::Infallible>(Frame::data(Bytes::from(sse))),
                            rx,
                        ));
                    }
                    Ok(Outbound::Response(_)) => continue,
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        let n = dropped_frames_notification(skipped);
                        let json = serde_json::to_string(&n).unwrap_or_default();
                        let sse = format!("event: error\ndata: {}\n\n", json);
                        return Some((Ok(Frame::data(Bytes::from(sse))), rx));
                    }
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        });
        let body = StreamBody::new(stream).boxed();
        let resp = HttpResponse::builder()
            .status(StatusCode::OK)
            .header("content-type", "text/event-stream")
            .header("cache-control", "no-cache")
            .header("connection", "keep-alive")
            .body(body)
            .expect("valid sse response");
        return Ok(resp);
    }

    // POST /api/providers/:id/send
    if method == Method::POST && path.starts_with("/api/providers/") && path.ends_with("/send") {
        let id = path
            .strip_prefix("/api/providers/")
            .and_then(|s| s.strip_suffix("/send"))
            .unwrap_or("");
        if id.is_empty() {
            return Ok(json_body(
                StatusCode::BAD_REQUEST,
                json!({"error": {"code": -32602, "message": "missing provider id"}}),
            ));
        }
        let body_bytes = match req.collect().await {
            Ok(c) => c.to_bytes(),
            Err(e) => {
                return Ok(text_body(
                    StatusCode::BAD_REQUEST,
                    format!("failed to read body: {e}"),
                ));
            }
        };
        let msg: provider_core::SendMessage = match serde_json::from_slice(&body_bytes) {
            Ok(v) => v,
            Err(e) => {
                return Ok(json_body(
                    StatusCode::BAD_REQUEST,
                    json!({"error": {"code": JsonRpcError::INVALID_PARAMS, "message": e.to_string()}}),
                ));
            }
        };
        let outcome = {
            let guard = state.lock().await;
            guard.registry().send(id, &msg).await
        };
        match outcome {
            Ok(receipt) => {
                let v = serde_json::to_value(&receipt).unwrap_or(Value::Null);
                return Ok(json_body(StatusCode::OK, v));
            }
            Err(e) => {
                let je = provider_error(e);
                let status = match je.code {
                    JsonRpcError::PROTOCOL_ERROR => StatusCode::NOT_FOUND,
                    JsonRpcError::AUTH_ERROR => StatusCode::UNAUTHORIZED,
                    JsonRpcError::RATE_LIMIT_ERROR => StatusCode::TOO_MANY_REQUESTS,
                    JsonRpcError::CONFIG_ERROR => StatusCode::BAD_REQUEST,
                    _ => StatusCode::INTERNAL_SERVER_ERROR,
                };
                return Ok(json_body(
                    status,
                    json!({"error": {"code": je.code, "message": je.message, "data": je.data}}),
                ));
            }
        }
    }

    // POST /rpc (and /api/rpc, / for compat)
    if method == Method::POST && (path == "/rpc" || path == "/api/rpc" || path == "/") {
        let body_bytes = match req.collect().await {
            Ok(c) => c.to_bytes(),
            Err(e) => {
                return Ok(text_body(
                    StatusCode::BAD_REQUEST,
                    format!("failed to read body: {e}"),
                ));
            }
        };
        let body_str = String::from_utf8_lossy(&body_bytes).into_owned();
        let response = match parse_request(&body_str) {
            Ok(request) => match state.lock().await.handle_request(request).await {
                DispatchOutcome::Response(r) | DispatchOutcome::Shutdown(r) => r,
                DispatchOutcome::Ignore => crate::jsonrpc::Response::err(
                    Id::Null,
                    JsonRpcError::INVALID_REQUEST,
                    "notifications are not supported over http",
                    None,
                ),
            },
            Err(response) => *response,
        };
        let text = serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_string());
        return Ok(HttpResponse::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(text)).boxed())
            .expect("valid rpc response"));
    }

    // Method not allowed for known paths with wrong verb
    if path == "/health"
        || path == "/api/events"
        || path == "/rpc"
        || path == "/api/rpc"
        || (path.starts_with("/api/providers/") && path.ends_with("/send"))
    {
        return Ok(text_body(
            StatusCode::METHOD_NOT_ALLOWED,
            "method not allowed".to_string(),
        ));
    }

    // Not found
    Ok(json_body(
        StatusCode::NOT_FOUND,
        json!({"error": {"code": -32601, "message": format!("not found: {path}")}}),
    ))
}
