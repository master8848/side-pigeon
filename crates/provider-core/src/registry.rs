//! Feature-gated provider registry.
//!
//! The registry owns the set of [`ChatProvider`]s the host has compiled in.
//! Provider crates depend only on this crate + `std`; the host wires them in
//! behind cargo features (compile-time pruning, per `docs/architecture.md`).
//!
//! The registry is gated behind the `registry` cargo feature (on by default)
//! so an embedder can build a provider-core without any registry machinery.

use std::collections::HashSet;
use std::sync::Arc;

use crate::schema::{ChannelMessage, SendMessage, SendReceipt};
use crate::traits::{ChatProvider, ProviderEvents};
use crate::ProviderError;

/// Runtime registry of compiled-in providers.
///
/// Thread-safety: the registry itself is not `Sync`; the transport owns it
/// (single-threaded stdio loop or behind a `Mutex` for ws/http) while
/// [`ProviderEvents`] callbacks are `Sync` and can fire from any provider task.
pub struct ProviderRegistry {
    events: Arc<dyn ProviderEvents>,
    providers: Vec<Box<dyn ChatProvider>>,
    started: HashSet<String>,
}

impl ProviderRegistry {
    /// Create an empty registry. `events` is the sink every provider reports
    /// inbound messages to (constructed by the transport).
    pub fn new(events: Arc<dyn ProviderEvents>) -> Self {
        ProviderRegistry {
            events,
            providers: Vec::new(),
            started: HashSet::new(),
        }
    }

    /// The shared event sink handed to providers.
    pub fn events(&self) -> &Arc<dyn ProviderEvents> {
        &self.events
    }

    /// Replace the shared event sink (used by `AppState::with_event_bus`).
    pub fn set_events(&mut self, events: Arc<dyn ProviderEvents>) {
        self.events = events;
    }

    /// Register a provider. Fails with [`ProviderError::Protocol`] if a
    /// provider with the same id is already registered.
    pub fn register(&mut self, provider: Box<dyn ChatProvider>) -> Result<(), ProviderError> {
        let id = provider.id();
        if self.get(id).is_some() {
            return Err(ProviderError::Protocol(format!(
                "duplicate provider id: {id}"
            )));
        }
        self.providers.push(provider);
        Ok(())
    }

    /// Look up a provider by id.
    pub fn get(&self, id: &str) -> Option<&dyn ChatProvider> {
        self.providers
            .iter()
            .find(|p| p.id() == id)
            .map(|p| p.as_ref())
    }

    /// Mutable lookup by id.
    pub fn get_mut(&mut self, id: &str) -> Option<&mut (dyn ChatProvider + '_)> {
        for provider in self.providers.iter_mut() {
            if provider.id() == id {
                return Some(provider.as_mut());
            }
        }
        None
    }

