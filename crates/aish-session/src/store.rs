use std::path::Path;

use chrono::Utc;
use rusqlite::params;
use tracing::debug;
use uuid::Uuid;

use aish_core::{AishError, Result};

use crate::models::{HistoryEntry, SessionRecord, SessionStateSnapshot};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS sessions (
    session_uuid TEXT PRIMARY KEY,
    created_at   TEXT NOT NULL,
    model        TEXT NOT NULL,
    api_base     TEXT,
    run_user     TEXT,
    state        TEXT DEFAULT '{}'
);

CREATE TABLE IF NOT EXISTS history (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    session_uuid  TEXT NOT NULL,
    command       TEXT NOT NULL,
    source        TEXT NOT NULL,
    returncode    INTEGER,
    stdout        TEXT,
    stderr        TEXT,
    created_at    TEXT NOT NULL,
    FOREIGN KEY (session_uuid) REFERENCES sessions(session_uuid)
);

CREATE INDEX IF NOT EXISTS idx_history_session ON history(session_uuid);
CREATE INDEX IF NOT EXISTS idx_history_created ON history(created_at);
"#;

/// SQLite-backed store for sessions and command history.
pub struct SessionStore {
    conn: rusqlite::Connection,
}

impl SessionStore {
    /// Open (or create) the session database.
    ///
    /// When `path` is `None` the default location
    /// `~/.local/share/aish/sessions.db` is used.
    pub fn open(path: Option<&Path>) -> Result<Self> {
        let db_path = match path {
            Some(p) => p.to_path_buf(),
            None => {
                let base =
                    dirs::data_local_dir().unwrap_or_else(|| Path::new("/tmp").to_path_buf());
                base.join("aish").join("sessions.db")
            }
        };

        // Ensure parent directory exists.
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                AishError::Session(format!("failed to create session db directory: {e}"))
            })?;
        }

        let conn = rusqlite::Connection::open(&db_path).map_err(|e| {
            AishError::Session(format!("failed to open session db at {:?}: {e}", db_path))
        })?;

        // Enable WAL for better concurrent read performance.
        conn.execute_batch("PRAGMA journal_mode=WAL;")
            .map_err(|e| AishError::Session(format!("failed to enable WAL mode: {e}")))?;

        // Create tables.
        conn.execute_batch(SCHEMA)
            .map_err(|e| AishError::Session(format!("failed to create schema: {e}")))?;

        debug!(path = ?db_path, "opened session store");

        Ok(Self { conn })
    }

    /// Create a new session and persist it.
    pub fn create_session(&self, model: &str, api_base: Option<&str>) -> Result<SessionRecord> {
        let uuid = Uuid::new_v4().to_string();
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        let state = serde_json::to_value(SessionStateSnapshot {
            updated_at: Some(now),
            ..SessionStateSnapshot::default()
        })?;
        let state_str = serde_json::to_string(&state)?;
        let user = std::env::var("USER").ok();

        self.conn
            .execute(
                "INSERT INTO sessions (session_uuid, created_at, model, api_base, run_user, state)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![uuid, now_str, model, api_base, user, state_str],
            )
            .map_err(|e| AishError::Session(format!("failed to insert session: {e}")))?;

        Ok(SessionRecord {
            session_uuid: uuid,
            created_at: now,
            model: model.to_string(),
            api_base: api_base.map(|s| s.to_string()),
            run_user: user,
            state,
        })
    }

    /// Update the persisted state snapshot for a session.
    pub fn update_session_state(&self, uuid: &str, snapshot: &SessionStateSnapshot) -> Result<()> {
        let state_str = serde_json::to_string(snapshot)?;
        let updated = self
            .conn
            .execute(
                "UPDATE sessions SET state = ?2 WHERE session_uuid = ?1",
                params![uuid, state_str],
            )
            .map_err(|e| AishError::Session(format!("failed to update session state: {e}")))?;

        if updated == 0 {
            return Err(AishError::Session(format!(
                "session not found for state update: {uuid}"
            )));
        }

        Ok(())
    }

    /// Delete a session and its command history.
    pub fn delete_session(&self, uuid: &str) -> Result<()> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| AishError::Session(format!("failed to start delete transaction: {e}")))?;

        tx.execute("DELETE FROM history WHERE session_uuid = ?1", params![uuid])
            .map_err(|e| AishError::Session(format!("failed to delete session history: {e}")))?;
        tx.execute(
            "DELETE FROM sessions WHERE session_uuid = ?1",
            params![uuid],
        )
        .map_err(|e| AishError::Session(format!("failed to delete session: {e}")))?;
        tx.commit()
            .map_err(|e| AishError::Session(format!("failed to commit session delete: {e}")))?;
        Ok(())
    }

    /// Retrieve a session by its UUID.
    pub fn get_session(&self, uuid: &str) -> Result<Option<SessionRecord>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT session_uuid, created_at, model, api_base, run_user, state
             FROM sessions WHERE session_uuid = ?1",
            )
            .map_err(|e| AishError::Session(format!("failed to prepare get_session: {e}")))?;

        let result = stmt.query_row(params![uuid], |row| {
            Ok(SessionRecord {
                session_uuid: row.get(0)?,
                created_at: parse_datetime(&row.get::<_, String>(1)?),
                model: row.get(2)?,
                api_base: row.get(3)?,
                run_user: row.get(4)?,
                state: serde_json::from_str(&row.get::<_, String>(5)?)
                    .unwrap_or(serde_json::Value::Object(Default::default())),
            })
        });

        match result {
            Ok(record) => Ok(Some(record)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(AishError::Session(format!("failed to query session: {e}"))),
        }
    }

    /// List the most recent sessions, ordered by last state update descending.
    pub fn list_sessions(&self, limit: usize) -> Result<Vec<SessionRecord>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT session_uuid, created_at, model, api_base, run_user, state
             FROM sessions",
            )
            .map_err(|e| AishError::Session(format!("failed to prepare list_sessions: {e}")))?;

        let rows = stmt
            .query_map([], |row| {
                Ok(SessionRecord {
                    session_uuid: row.get(0)?,
                    created_at: parse_datetime(&row.get::<_, String>(1)?),
                    model: row.get(2)?,
                    api_base: row.get(3)?,
                    run_user: row.get(4)?,
                    state: serde_json::from_str(&row.get::<_, String>(5)?)
                        .unwrap_or(serde_json::Value::Object(Default::default())),
                })
            })
            .map_err(|e| AishError::Session(format!("failed to query sessions: {e}")))?;

        let mut sessions = Vec::new();
        for row in rows {
            sessions.push(
                row.map_err(|e| AishError::Session(format!("failed to read session row: {e}")))?,
            );
        }

        sessions.sort_by(|a, b| {
            let a_updated = a.state_snapshot().updated_at.unwrap_or(a.created_at);
            let b_updated = b.state_snapshot().updated_at.unwrap_or(b.created_at);
            b_updated.cmp(&a_updated)
        });
        sessions.truncate(limit);

        Ok(sessions)
    }

    /// Add a command history entry and return its row id.
    pub fn add_history_entry(&self, entry: &HistoryEntry) -> Result<i64> {
        let now_str = entry.created_at.to_rfc3339();

        self.conn.execute(
            "INSERT INTO history (session_uuid, command, source, returncode, stdout, stderr, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                entry.session_uuid,
                entry.command,
                entry.source,
                entry.returncode,
                entry.stdout,
                entry.stderr,
                now_str,
            ],
        ).map_err(|e| AishError::Session(
            format!("failed to insert history entry: {e}")
        ))?;

        if let Err(error) = self.touch_session(&entry.session_uuid, entry.created_at) {
            tracing::warn!(
                session_uuid = %entry.session_uuid,
                %error,
                "history row inserted but failed to update session timestamp"
            );
        }

        Ok(self.conn.last_insert_rowid())
    }

    fn touch_session(&self, uuid: &str, updated_at: chrono::DateTime<chrono::Utc>) -> Result<()> {
        if let Some(record) = self.get_session(uuid)? {
            let mut snapshot = record.state_snapshot();
            snapshot.updated_at = Some(updated_at);
            self.update_session_state(uuid, &snapshot)?;
        }
        Ok(())
    }

    /// Retrieve command history for a session, newest first.
    pub fn get_history(&self, session_uuid: &str, limit: usize) -> Result<Vec<HistoryEntry>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, session_uuid, command, source, returncode, stdout, stderr, created_at
             FROM history WHERE session_uuid = ?1
             ORDER BY created_at DESC LIMIT ?2",
            )
            .map_err(|e| AishError::Session(format!("failed to prepare get_history: {e}")))?;

        let rows = stmt
            .query_map(params![session_uuid, limit], |row| {
                Ok(HistoryEntry {
                    id: row.get(0)?,
                    session_uuid: row.get(1)?,
                    command: row.get(2)?,
                    source: row.get(3)?,
                    returncode: row.get(4)?,
                    stdout: row.get(5)?,
                    stderr: row.get(6)?,
                    created_at: parse_datetime(&row.get::<_, String>(7)?),
                })
            })
            .map_err(|e| AishError::Session(format!("failed to query history: {e}")))?;

        let mut entries = Vec::new();
        for row in rows {
            entries.push(
                row.map_err(|e| AishError::Session(format!("failed to read history row: {e}")))?,
            );
        }

        Ok(entries)
    }

    /// Close the database connection gracefully.
    pub fn close(self) -> Result<()> {
        self.conn
            .close()
            .map_err(|(_, e)| AishError::Session(format!("failed to close session db: {e}")))
    }
}

