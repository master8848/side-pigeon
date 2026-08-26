//! Shared server state and JSON-RPC method dispatch.

use std::sync::Arc;

use crate::events::ErrorEvent;
use provider_core::client::EventBus;
use provider_core::{ChannelMessage, ProviderError, ProviderEvents, ProviderRegistry, SendMessage};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::broadcast;

use crate::events::{EVENT_ERROR, EVENT_MESSAGE};
use crate::jsonrpc::{JsonRpcError, Notification, Request, Response, JSONRPC_VERSION};
#[cfg(feature = "persist")]
use crate::persist::EventLog;

/// Broadcast channel capacity for outgoing frames (responses + notifications).
///
/// Sized for chat-rate traffic (not 512 ~512 KB idle overhead per connection).
/// P6: 32 is intentionally small for the global `broadcast` fan-out; per-connection
/// `mpsc` in `ws.rs` is 1024, so a slow client lags on its own queue and receives
/// an honest `-32006` `dropped_frames` notification instead of growing the global
/// buffer. Bumping to 128 would hide backpressure and increase idle memory per
/// subscriber (`broadcast` pre-allocates slots). Keep 32 unless load testing shows
/// sustained >32 frames in-flight at chat rate; the `Lagged` handler already
/// recovers gracefully.
/// See also `crates/provider-core/src/plugin.rs` P6 for the companion `O(N)`
/// dedup fix (now `O(1)` amortized via `VecDeque` order queue).
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
///
/// # P4 — global lock serialization
///
/// Historical debt: callers hold `Arc<Mutex<AppState>>` (see `bin/pc/src/main.rs:1220`
/// `Arc::new(Mutex::new(state))`, `crates/provider-transport/src/ws.rs:196` and
/// `http.rs:490` `state.lock().await.handle_request(...).await`) and
/// `handle_request(&mut self)` requires exclusive access, so a `send` holds the
/// global lock across an `.await` (`ProviderRegistry::send` → provider network I/O).
/// All other `send`/`listen`/`capabilities` requests serialize behind it.
///
/// Ideal fix (deferred — see `docs/POLISH.md:P4`): split `AppState` into
/// `RwLock<ProviderRegistry>` (read for `send`/`capabilities`/`listen` routing,
/// write only for `shutdown` flag / `stop_all`) + per-provider `Mutex` inside
/// `ProviderRegistry` (each provider's `start`/`stop`/`send` serializes only on
/// its own key). `handle_request` would then take `&self` (no `&mut`) and callers
/// would use `Arc<RwLock<AppState>>` or `Arc<AppState>` with interior locks, so
/// `send` never holds a global exclusive guard across an await. `ProviderRegistry::send`
/// already takes `&self`, so the registry itself is read-friendly; only the outer
/// `AppState` `&mut` and `shutdown: bool` force exclusive access today.
///
/// Minimal safe mitigation (this file): `send` delegates to `registry.send(&self)`
/// which does not require `&mut`; `listen`/`shutdown` are the only writers.
/// Callers that only need `send` can release the outer guard before awaiting by
/// cloning `registry` handles or by using the read-only paths in `http.rs`
/// (`guard.registry().send(...).await` holds the guard across await today — future
/// change is to downgrade to `RwLock::read` or to `Arc::clone(registry)`). No
/// deadlock is possible: the lock is a single `tokio::sync::Mutex` with no
/// nested acquisition, and `ProviderEvents` callbacks never re-enter `AppState`.
/// Full `RwLock` migration is intentionally left as a `TODO(P4)` to avoid a
/// risky cross-crate signature churn in this polish pass.
pub struct AppState {
    protocol_version: String,
    transport: Vec<String>,
    registry: ProviderRegistry,
    notify: broadcast::Sender<Outbound>,
    shutdown: bool,
    event_bus: Option<EventBus>,
    #[cfg(feature = "persist")]
    persist: Option<std::sync::Arc<crate::persist::EventLog>>,
}

impl AppState {
    /// Create the state and its notification broadcast sender. Hand `sender`
    /// to the transport (stdio: one subscription; ws: one per connection;
    /// http: none — notifications are dropped, responses are returned inline).
    pub fn new(transport: &str) -> (Self, broadcast::Sender<Outbound>) {
        Self::new_with_transports(vec![transport.to_string()])
    }