    /// Sorted list of registered provider ids.
    pub fn ids(&self) -> Vec<&'static str> {
        let mut ids: Vec<&'static str> = self.providers.iter().map(|p| p.id()).collect();
        ids.sort_unstable();
        ids
    }

    /// Number of registered providers.
    pub fn len(&self) -> usize {
        self.providers.len()
    }

    /// Whether no providers are registered.
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    /// Whether a provider has been started (via [`start`](Self::start)).
    pub fn is_started(&self, id: &str) -> bool {
        self.started.contains(id)
    }

    /// Sorted ids of currently running providers.
    pub fn started_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.started.iter().cloned().collect();
        ids.sort();
        ids
    }

    /// Start a single provider (idempotent). Unknown ids fail with
    /// [`ProviderError::Protocol`].
    pub async fn start(&mut self, id: &str) -> Result<(), ProviderError> {
        if self.started.contains(id) {
            return Ok(());
        }
        let provider = self
            .get_mut(id)
            .ok_or_else(|| ProviderError::Protocol(format!("unknown provider: {id}")))?;
        provider.start().await?;
        self.started.insert(id.to_string());
        Ok(())
    }

    /// Start every registered provider. Attempts all of them and returns the
    /// ids that started; if any failed, the first error is returned (already
    /// started ones are left running).
    ///
    /// Provider starts run concurrently via `JoinSet` so N providers do not
    /// pay N× startup latency (see Phase 06: bod server fans out once).
    /// Each start is jittered with a tiny deterministic delay to avoid
    /// thundering herds on restart without pulling `rand` into the lean core.
    pub async fn start_all(&mut self) -> Result<Vec<String>, ProviderError> {
        // Drain providers that still need starting so each `Box<dyn ChatProvider>`
        // can be moved into its own task (required for `&mut self` start).
        let mut to_start: Vec<Box<dyn ChatProvider>> = Vec::new();
        let mut remain: Vec<Box<dyn ChatProvider>> = Vec::new();
        for p in self.providers.drain(..) {
            if self.started.contains(p.id()) {
                remain.push(p);
            } else {
                to_start.push(p);
            }
        }
        self.providers.append(&mut remain);

        if to_start.is_empty() {
            return Ok(self.started_ids());
        }

        // SAFETY: `registry` feature implies `tokio` dep is present (see Cargo.toml).
        let mut set: tokio::task::JoinSet<(
            Box<dyn ChatProvider>,
            String,
            Result<(), ProviderError>,
        )> = tokio::task::JoinSet::new();
        for mut provider in to_start {
            let id_clone = provider.id().to_string();
            set.spawn(async move {
                let jitter = {
                    let mut x: u64 = 14695981039346656037;
                    for b in id_clone.as_bytes() {
                        x ^= *b as u64;
                        x = x.wrapping_mul(1099511628211);
                    }
                    x % 50
                };
                tokio::time::sleep(std::time::Duration::from_millis(jitter)).await;
                let res = provider.start().await;
                (provider, id_clone, res)
            });
        }

        let mut started: Vec<String> = Vec::new();
        let mut first_error: Option<ProviderError> = None;
        while let Some(join) = set.join_next().await {
            match join {
                Ok((provider, id, Ok(()))) => {
                    self.started.insert(id.clone());
                    started.push(id);
                    self.providers.push(provider);
                }
                Ok((provider, _id, Err(e))) => {
                    if first_error.is_none() {
                        first_error = Some(e);
                    }
                    self.providers.push(provider);
                }
                Err(e) => {
                    if first_error.is_none() {
                        first_error = Some(ProviderError::Other(format!(
                            "provider start panicked: {e}"
                        )));
                    }
                }
            }
        }
        started.sort();
        match first_error {
            Some(e) => Err(e),
            None => Ok(started),
        }
    }

    /// Stop a single provider (idempotent).
    pub async fn stop(&mut self, id: &str) -> Result<(), ProviderError> {
        if !self.started.remove(id) {
            return Ok(());
        }
        let provider = self
            .get_mut(id)
            .ok_or_else(|| ProviderError::Protocol(format!("unknown provider: {id}")))?;
        provider.stop().await
    }

    /// Stop every running provider. Best-effort: all are stopped, the first
    /// error (if any) is returned.
    pub async fn stop_all(&mut self) -> Result<(), ProviderError> {
        let ids: Vec<String> = self.started.iter().cloned().collect();
        let mut first_error: Option<ProviderError> = None;
        for id in &ids {
            if let Err(e) = self.stop(id).await {
                if first_error.is_none() {
                    first_error = Some(e);
                }
            }
        }
        match first_error {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Send a message through one provider. The provider must be started.
    pub async fn send(&self, id: &str, msg: &SendMessage) -> Result<SendReceipt, ProviderError> {
        if !self.started.contains(id) {
            return Err(ProviderError::Protocol(format!(
                "provider not started: {id} (call listen first)"
            )));
        }
        let provider = self
            .get(id)
            .ok_or_else(|| ProviderError::Protocol(format!("unknown provider: {id}")))?;
        provider.send(msg).await
    }

    /// Forward a normalized inbound message to the shared event sink.
    pub fn dispatch_message(&self, msg: ChannelMessage) {
        self.events.on_message(msg);
    }

    /// Forward an asynchronous provider error to the shared event sink
    /// (emitted as `event.error` by the transport).
    pub fn dispatch_error(&self, provider: &str, error: &ProviderError) {
        self.events.on_error(provider, error);
    }
}
