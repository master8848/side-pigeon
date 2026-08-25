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
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent).map_err(|e| format!("create dir {}: {e}", parent.display()))?;
                }
            }
            let conn = Connection::open(path).map_err(|e| format!("open sqlite {}: {e}", path.display()))?;
            conn.execute_batch(
                "PRAGMA journal_mode=WAL;
                 PRAGMA synchronous=NORMAL;
                 CREATE TABLE IF NOT EXISTS events (
                     cursor INTEGER PRIMARY KEY AUTOINCREMENT,
                     method TEXT NOT NULL,
                     payload TEXT NOT NULL,
                     ts INTEGER NOT NULL
                 ) STRICT;",
            )
            .map_err(|e| format!("init sqlite: {e}"))?;
            Ok(Self { conn: Mutex::new(conn) })
        }

        /// Append one notification, returning its monotonic cursor.
        pub fn append(&self, n: &Notification) -> Result<u64, String> {
            let payload = serde_json::to_string(n).map_err(|e| format!("serialize notification: {e}"))?;
            let ts = chrono_now_millis();
            let conn = self.conn.lock().map_err(|e| format!("lock poisoned: {e}"))?;
            conn.execute(
                "INSERT INTO events (method, payload, ts) VALUES (?1, ?2, ?3)",
                rusqlite::params![n.method, payload, ts],
            )
            .map_err(|e| format!("insert event: {e}"))?;
            Ok(conn.last_insert_rowid() as u64)
        }

        /// Replay events with `cursor > since`, ordered ASC. `limit` caps rows.
        pub fn replay_since(&self, since: u64, limit: Option<u64>) -> Result<Vec<(u64, Value)>, String> {
            let conn = self.conn.lock().map_err(|e| format!("lock poisoned: {e}"))?;
            let mut out = Vec::new();
            if let Some(l) = limit {
                let mut stmt = conn
                    .prepare("SELECT cursor, payload FROM events WHERE cursor > ?1 ORDER BY cursor ASC LIMIT ?2")
                    .map_err(|e| format!("prepare: {e}"))?;
                let rows = stmt
                    .query_map(rusqlite::params![since as i64, l as i64], |row| {
                        let c: i64 = row.get(0)?;
                        let p: String = row.get(1)?;
                        Ok((c as u64, p))
                    })
                    .map_err(|e| format!("query: {e}"))?;
                for r in rows {
                    let (c, s) = r.map_err(|e| format!("row: {e}"))?;
                    let v: Value = serde_json::from_str(&s).map_err(|e| format!("parse payload: {e}"))?;
                    out.push((c, v));
                }
            } else {
                let mut stmt = conn
                    .prepare("SELECT cursor, payload FROM events WHERE cursor > ?1 ORDER BY cursor ASC")
                    .map_err(|e| format!("prepare: {e}"))?;
                let rows = stmt
                    .query_map(rusqlite::params![since as i64], |row| {
                        let c: i64 = row.get(0)?;
                        let p: String = row.get(1)?;
                        Ok((c as u64, p))
                    })
                    .map_err(|e| format!("query: {e}"))?;
                for r in rows {
                    let (c, s) = r.map_err(|e| format!("row: {e}"))?;
                    let v: Value = serde_json::from_str(&s).map_err(|e| format!("parse payload: {e}"))?;
                    out.push((c, v));
                }
            }
            Ok(out)
        }

        /// Latest cursor (0 if empty).
        pub fn latest_cursor(&self) -> Result<u64, String> {
            let conn = self.conn.lock().map_err(|e| format!("lock poisoned: {e}"))?;
            let v: i64 = conn
                .query_row("SELECT COALESCE(MAX(cursor),0) FROM events", [], |r| r.get(0))
                .map_err(|e| format!("max cursor: {e}"))?;
            Ok(v as u64)
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
        pub fn replay_since(&self, _since: u64, _limit: Option<u64>) -> Result<Vec<(u64, Value)>, String> {
            Err("built without --features persist".into())
        }
        /// Always fails — build with `--features persist`.
        pub fn latest_cursor(&self) -> Result<u64, String> {
            Err("built without --features persist".into())
        }
    }
}

pub use imp::EventLog;
