//! Optional minimal HTTP/1.1 JSON-RPC server (feature `http`, hand-rolled on hyper).
//!
//! `POST /rpc` with a JSON-RPC request body returns the JSON-RPC response
//! body. No server→client push over plain HTTP (notifications have no
//! receivers and are dropped); clients use `capabilities`/`send` for a
//! request/response interaction. Batch requests are rejected with `-32600`.

use std::sync::Arc;

use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request as HttpRequest, Response as HttpResponse, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio::sync::Mutex;

use crate::error::TransportError;
use crate::jsonrpc::{parse_request, Id, JsonRpcError, Response};
use crate::state::{AppState, DispatchOutcome};

/// Accept HTTP connections on `listener` and serve JSON-RPC at any POST path.
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

async fn handle_http(
    state: Arc<Mutex<AppState>>,
    request: HttpRequest<Incoming>,
) -> Result<HttpResponse<String>, hyper::Error> {
    if request.method() != Method::POST {
        return Ok(HttpResponse::builder()
            .status(StatusCode::METHOD_NOT_ALLOWED)
            .body("method not allowed: use POST /rpc".to_string())
            .expect("valid static response"));
    }
    let body = request.collect().await?.to_bytes();
    let body = String::from_utf8_lossy(&body).into_owned();

    let response = match parse_request(&body) {
        Ok(request) => match state.lock().await.handle_request(request).await {
            DispatchOutcome::Response(r) | DispatchOutcome::Shutdown(r) => r,
            DispatchOutcome::Ignore => Response::err(
                Id::Null,
                JsonRpcError::INVALID_REQUEST,
                "notifications are not supported over http",
                None,
            ),
        },
        Err(response) => *response,
    };

    let text = serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_string());
    Ok(HttpResponse::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/json")
        .body(text)
        .expect("valid static response"))
}
