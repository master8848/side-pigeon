//! Shared server state and JSON-RPC method dispatch.

use std::sync::Arc;

use crate::events::ErrorEvent;
use provider_core::{ChannelMessage, ProviderError, ProviderEvents, ProviderRegistry, SendMessage};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::broadcast;

use crate::events::{EVENT_ERROR, EVENT_MESSAGE};
use crate::jsonrpc::{JsonRpcError, Notification, Request, Response, JSONRPC_VERSION};

/// Broadcast channel capacity for outgoing frames (responses + notifications).
/// Sized for chat-rate traffic (not 512 ~512 KB idle overhead per connection).
const OUTBOUND_CAPACITY: usize = 32;

/// A frame the transport writes to the client: either a response to a request
/// or an asynchronous event notification.
#[derive(Debug, Clone, PartialEq)]
pub enum Outbound {
    /// Response to a client request.
    Response(Response),
    /// Server→client notification.
    Notification(Notification),
}

/// Result of dispatching one request.
#[derive(Debug)]
pub enum DispatchOutcome {
    /// A response should be sent to the client.
    Response(Response),
    /// The request was a client→server notification; no response.
    Ignore,
    /// A response should be sent, then the transport should stop serving.
    Shutdown(Response),
}

/// Shared JSON-RPC server state: provider registry + notification fan-out.
pub struct AppState {
    protocol_version: String,
    transport: Vec<String>,
    registry: ProviderRegistry,
    notify: broadcast::Sender<Outbound>,
    shutdown: bool,
}

impl AppState {
    /// Create the state and its notification broadcast sender. Hand `sender`
    /// to the transport (stdio: one subscription; ws: one per connection;
    /// http: none — notifications are dropped, responses are returned inline).
    pub fn new(transport: &str) -> (Self, broadcast::Sender<Outbound>) {
        let (tx, _rx) = broadcast::channel(OUTBOUND_CAPACITY);
        let events: Arc<dyn ProviderEvents> = Arc::new(NotifyEvents { tx: tx.clone() });
        let state = AppState {
            protocol_version: env!("CARGO_PKG_VERSION").to_string(),
            transport: vec![transport.to_string()],
            registry: ProviderRegistry::new(events),
            notify: tx.clone(),
            shutdown: false,
        };
        (state, tx)
    }

    /// The shared event sink (hand this to providers at construction).
    pub fn events(&self) -> Arc<dyn ProviderEvents> {
        self.registry.events().clone()
    }

    /// Mutable access to the provider registry (register providers before serving).
    pub fn registry_mut(&mut self) -> &mut ProviderRegistry {
        &mut self.registry
    }

    /// Immutable access to the provider registry.
    pub fn registry(&self) -> &ProviderRegistry {
        &self.registry
    }

    /// The protocol version announced by `initialize`/`capabilities`.
    pub fn protocol_version(&self) -> &str {
        &self.protocol_version
    }

    /// Whether a `shutdown` request has been handled.
    pub fn is_shutdown(&self) -> bool {
        self.shutdown
    }

    /// The notification broadcast sender.
    pub fn notify(&self) -> &broadcast::Sender<Outbound> {
        &self.notify
    }

    /// Dispatch one request. See the crate docs for the method table.
    pub async fn handle_request(&mut self, req: Request) -> DispatchOutcome {
        let Some(id) = req.id.clone() else {
            tracing::debug!(method = %req.method, "ignoring client notification (no id)");
            return DispatchOutcome::Ignore;
        };
        if req.jsonrpc != JSONRPC_VERSION {
            return DispatchOutcome::Response(Response::err(
                id,
                JsonRpcError::INVALID_REQUEST,
                format!(
                    "jsonrpc version must be {JSONRPC_VERSION} (got {})",
                    req.jsonrpc
                ),
                None,
            ));
        }
        let result = match req.method.as_str() {
            "initialize" => Ok(self.capabilities_value()),
            "capabilities" => Ok(self.capabilities_value()),
            "listen" => self.listen(req.params.as_ref()).await,
            "send" => self.send(req.params.as_ref()).await,
            "shutdown" => {
                self.shutdown = true;
                Ok(Value::Null)
            }
            other => Err(JsonRpcError::method_not_found(other)),
        };
        match result {
            Ok(value) => {
                let response = Response::ok(id, value);
                if self.shutdown && req.method == "shutdown" {
                    DispatchOutcome::Shutdown(response)
                } else {
                    DispatchOutcome::Response(response)
                }
            }
            Err(error) => DispatchOutcome::Response(Response {
                jsonrpc: JSONRPC_VERSION.into(),
                id,
                result: None,
                error: Some(error),
            }),
        }
    }

    /// The self-describing capabilities object returned by `initialize` and
    /// `capabilities`.
    pub fn capabilities_value(&self) -> Value {
        json!({
            "protocolVersion": self.protocol_version,
            "methods": ["initialize", "capabilities", "listen", "send", "shutdown"],
            // Only emit event.message + event.error today; draft/choice are
            // reserved vocabulary but not implemented, so they must not be
            // advertised (the contract must not lie to clients).
            "notifications": [EVENT_MESSAGE, EVENT_ERROR],
            "features": ["send"],
            "transport": self.transport,
            "providers": self.registry.ids(),
        })
    }

