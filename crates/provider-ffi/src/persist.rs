//! Persistence stub (feature `persist`).
//!
//! Future sqlite-backed at-least-once log for `GET /api/events?since=cursor`
//! replay. Behind `persist` so the lean cdylib does not pull `rusqlite` by
//! default. Mirrors the phase 08 `sqlite feature for message log` scope.

#[cfg(feature = "persist")]
pub mod sqlite_log {
    /// Placeholder — real implementation would open `sqlite` and append
    /// `ChannelMessage` rows with a monotonic cursor.
    #[derive(Debug, Default)]
    pub struct SqliteLog {
        _path: String,
    }

    impl SqliteLog {
        pub fn open(_path: impl Into<String>) -> Result<Self, String> {
            Err("persist feature: sqlite backend not yet implemented (stub)".into())
        }
        pub fn append(&self, _json: &str) -> Result<u64, String> {
            Err("stub".into())
        }
        pub fn replay_since(&self, _cursor: u64) -> Vec<String> {
            Vec::new()
        }
    }
}

#[cfg(not(feature = "persist"))]
pub mod sqlite_log {
    /// Stub: compiled without `persist` — all ops are no-ops.
    #[derive(Debug, Default)]
    pub struct SqliteLog;
    impl SqliteLog {
        pub fn open(_path: impl Into<String>) -> Result<Self, String> {
            Err("pc built without --features persist".into())
        }
    }
}
