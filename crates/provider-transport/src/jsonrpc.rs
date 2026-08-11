//! JSON-RPC 2.0 wire types and framing helpers.
//!
//! Spec: <https://www.jsonrpc.org/specification>. Framing on stdio is one
//! JSON document per line (NDJSON); over WebSocket it is one JSON document per
//! text message.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::events::{
    ChoiceEvent, DraftEvent, ErrorEvent, EVENT_CHOICE, EVENT_DRAFT, EVENT_ERROR, EVENT_MESSAGE,
};
use provider_core::ChannelMessage;

/// The JSON-RPC version this implementation speaks.
pub const JSONRPC_VERSION: &str = "2.0";

/// Request/response id: number, string, or explicit null.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    /// Numeric id.
    Number(u64),
    /// String id.
    Str(String),
    /// Explicit `null` id.
    Null,
}

/// A JSON-RPC 2.0 request. A request without `id` is a notification and
/// receives no response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    /// Must be `"2.0"`.
    pub jsonrpc: String,
    /// Request id; absent for client→server notifications.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Id>,
    /// Method name.
    pub method: String,
    /// Positional or named parameters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

/// A JSON-RPC 2.0 error object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcError {
    /// Error code (negative for protocol errors).
    pub code: i64,
    /// Short human-readable message.
    pub message: String,
    /// Optional structured payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl JsonRpcError {
    /// Parse error.
    pub const PARSE_ERROR: i64 = -32700;
    /// Invalid request.
    pub const INVALID_REQUEST: i64 = -32600;
    /// Method not found.
    pub const METHOD_NOT_FOUND: i64 = -32601;
    /// Invalid params.
    pub const INVALID_PARAMS: i64 = -32602;
    /// Internal error.
    pub const INTERNAL_ERROR: i64 = -32603;
    /// Server error base (implementation-defined range `-32000..-32099`).
    pub const SERVER_ERROR: i64 = -32000;
    /// Provider configuration error.
    pub const CONFIG_ERROR: i64 = -32001;
    /// Provider auth error.
    pub const AUTH_ERROR: i64 = -32002;
    /// Provider rate-limit error.
    pub const RATE_LIMIT_ERROR: i64 = -32003;
    /// Provider protocol error (unknown provider, not started, ...).
    pub const PROTOCOL_ERROR: i64 = -32004;
    /// Provider network error.
    pub const NETWORK_ERROR: i64 = -32005;

    /// Build an error.
    pub fn new(code: i64, message: impl Into<String>, data: Option<Value>) -> Self {
        JsonRpcError {
            code,
            message: message.into(),
            data,
        }
    }

    /// `-32700` parse error.
    pub fn parse_error(data: Option<Value>) -> Self {
        JsonRpcError::new(Self::PARSE_ERROR, "parse error", data)
    }

    /// `-32600` invalid request.
    pub fn invalid_request(data: Option<Value>) -> Self {
        JsonRpcError::new(Self::INVALID_REQUEST, "invalid request", data)
    }

    /// `-32601` method not found.
    pub fn method_not_found(method: &str) -> Self {
        JsonRpcError::new(
            Self::METHOD_NOT_FOUND,
            format!("method not found: {method}"),
            None,
        )
    }

    /// `-32602` invalid params.
    pub fn invalid_params(data: Option<Value>) -> Self {
        JsonRpcError::new(Self::INVALID_PARAMS, "invalid params", data)
    }

    /// `-32603` internal error.
    pub fn internal_error(data: Option<Value>) -> Self {
        JsonRpcError::new(Self::INTERNAL_ERROR, "internal error", data)
    }
}

/// A JSON-RPC 2.0 response (success or failure).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    /// Must be `"2.0"`.
    pub jsonrpc: String,
    /// Echoes the request id.
    pub id: Id,
    /// Present on success.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// Present on failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

impl Response {
    /// Success response.
    pub fn ok(id: Id, result: Value) -> Self {
        Response {
            jsonrpc: JSONRPC_VERSION.into(),
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Error response.
    pub fn err(id: Id, code: i64, message: impl Into<String>, data: Option<Value>) -> Self {
        Response {
            jsonrpc: JSONRPC_VERSION.into(),
            id,
            result: None,
            error: Some(JsonRpcError::new(code, message, data)),
        }
    }

    /// Whether this is a success response.
    pub fn is_success(&self) -> bool {
        self.error.is_none()
    }

    /// Consume into `Ok(result)` / `Err(error)`.
    pub fn into_result(self) -> Result<Value, JsonRpcError> {
        match (self.result, self.error) {
            (Some(result), _) => Ok(result),
            (_, Some(error)) => Err(error),
            _ => Err(JsonRpcError::internal_error(None)),
        }
    }
}

/// A server→client notification (a request without an id).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    /// Must be `"2.0"`.
    pub jsonrpc: String,
    /// Notification method, e.g. `event.message`.
    pub method: String,
    /// Payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Notification {
    /// Build a notification with an arbitrary JSON payload.
    pub fn new(method: impl Into<String>, params: Option<Value>) -> Self {
        Notification {
            jsonrpc: JSONRPC_VERSION.into(),
            method: method.into(),
            params,
        }
    }

    /// `event.message` notification carrying an inbound message.
    pub fn message(msg: &ChannelMessage) -> Self {
        Notification::new(
            EVENT_MESSAGE,
            serde_json::to_value(msg)
                .ok()
                .map(|v| serde_json::json!({ "message": v })),
        )
    }

    /// `event.draft` notification.
    pub fn draft(ev: &DraftEvent) -> Self {
        Notification::new(EVENT_DRAFT, serde_json::to_value(ev).ok())
    }

    /// `event.choice` notification.
    pub fn choice(ev: &ChoiceEvent) -> Self {
        Notification::new(EVENT_CHOICE, serde_json::to_value(ev).ok())
    }

    /// `event.error` notification.
    pub fn error(ev: &ErrorEvent) -> Self {
        Notification::new(EVENT_ERROR, serde_json::to_value(ev).ok())
    }
}

/// Parse a JSON-RPC request from one framed document.
///
/// On malformed JSON returns a `-32700` response with `id: null`; on valid
/// JSON that is not a well-formed request (wrong version, missing fields,
/// batch array, ...) returns a `-32600` response with `id: null`.
// The error path (a parse failure response) is rare; box it so the hot path
// does not move a 144-byte Response around.
pub fn parse_request(line: &str) -> Result<Request, Box<Response>> {
    let value: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            return Err(Box::new(Response::err(
                Id::Null,
                JsonRpcError::PARSE_ERROR,
                "parse error",
                Some(serde_json::json!({ "detail": e.to_string() })),
            )));
        }
    };
    if value.is_array() {
        return Err(Box::new(Response::err(
            Id::Null,
            JsonRpcError::INVALID_REQUEST,
            "batch requests are not supported",
            None,
        )));
    }
    let request: Request = match serde_json::from_value(value) {
        Ok(r) => r,
        Err(e) => {
            return Err(Box::new(Response::err(
                Id::Null,
                JsonRpcError::INVALID_REQUEST,
                "invalid request",
                Some(serde_json::json!({ "detail": e.to_string() })),
            )));
        }
    };
    if request.jsonrpc != JSONRPC_VERSION {
        return Err(Box::new(Response::err(
            Id::Null,
            JsonRpcError::INVALID_REQUEST,
            format!(
                "jsonrpc version must be {JSONRPC_VERSION} (got {})",
                request.jsonrpc
            ),
            None,
        )));
    }
    Ok(request)
}
