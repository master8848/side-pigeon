//! Optional minimal HTTP/1.1 JSON-RPC + REST server (feature `http`, hand-rolled on hyper).
//!
//! Routes:
//! ```text
//! GET  /health                     -> capabilities_value() (k8s / pc check)
//! POST /api/providers/:id/send     -> AppState::send (typed SendMessage JSON)
//! GET  /api/events                 -> SSE live stream
//! GET  /api/events?since=CURSOR    -> JSON replay (when --features persist)
//! POST /rpc                        -> JSON-RPC dispatch (same as stdio/ws handle_request)
//! ```
//! Keeps stdio primary; enabled via `pc serve --http :8788 --ws :8787`.
//!
//! # Security note — TLS
//! Plaintext HTTP here is **localhost-only** by design. Production deployments
//! MUST place a reverse-proxy (nginx, Caddy, Fly, etc.) in front that
//! terminates TLS and enforces authentication. Do not expose the raw listener
//! directly to the public internet.
#![allow(clippy::too_many_lines)]

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
use tokio::sync::{Mutex, Semaphore, broadcast};

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
    const MAX_CONNECTIONS: usize = 256;
    let semaphore = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    // TODO(idle-timeout): enforce idle read/write timeouts on HTTP connections
    // (e.g. `tokio::time::timeout` around `serve_connection` or `http1::Builder`
    // header read timeout) to reclaim slow-loris / idle sockets.
    loop {
        let (stream, peer) = listener.accept().await?;
        let permit = match semaphore.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                tracing::warn!(peer = %peer, "http connection limit reached (256), dropping");
                // Drop connection; alternative would be to reply 503 before closing.
                continue;
            }
        };
        let state = state.clone();
        tokio::spawn(async move {
            let _permit = permit; // held for connection lifetime
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

fn query_param(uri: &hyper::Uri, key: &str) -> Option<String> {
    uri.query().and_then(|q| {
        for pair in q.split('&') {
            if let Some((k, v)) = pair.split_once('=') {
                if k == key {
                    return Some(v.to_string());
                }
            }
        }
        None
    })
}

const MAX_BODY_BYTES: usize = 1 << 20; // 1 MiB

fn is_allowed_http_origin(origin: &str) -> bool {
    // Allow only loopback origins. Covers http://127.0.0.1[:port], http://localhost[:port], http://[::1][:port]
    origin.contains("127.0.0.1") || origin.contains("localhost") || origin.contains("[::1]")
}

fn with_cors(mut resp: HttpResponse<BoxBodyType>, origin: Option<&str>) -> HttpResponse<BoxBodyType> {
    if let Some(o) = origin {
        if is_allowed_http_origin(o) {
            let headers = resp.headers_mut();
            headers.insert("access-control-allow-origin", o.parse().unwrap_or_else(|_| "http://127.0.0.1".parse().unwrap()));
            headers.insert("access-control-allow-methods", "GET, POST, OPTIONS".parse().unwrap());
            headers.insert("access-control-allow-headers", "content-type, authorization".parse().unwrap());
            headers.insert("vary", "Origin".parse().unwrap());
        }
    }
    resp
}

async fn handle_http(
    state: Arc<Mutex<AppState>>,
    req: HttpRequest<Incoming>,
) -> Result<HttpResponse<BoxBodyType>, hyper::Error> {
    let origin_hdr = req
        .headers()
        .get("origin")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_owned());

    // CORS preflight
    if req.method() == Method::OPTIONS {
        if let Some(ref origin) = origin_hdr {
            if !is_allowed_http_origin(origin) {
                return Ok(text_body(StatusCode::FORBIDDEN, "forbidden origin".to_string()));
            }
            let resp = HttpResponse::builder()
                .status(StatusCode::NO_CONTENT)
                .header("access-control-allow-origin", origin.as_str())
                .header("access-control-allow-methods", "GET, POST, OPTIONS")
                .header("access-control-allow-headers", "content-type, authorization")
                .header("access-control-max-age", "86400")
                .header("vary", "Origin")
                .body(Full::new(Bytes::new()).boxed())
                .expect("valid preflight response");
            return Ok(resp);
        }
        // No Origin: still answer 204 with allowed methods
        return Ok(HttpResponse::builder()
            .status(StatusCode::NO_CONTENT)
            .header("allow", "GET, POST, OPTIONS")
            .body(Full::new(Bytes::new()).boxed())
            .expect("valid OPTIONS response"));
    }

    // CORS enforcement: if Origin present and not loopback, reject
    if let Some(ref origin) = origin_hdr {
        if !is_allowed_http_origin(origin) {
            return Ok(text_body(StatusCode::FORBIDDEN, "forbidden origin".to_string()));
        }
    }

    let uri = req.uri().clone();
    let path = uri.path().to_owned();
    let method = req.method().clone();

    // GET /health
    if method == Method::GET && path == "/health" {
        let caps = {
            let guard = state.lock().await;
            guard.capabilities_value()
        };
        return Ok(with_cors(json_body(StatusCode::OK, caps), origin_hdr.as_deref()));
    }

    // GET /api/events -> SSE (live) or JSON replay (?since=CURSOR when persist)
    if method == Method::GET && path == "/api/events" {
        // Replay: GET /api/events?since=123[&limit=200]
        if let Some(since_raw) = query_param(&uri, "since") {
            #[cfg(not(feature = "persist"))]
            {
                let _ = since_raw;
                return Ok(with_cors(
                    json_body(
                        StatusCode::NOT_IMPLEMENTED,
                        json!({"error": {"code": -32000, "message": "built without --features persist; replay unavailable"}}),
                    ),
                    origin_hdr.as_deref(),
                ));
            }
            #[cfg(feature = "persist")]
            {
                let since: u64 = match since_raw.parse() {
                    Ok(v) => v,
                    Err(_) => {
                        return Ok(with_cors(
                            json_body(
                                StatusCode::BAD_REQUEST,
                                json!({"error": {"code": -32602, "message": "since must be an integer cursor"}}),
                            ),
                            origin_hdr.as_deref(),
                        ));
                    }
                };
                // Cap replay to prevent unbounded reads / DoS
                let raw_limit: Option<u64> = query_param(&uri, "limit").and_then(|s| s.parse().ok());
                let capped_limit: Option<u64> = Some(raw_limit.unwrap_or(500).min(1000));
                let res = {
                    let guard = state.lock().await;
                    match guard.persist() {
                        Some(log) => log.replay_since(since, capped_limit),
                        None => Err("persistence not enabled (use --persist)".into()),
                    }
                };
                match res {
                    Ok(rows) => {
                        let items: Vec<Value> = rows
                            .into_iter()
                            .map(|(cursor, payload)| json!({"cursor": cursor, "event": payload}))
                            .collect();
                        let latest = {
                            let guard = state.lock().await;
                            guard.persist().and_then(|l| l.latest_cursor().ok()).unwrap_or(0)
                        };
                        return Ok(with_cors(
                            json_body(StatusCode::OK, json!({"events": items, "latest_cursor": latest})),
                            origin_hdr.as_deref(),
                        ));
                    }
                    Err(e) => {
                        return Ok(with_cors(
                            json_body(
                                StatusCode::INTERNAL_SERVER_ERROR,
                                json!({"error": {"code": -32603, "message": e}}),
                            ),
                            origin_hdr.as_deref(),
                        ));
                    }
                }
            }
        }
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
        // Add CORS headers to SSE if loopback Origin
        let resp = with_cors(resp, origin_hdr.as_deref());
        return Ok(resp);
    }

    // POST /api/providers/:id/send
    if method == Method::POST && path.starts_with("/api/providers/") && path.ends_with("/send") {
        let id = path
            .strip_prefix("/api/providers/")
            .and_then(|s| s.strip_suffix("/send"))
            .unwrap_or("");
        if id.is_empty() {
            return Ok(with_cors(
                json_body(
                    StatusCode::BAD_REQUEST,
                    json!({"error": {"code": -32602, "message": "missing provider id"}}),
                ),
                origin_hdr.as_deref(),
            ));
        }
        let body_bytes = match req.collect().await {
            Ok(c) => c.to_bytes(),
            Err(e) => {
                return Ok(with_cors(
                    text_body(
                        StatusCode::BAD_REQUEST,
                        format!("failed to read body: {e}"),
                    ),
                    origin_hdr.as_deref(),
                ));
            }
        };
        if body_bytes.len() > MAX_BODY_BYTES {
            return Ok(with_cors(
                text_body(StatusCode::PAYLOAD_TOO_LARGE, "payload too large (max 1 MiB)".to_string()),
                origin_hdr.as_deref(),
            ));
        }
        let msg: provider_core::SendMessage = match serde_json::from_slice(&body_bytes) {
            Ok(v) => v,
            Err(e) => {
                return Ok(with_cors(
                    json_body(
                        StatusCode::BAD_REQUEST,
                        json!({"error": {"code": JsonRpcError::INVALID_PARAMS, "message": e.to_string()}}),
                    ),
                    origin_hdr.as_deref(),
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
                return Ok(with_cors(json_body(StatusCode::OK, v), origin_hdr.as_deref()));
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
                return Ok(with_cors(
                    json_body(
                        status,
                        json!({"error": {"code": je.code, "message": je.message, "data": je.data}}),
                    ),
                    origin_hdr.as_deref(),
                ));
            }
        }
    }

    // POST /rpc (and /api/rpc, / for compat)
    if method == Method::POST && (path == "/rpc" || path == "/api/rpc" || path == "/") {
        let body_bytes = match req.collect().await {
            Ok(c) => c.to_bytes(),
            Err(e) => {
                return Ok(with_cors(
                    text_body(
                        StatusCode::BAD_REQUEST,
                        format!("failed to read body: {e}"),
                    ),
                    origin_hdr.as_deref(),
                ));
            }
        };
        if body_bytes.len() > MAX_BODY_BYTES {
            return Ok(with_cors(
                text_body(StatusCode::PAYLOAD_TOO_LARGE, "payload too large (max 1 MiB)".to_string()),
                origin_hdr.as_deref(),
            ));
        }
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
        let resp = HttpResponse::builder()
            .status(StatusCode::OK)
            .header("content-type", "application/json")
            .body(Full::new(Bytes::from(text)).boxed())
            .expect("valid rpc response");
        return Ok(with_cors(resp, origin_hdr.as_deref()));
    }

    // Method not allowed for known paths with wrong verb
    if path == "/health"
        || path == "/api/events"
        || path == "/rpc"
        || path == "/api/rpc"
        || (path.starts_with("/api/providers/") && path.ends_with("/send"))
    {
        return Ok(with_cors(
            text_body(
                StatusCode::METHOD_NOT_ALLOWED,
                "method not allowed".to_string(),
            ),
            origin_hdr.as_deref(),
        ));
    }

    // Not found
    Ok(with_cors(
        json_body(
            StatusCode::NOT_FOUND,
            json!({"error": {"code": -32601, "message": format!("not found: {path}")}}),
        ),
        origin_hdr.as_deref(),
    ))
}