    /// Create state advertising multiple transports (e.g. `["stdio","http","ws"]`
    /// for `pc serve`). Backward-compatible wrapper around [`AppState::new`].
    pub fn new_with_transports(transports: Vec<String>) -> (Self, broadcast::Sender<Outbound>) {
        let transports = if transports.is_empty() {
            vec!["stdio".to_string()]
        } else {
            transports
        };
        let (tx, _rx) = broadcast::channel(OUTBOUND_CAPACITY);
        let events: Arc<dyn ProviderEvents> = Arc::new(NotifyEvents {
            tx: tx.clone(),
            #[cfg(feature = "persist")]
            persist: None,
        });
        let state = AppState {
            protocol_version: env!("CARGO_PKG_VERSION").to_string(),
            transport: transports,
            registry: ProviderRegistry::new(events),
            notify: tx.clone(),
            shutdown: false,
            event_bus: None,
            #[cfg(feature = "persist")]
            persist: None,
        };
        (state, tx)
    }

    /// Enable SQLite persistence at `path` (feature `persist` only). Every
    /// `event.message` / `event.error` is appended; `GET /api/events?since=c`
    /// replays. No-op when built without `persist`.
    #[cfg(feature = "persist")]
    pub fn with_persist(mut self, path: impl AsRef<std::path::Path>) -> Result<Self, String> {
        let log = std::sync::Arc::new(crate::persist::EventLog::open(path)?);
        let tx = self.notify.clone();
        let persist = log.clone();
        // Re-wire the events sink to include the log
        let events: Arc<dyn ProviderEvents> = if let Some(bus) = self.event_bus.clone() {
            Arc::new(BridgeEvents {
                tx,
                bus,
                persist: Some(persist.clone()),
            })
        } else {
            Arc::new(NotifyEvents {
                tx,
                persist: Some(persist.clone()),
            })
        };
        self.registry.set_events(events);
        self.persist = Some(log);
        Ok(self)
    }

    /// Borrow the persist log, if enabled.
    #[cfg(feature = "persist")]
    pub fn persist(&self) -> Option<std::sync::Arc<crate::persist::EventLog>> {
        self.persist.clone()
    }

    /// Append an additional transport to the advertised capabilities (builder-style).
    pub fn with_transport(mut self, transport: impl Into<String>) -> Self {
        let t = transport.into();
        if !self.transport.contains(&t) {
            self.transport.push(t);
        }
        self
    }

    /// Install a headless [`EventBus`] as the provider event sink.
    ///
    /// When a bus is present, every inbound `ChannelMessage` first runs the
    /// bus's plugin chain and only then fans out to local typed subscribers
    /// AND to the JSON-RPC `broadcast::Sender<Outbound>`. Messages dropped by
    /// a plugin never reach either sink. Rewrites are forwarded as the mutated
    /// message. The JSON-RPC wire is unchanged.
    pub fn with_event_bus(mut self, bus: EventBus) -> Self {
        let tx = self.notify.clone();
        #[cfg(feature = "persist")]
        let persist = self.persist.clone();
        let bridge: Arc<dyn ProviderEvents> = Arc::new(BridgeEvents {
            tx,
            bus: bus.clone(),
            #[cfg(feature = "persist")]
            persist,
        });
        self.registry.set_events(bridge);
        self.event_bus = Some(bus);
        self
    }

    /// Borrow the installed event bus, if any.
    pub fn event_bus(&self) -> Option<&EventBus> {
        self.event_bus.as_ref()
    }

