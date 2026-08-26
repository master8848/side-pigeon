//! Persistence for the FFI cdylib (feature `persist`).
//!
//! Delegates to `provider-transport`'s WAL SQLite `EventLog` so the cdylib
//! and `pc serve` share the exact same on-disk format. Behind `persist` so
//! the lean cdylib stays lean.

#[cfg(feature = "persist")]
pub mod sqlite_log {
    use std::path::Path;

    use provider_transport::jsonrpc::Notification;
    use provider_transport::persist::EventLog as TransportLog;
    use serde_json::Value;

    /// Thin wrapper over `provider_transport::persist::EventLog` that exposes
    /// the `SqliteLog` API expected by the FFI `persist` module, while
    /// delegating all persistence to the transport crate's SQLite impl.
    pub struct SqliteLog {
        inner: TransportLog,
    }

    impl std::fmt::Debug for SqliteLog {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("SqliteLog").finish_non_exhaustive()
        }
    }

    impl SqliteLog {
        /// Open (or create) the DB at `path`.
        pub fn open(path: impl AsRef<Path>) -> Result<Self, String> {
            let inner = TransportLog::open(path).map_err(|e| e.to_string())?;
            Ok(Self { inner })
        }

        /// Append one JSON-encoded notification/message. `json` is parsed as
        /// `serde_json::Value`; if it already looks like a JSON-RPC
        /// Notification (`{jsonrpc, method, params}`) it is stored as-is,
        /// otherwise it is wrapped as `event.message` with the value as
        /// `params.message` when the value is an object, or as `params` directly.
        pub fn append(&self, json: &str) -> Result<u64, String> {
            let v: Value = serde_json::from_str(json).map_err(|e| format!("invalid json: {e}"))?;
            // If already a Notification-shaped object, use it directly.
            let notif = if v.get("jsonrpc").is_some() && v.get("method").is_some() {
                let n: Notification =
                    serde_json::from_value(v).map_err(|e| format!("invalid notification: {e}"))?;
                n
            } else {
                // Wrap raw ChannelMessage / arbitrary value as event.message
                Notification::new("event.message", Some(v))
            };
            self.inner.append(&notif)
        }

        /// Append a typed `Notification` directly, returning its cursor.
        pub fn append_notification(&self, n: &Notification) -> Result<u64, String> {
            self.inner.append(n)
        }

        /// Replay payloads with `cursor > since` as JSON strings ordered ASC.
        pub fn replay_since(&self, cursor: u64) -> Vec<String> {
            self.replay_since_limit(cursor, None)
        }

        /// Replay with an optional row limit.
        /// S8: caps to 1000 even when None (defense-in-depth; inner also caps).
        pub fn replay_since_limit(&self, cursor: u64, limit: Option<u64>) -> Vec<String> {
            let capped = Some(limit.unwrap_or(1000).clamp(1, 1000));
            match self.inner.replay_since(cursor, capped) {
                Ok(rows) => rows.into_iter().map(|(_, v)| v.to_string()).collect(),
                Err(e) => {
                    tracing::warn!("SqliteLog replay_since failed: {e}");
                    Vec::new()
                }
            }
        }

        /// Typed replay returning cursors + values. Caps limit to 1000 (S8).
        pub fn replay_since_typed(
            &self,
            since: u64,
            limit: Option<u64>,
        ) -> Result<Vec<(u64, Value)>, String> {
            let capped = Some(limit.unwrap_or(1000).clamp(1, 1000));
            self.inner.replay_since(since, capped)
        }

        /// Latest cursor (0 if empty).
        pub fn latest_cursor(&self) -> Result<u64, String> {
            self.inner.latest_cursor()
        }
    }

    impl Default for SqliteLog {
        fn default() -> Self {
            // For Default we open an in-memory or temp path; but we delegate to
            // a temp file. Instead just panic with guidance — Default is only
            // for tests that will call open explicitly. Create a temp dir fallback.
            // Try to open an in-memory db via ":memory:" if underlying rusqlite supports.
            SqliteLog::open(":memory:").expect("in-memory sqlite open failed")
        }
    }
}

#[cfg(not(feature = "persist"))]
pub mod sqlite_log {
    /// Stub: compiled without `persist` — all ops error clearly.
    #[derive(Debug, Default)]
    pub struct SqliteLog;
    impl SqliteLog {
        pub fn open(_path: impl AsRef<std::path::Path>) -> Result<Self, String> {
            Err("pc built without --features persist (rebuild with --features persist)".into())
        }
        pub fn append(&self, _json: &str) -> Result<u64, String> {
            Err("pc built without --features persist".into())
        }
        pub fn replay_since(&self, _cursor: u64) -> Vec<String> {
            Vec::new()
        }
        pub fn latest_cursor(&self) -> Result<u64, String> {
            Err("pc built without --features persist".into())
        }
    }
}