/// Parse an RFC 3339 datetime string, falling back to UTC now on failure.
fn parse_datetime(s: &str) -> chrono::DateTime<chrono::Utc> {
    let normalized = if let Some(prefix) = s.strip_suffix('Z') {
        format!("{}+00:00", prefix.replace('T', " "))
    } else if s.contains('T') {
        s.replace('T', " ")
    } else if s.contains('+') || s.rfind('-').is_some_and(|idx| idx > 10) {
        s.to_string()
    } else {
        format!("{s}+00:00")
    };

    chrono::DateTime::parse_from_str(&normalized, "%Y-%m-%d %H:%M:%S%.f%:z")
        .unwrap_or_else(|_| chrono::Utc::now().fixed_offset())
        .with_timezone(&chrono::Utc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{SessionContextMessage, SessionStateSnapshot};
    use aish_core::MemoryType;
    use chrono::{Datelike, Timelike};

    #[test]
    fn update_session_state_round_trips_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("sessions.db");
        let store = SessionStore::open(Some(&db_path)).unwrap();
        let record = store
            .create_session("test-model", Some("http://localhost"))
            .unwrap();
        let snapshot = SessionStateSnapshot {
            cwd: Some("/tmp".to_string()),
            summary_preview: Some("summary".to_string()),
            context_messages_snapshot: vec![SessionContextMessage {
                role: "user".to_string(),
                content: "hello".to_string(),
                memory_type: MemoryType::Llm,
                name: None,
                tool_call_id: None,
            }],
            updated_at: Some(Utc::now()),
        };

        store
            .update_session_state(&record.session_uuid, &snapshot)
            .unwrap();

        let loaded = store.get_session(&record.session_uuid).unwrap().unwrap();
        let loaded_snapshot = loaded.state_snapshot();
        assert_eq!(loaded_snapshot.cwd.as_deref(), Some("/tmp"));
        assert_eq!(loaded_snapshot.summary_preview.as_deref(), Some("summary"));
        assert_eq!(loaded_snapshot.context_messages_snapshot.len(), 1);
    }

    #[test]
    fn list_sessions_orders_by_snapshot_update_time() {
        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("sessions.db");
        let store = SessionStore::open(Some(&db_path)).unwrap();
        let older = store.create_session("older", None).unwrap();
        let newer = store.create_session("newer", None).unwrap();

        store
            .update_session_state(
                &older.session_uuid,
                &SessionStateSnapshot {
                    updated_at: Some(Utc::now() + chrono::Duration::seconds(60)),
                    ..SessionStateSnapshot::default()
                },
            )
            .unwrap();

        let sessions = store.list_sessions(2).unwrap();
        assert_eq!(sessions[0].session_uuid, older.session_uuid);
        assert_eq!(sessions[1].session_uuid, newer.session_uuid);
    }

    #[test]
    fn parse_datetime_supports_legacy_sqlite_format() {
        let parsed = parse_datetime("2026-05-18 09:55:56.246970");

        assert_eq!(parsed.year(), 2026);
        assert_eq!(parsed.month(), 5);
        assert_eq!(parsed.day(), 18);
        assert_eq!(parsed.hour(), 9);
        assert_eq!(parsed.minute(), 55);
        assert_eq!(parsed.second(), 56);
        assert_eq!(parsed.timestamp_subsec_micros(), 246970);
    }
}
