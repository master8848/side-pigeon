//! At-least-once event log for `pc serve` (feature `persist`).
//!
//! When enabled, every `event.message` / `event.error` notification is
//! appended to a local SQLite DB (WAL mode). Clients that missed events while
//! offline can replay with `GET /api/events?since=<cursor>`.

#[cfg(feature = "persist")]
mod imp {
    use std::path::Path;
    use std::sync::Mutex;

    use rusqlite::Connection;
    use serde_json::Value;

    use crate::jsonrpc::Notification;

    /// Cursor-ordered SQLite log. Thread-safe via `Mutex<Connection>`.
    pub struct EventLog {
        conn: Mutex<Connection>,
    }

    impl EventLog {
        /// Open (or create) the DB at `path`. Creates parent dirs, enables
        /// WAL + NORMAL synchronous, and ensures the `events` table exists.
        pub fn open(path: impl AsRef<Path>) -> Result<Self, String> {
            let path = path.as_ref();
            let s = path.to_string_lossy();
            if s.contains("..") || s.contains(':') || s.contains('?') || s.contains("file:") {
                return Err(format!(
                    "refusing persist path with traversal/uri chars: {}",
                    path.display()
                ));
            }
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("create dir {}: {e}", parent.display()))?;
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let _ = std::fs::set_permissions(
                            parent,
                            std::fs::Permissions::from_mode(0o700),
                        );
                        if let Ok(meta) = std::fs::symlink_metadata(path) {
                            if meta.is_symlink() {
                                return Err(format!(
                                    "refusing persist path that is a symlink: {}",
                                    path.display()
                                ));
                            }
                        }
                        if let Ok(meta) = std::fs::symlink_metadata(parent) {
                            if meta.is_symlink() {
                                return Err(format!(
                                    "refusing persist parent that is a symlink: {}",
                                    parent.display()
                                ));
                            }
                        }
                    }
                    if let Ok(canonical_parent) = parent.canonicalize() {
                        if let Some(file_name) = path.file_name() {
                            let _expected = canonical_parent.join(file_name);
                            if let Ok(canonical_file) = path.canonicalize() {
                                if !canonical_file.starts_with(&canonical_parent) {
                                    return Err(format!(
                                        "refusing persist path traversal outside parent: {}",
                                        path.display()
                                    ));
                                }
                            }
                        }
                    }
                } else {
                    #[cfg(unix)]
                    {
                        if let Ok(meta) = std::fs::symlink_metadata(path) {
                            if meta.is_symlink() {
                                return Err(format!(
                                    "refusing persist path that is a symlink: {}",
                                    path.display()
                                ));
                            }
                        }
                    }
                }
            } else {
                #[cfg(unix)]
                {
                    if let Ok(meta) = std::fs::symlink_metadata(path) {
                        if meta.is_symlink() {
                            return Err(format!(
                                "refusing persist path that is a symlink: {}",
                                path.display()
                            ));
                        }
                    }
                }
            }
            let conn = Connection::open(path)
                .map_err(|e| format!("open sqlite {}: {e}", path.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
            }
            conn.execute_batch(
                "PRAGMA journal_mode=WAL;
                 PRAGMA synchronous=NORMAL;
                 PRAGMA journal_size_limit=33554432;
                 CREATE TABLE IF NOT EXISTS events (
                     cursor INTEGER PRIMARY KEY AUTOINCREMENT,
                     method TEXT NOT NULL,
                     payload TEXT NOT NULL,
                     ts INTEGER NOT NULL
                 ) STRICT;",
            )
            .map_err(|e| format!("init sqlite: {e}"))?;
            Ok(Self {
                conn: Mutex::new(conn),
            })
        }

        /// Append one notification, returning its monotonic cursor.
        /// S7: sync WAL insert ~0.5-5ms; mitigated by WAL+NORMAL+32M journal_size_limit.
        /// Ideal is mpsc writer thread / spawn_blocking at call-site (see state.rs).
        /// Call prune() periodically to bound file growth.
        pub fn append(&self, n: &Notification) -> Result<u64, String> {
            let payload =
                serde_json::to_string(n).map_err(|e| format!("serialize notification: {e}"))?;
            let ts = chrono_now_millis();
            let conn = self
                .conn
                .lock()
                .map_err(|e| format!("lock poisoned: {e}"))?;
            conn.execute(
                "INSERT INTO events (method, payload, ts) VALUES (?1, ?2, ?3)",
                rusqlite::params![n.method, payload, ts],
            )
            .map_err(|e| format!("insert event: {e}"))?;
            Ok(conn.last_insert_rowid() as u64)
        }

        /// Replay events with `cursor > since`, ordered ASC. `limit` caps rows.
        /// S8: hard LIMIT 1000 even when limit=None to prevent unbounded OOM.
        pub fn replay_since(
            &self,
            since: u64,
            limit: Option<u64>,
        ) -> Result<Vec<(u64, Value)>, String> {
            // Hard cap to prevent unbounded reads (S8). Matches http.rs capped_limit.
            let capped: u64 = limit.unwrap_or(1000).clamp(1, 1000);
            let conn = self
                .conn
                .lock()
                .map_err(|e| format!("lock poisoned: {e}"))?;
            let mut stmt = conn
                .prepare("SELECT cursor, payload FROM events WHERE cursor > ?1 ORDER BY cursor ASC LIMIT ?2")
                .map_err(|e| format!("prepare: {e}"))?;
            let rows = stmt
                .query_map(rusqlite::params![since as i64, capped as i64], |row| {
                    let c: i64 = row.get(0)?;
                    let p: String = row.get(1)?;
                    Ok((c as u64, p))
                })
                .map_err(|e| format!("query: {e}"))?;
            let mut out = Vec::new();
            for r in rows {
                let (c, s) = r.map_err(|e| format!("row: {e}"))?;
                let v: Value =
                    serde_json::from_str(&s).map_err(|e| format!("parse payload: {e}"))?;
                out.push((c, v));
            }
            Ok(out)
        }

        /// Latest cursor (0 if empty).
        pub fn latest_cursor(&self) -> Result<u64, String> {
            let conn = self
                .conn
                .lock()
                .map_err(|e| format!("lock poisoned: {e}"))?;
            let v: i64 = conn
                .query_row("SELECT COALESCE(MAX(cursor),0) FROM events", [], |r| {
                    r.get(0)
                })
                .map_err(|e| format!("max cursor: {e}"))?;
            Ok(v as u64)
        }

        /// Prune old rows, keeping only the last `keep_last` entries.
        /// S7: prevents unbounded file growth. Call periodically or on startup.
        pub fn prune(&self, keep_last: u64) -> Result<u64, String> {
            let conn = self
                .conn
                .lock()
                .map_err(|e| format!("lock poisoned: {e}"))?;
            let deleted = conn
                .execute(
                    "DELETE FROM events WHERE cursor <= (SELECT COALESCE(MAX(cursor),0) - ?1 FROM events)",
                    rusqlite::params![keep_last as i64],
                )
                .map_err(|e| format!("prune: {e}"))?;
            Ok(deleted as u64)
        }
    }

    fn chrono_now_millis() -> i64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }
}

#[cfg(not(feature = "persist"))]
mod imp {
    use serde_json::Value;

    use crate::jsonrpc::Notification;

    /// Stub when built without `persist` feature.
    pub struct EventLog;

    impl EventLog {
        /// Always fails — build with `--features persist`.
        pub fn open(_path: impl AsRef<std::path::Path>) -> Result<Self, String> {
            Err("built without --features persist".into())
        }
        /// Always fails — build with `--features persist`.
        pub fn append(&self, _n: &Notification) -> Result<u64, String> {
            Err("built without --features persist".into())
        }
        /// Always fails — build with `--features persist`.
        pub fn replay_since(
            &self,
            _since: u64,
            _limit: Option<u64>,
        ) -> Result<Vec<(u64, Value)>, String> {
            Err("built without --features persist".into())
        }
        /// Always fails — build with `--features persist`.
        pub fn latest_cursor(&self) -> Result<u64, String> {
            Err("built without --features persist".into())
        }
        /// Stub prune.
        pub fn prune(&self, _keep_last: u64) -> Result<u64, String> {
            Err("built without --features persist".into())
        }
    }
}

pub use imp::EventLog;