    /// Push a plugin onto the installed bus. If no bus is installed yet, a
    /// fresh bus is created, bridged, and stored. Returns `&mut Self` for
    /// builder chaining.
    pub fn with_plugin<P: provider_core::Plugin + 'static>(&mut self, plugin: P) -> &mut Self {
        if self.event_bus.is_none() {
            let bus = EventBus::new();
            let tx = self.notify.clone();
            #[cfg(feature = "persist")]
            let persist = self.persist.clone();
            let bridge: Arc<dyn ProviderEvents> = Arc::new(BridgeEvents {
                tx,
                bus: bus.clone(),
                #[cfg(feature = "persist")]
                persist,
            });
            self.registry.set_events(bridge);
            self.event_bus = Some(bus);
        }
        if let Some(bus) = &self.event_bus {
            bus.use_plugin(plugin);
        }
        self
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
    ///
    /// P4 note: `&mut self` forces callers to hold `Arc<Mutex<AppState>>` across
    /// the await (serializes `send`). The read-only branches (`send`, `capabilities`)
    /// only need `&self`; a future `handle_request(&self)` with `RwLock` + per-provider
    /// `Mutex` would allow concurrent `send`s. Kept as `&mut` in this pass to
    /// avoid breaking `ws.rs`/`http.rs`/`stdio.rs` signatures; the interior
    /// `registry.send(&self)` is already `&self`-friendly so the global lock is
    /// the only serializer. See struct-level `P4` doc for the full migration plan.
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
        #[cfg_attr(not(feature = "persist"), allow(unused_mut))]
        let mut features = vec!["send"];
        #[cfg(feature = "persist")]
        if self.persist.is_some() {
            features.push("persist");
        }
        json!({
            "protocolVersion": self.protocol_version,
            "methods": ["initialize", "capabilities", "listen", "send", "shutdown"],
            // Only emit event.message + event.error today; draft/choice are
            // reserved vocabulary but not implemented, so they must not be
            // advertised (the contract must not lie to clients).
            "notifications": [EVENT_MESSAGE, EVENT_ERROR],
            "features": features,
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
    #[cfg(feature = "persist")]
    persist: Option<std::sync::Arc<EventLog>>,
}

impl ProviderEvents for NotifyEvents {
    fn on_message(&self, msg: ChannelMessage) {
        let notification = Notification::message(&msg);
        // S7: EventLog::append is sync WAL insert (~0.5-5ms). Mitigated by
        // WAL+NORMAL+32M journal_size_limit in persist.rs. Ideal is mpsc writer
        // thread or tokio::task::spawn_blocking at async call-sites; this
        // sync ProviderEvents hook cannot spawn_blocking without a runtime
        // handle, so we keep the direct append and bound growth via prune().
        #[cfg(feature = "persist")]
        if let Some(log) = &self.persist {
            if let Err(e) = log.append(&notification) {
                tracing::warn!(error = %e, "persist append event.message failed");
            }
        }
        if let Err(err) = self.tx.send(Outbound::Notification(notification)) {
            tracing::debug!(error = %err, "dropping event.message (no receivers)");
        }
    }

    fn on_error(&self, provider: &str, error: &ProviderError) {
        let notification = error_notification(provider, error);
        // S7: see on_message comment — sync WAL insert mitigated by journal_size_limit + prune.
        #[cfg(feature = "persist")]
        if let Some(log) = &self.persist {
            if let Err(e) = log.append(&notification) {
                tracing::warn!(error = %e, "persist append event.error failed");
            }
        }
        if let Err(err) = self.tx.send(Outbound::Notification(notification)) {
            tracing::debug!(error = %err, "dropping event.error (no receivers)");
        }
    }
}

/// Bridge that runs the headless plugin chain AND forwards to JSON-RPC.
///
/// `on_message` first calls `bus.publish_filtered` so plugins can drop or
/// rewrite. Dropped messages never reach the broadcast channel or the local
/// typed subscribers. Non-dropped messages fan out to both: the bus's local
/// callbacks (via the publish call) and the JSON-RPC `broadcast::Sender`.
struct BridgeEvents {
    tx: broadcast::Sender<Outbound>,
    bus: EventBus,
    #[cfg(feature = "persist")]
    persist: Option<std::sync::Arc<EventLog>>,
}

impl ProviderEvents for BridgeEvents {
    fn on_message(&self, msg: ChannelMessage) {
        let Some((_, filtered)) = self.bus.publish_filtered(msg) else {
            return;
        };
        let notification = Notification::message(&filtered);
        // S7: see NotifyEvents::on_message — sync WAL insert mitigated by journal_size_limit + prune.
        #[cfg(feature = "persist")]
        if let Some(log) = &self.persist {
            if let Err(e) = log.append(&notification) {
                tracing::warn!(error = %e, "persist append event.message failed");
            }
        }
        if let Err(err) = self.tx.send(Outbound::Notification(notification)) {
            tracing::debug!(error = %err, "dropping event.message (no receivers)");
        }
    }

    fn on_error(&self, provider: &str, error: &ProviderError) {
        self.bus.publish_error(provider, error);
        let notification = error_notification(provider, error);
        // S7: see NotifyEvents::on_message — sync WAL insert mitigated by journal_size_limit + prune.
        #[cfg(feature = "persist")]
        if let Some(log) = &self.persist {
            if let Err(e) = log.append(&notification) {
                tracing::warn!(error = %e, "persist append event.error failed");
            }
        }
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