    async fn listen(&mut self, params: Option<&Value>) -> Result<Value, JsonRpcError> {
        let want: Option<Vec<String>> = match params {
            None => None,
            Some(v) if v.is_null() => None,
            Some(v) => {
                let object = v.as_object().ok_or_else(|| {
                    JsonRpcError::invalid_params(Some(json!({ "expected": "object" })))
                })?;
                match object.get("providers") {
                    None => None,
                    Some(Value::Array(_)) => Some(
                        serde_json::from_value(object["providers"].clone()).map_err(|e| {
                            JsonRpcError::invalid_params(Some(
                                json!({ "providers": e.to_string() }),
                            ))
                        })?,
                    ),
                    Some(_) => {
                        return Err(JsonRpcError::invalid_params(Some(json!({
                            "providers": "must be an array of provider ids"
                        }))))
                    }
                }
            }
        };
        match want {
            None => {
                self.registry.start_all().await.map_err(provider_error)?;
            }
            Some(ids) => {
                for id in ids {
                    self.registry.start(&id).await.map_err(provider_error)?;
                }
            }
        }
        Ok(json!({ "started": self.registry.started_ids() }))
    }

    async fn send(&mut self, params: Option<&Value>) -> Result<Value, JsonRpcError> {
        #[derive(Deserialize)]
        struct SendParams {
            provider: String,
            message: SendMessage,
        }
        let params: SendParams = serde_json::from_value(params.cloned().unwrap_or(Value::Null))
            .map_err(|e| {
                JsonRpcError::invalid_params(Some(json!({
                    "expected": { "provider": "string", "message": "SendMessage" },
                    "detail": e.to_string(),
                })))
            })?;
        let receipt = self
            .registry
            .send(&params.provider, &params.message)
            .await
            .map_err(provider_error)?;
        serde_json::to_value(receipt)
            .map_err(|e| JsonRpcError::internal_error(Some(json!({ "detail": e.to_string() }))))
    }
}

/// Map a [`ProviderError`] to a JSON-RPC error (`-32000..-32005`).
pub fn provider_error(e: ProviderError) -> JsonRpcError {
    let code = match &e {
        ProviderError::Config(_) => JsonRpcError::CONFIG_ERROR,
        ProviderError::Auth(_) => JsonRpcError::AUTH_ERROR,
        ProviderError::RateLimit(_) => JsonRpcError::RATE_LIMIT_ERROR,
        ProviderError::Protocol(_) => JsonRpcError::PROTOCOL_ERROR,
        ProviderError::Network(_) => JsonRpcError::NETWORK_ERROR,
        ProviderError::Other(_) => JsonRpcError::SERVER_ERROR,
    };
    JsonRpcError {
        code,
        message: e.to_string(),
        data: Some(json!({ "kind": e.kind() })),
    }
}

/// ProviderEvents sink that pushes `event.*` notifications into the broadcast
/// channel. Synchronous by design (the provider contract's `on_message` is
/// sync); broadcast send never blocks.
struct NotifyEvents {
    tx: broadcast::Sender<Outbound>,
}

impl ProviderEvents for NotifyEvents {
    fn on_message(&self, msg: ChannelMessage) {
        let notification = Notification::message(&msg);
        if let Err(err) = self.tx.send(Outbound::Notification(notification)) {
            tracing::debug!(error = %err, "dropping event.message (no receivers)");
        }
    }

    fn on_error(&self, provider: &str, error: &ProviderError) {
        let notification = error_notification(provider, error);
        if let Err(err) = self.tx.send(Outbound::Notification(notification)) {
            tracing::debug!(error = %err, "dropping event.error (no receivers)");
        }
    }
}

/// Build the `event.error` notification for a provider error, reusing the
/// JSON-RPC error-code mapping from [`provider_error`] so hosts see the same
/// `code`/`kind` vocabulary as request errors.
pub fn error_notification(provider: &str, error: &ProviderError) -> Notification {
    let code = provider_error(error.clone()).code;
    let event = ErrorEvent {
        provider: Some(provider.to_string()),
        code,
        message: error.to_string(),
        data: Some(serde_json::json!({ "kind": error.kind() })),
    };
    Notification::error(&event)
}

/// The `event.error` notification emitted when the transport itself had to
/// drop frames (client too slow / disconnected): honest backpressure signal.
pub fn dropped_frames_notification(skipped: u64) -> Notification {
    let event = ErrorEvent {
        provider: None,
        code: -32006, // transport-level, outside the provider mapping range
        message: format!("transport dropped {skipped} outbound frame(s) (client lag)"),
        data: Some(serde_json::json!({ "kind": "Transport", "skipped": skipped })),
    };
    Notification::error(&event)
}
