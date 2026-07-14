use std::path::Path;

use chrono::Utc;
use rusqlite::params;
use tracing::debug;
use uuid::Uuid;

use aish_core::{AishError, AuditEvent, AuditEventType, AuditSink, Result};

use crate::models::{
    AuditEventRecord, AuditQuery, HistoryEntry, SessionRecord, SessionStateSnapshot,
};

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

CREATE TABLE IF NOT EXISTS audit_events (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    ts            TEXT NOT NULL,
    session_uuid  TEXT,
    user          TEXT,
    host          TEXT,
    event_type    TEXT NOT NULL,
    command       TEXT,
    source        TEXT,
    return_code   INTEGER,
    ai_tool       TEXT,
    ai_args       TEXT,
    ai_result     TEXT,
    decision      TEXT,
    user_choice   TEXT,
    matched_rule  TEXT,
    risk_level    TEXT
);

CREATE INDEX IF NOT EXISTS idx_audit_ts ON audit_events(ts);
CREATE INDEX IF NOT EXISTS idx_audit_user ON audit_events(user);
CREATE INDEX IF NOT EXISTS idx_audit_host ON audit_events(host);
CREATE INDEX IF NOT EXISTS idx_audit_event_type ON audit_events(event_type);
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

        // Set busy timeout so concurrent writes from separate connections
        // (e.g. SessionStore + AuditStore on the same file) retry instead of
        // failing immediately with SQLITE_BUSY.
        conn.execute_batch("PRAGMA busy_timeout=5000;")
            .map_err(|e| AishError::Session(format!("failed to set busy_timeout: {e}")))?;

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

    // -----------------------------------------------------------------
    // Audit events
    // -----------------------------------------------------------------

    /// Persist a single audit event and return its row id.
    pub fn add_audit_event(&self, event: &AuditEvent) -> Result<i64> {
        let ts_str = event.timestamp.to_rfc3339();
        self.conn
            .execute(
                "INSERT INTO audit_events
                 (ts, session_uuid, user, host, event_type,
                  command, source, return_code,
                  ai_tool, ai_args, ai_result,
                  decision, user_choice, matched_rule, risk_level)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
                params![
                    ts_str,
                    event.session_uuid,
                    event.user,
                    event.host,
                    event.event_type.to_string(),
                    event.command,
                    event.source,
                    event.return_code,
                    event.ai_tool,
                    event.ai_args,
                    event.ai_result,
                    event.decision,
                    event.user_choice,
                    event.matched_rule,
                    event.risk_level,
                ],
            )
            .map_err(|e| AishError::Session(format!("failed to insert audit event: {e}")))?;

        Ok(self.conn.last_insert_rowid())
    }

    /// Query audit events with optional filters, newest first.
    pub fn query_audit_events(&self, query: &AuditQuery) -> Result<Vec<AuditEventRecord>> {
        let mut sql = String::from(
            "SELECT id, ts, session_uuid, user, host, event_type,
                    command, source, return_code,
                    ai_tool, ai_args, ai_result,
                    decision, user_choice, matched_rule, risk_level
             FROM audit_events WHERE 1=1",
        );
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if let Some(ref user) = query.user {
            sql.push_str(" AND user = ?");
            params_vec.push(Box::new(user.clone()));
        }
        if let Some(ref host) = query.host {
            sql.push_str(" AND host = ?");
            params_vec.push(Box::new(host.clone()));
        }
        if let Some(ref event_type) = query.event_type {
            sql.push_str(" AND event_type = ?");
            params_vec.push(Box::new(event_type.to_string()));
        }
        if let Some(since) = query.since {
            sql.push_str(" AND ts >= ?");
            params_vec.push(Box::new(since.to_rfc3339()));
        }

        sql.push_str(" ORDER BY ts DESC LIMIT ?");
        params_vec.push(Box::new(query.limit as i64));

        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(|e| AishError::Session(format!("failed to prepare audit query: {e}")))?;

        let param_refs: Vec<&dyn rusqlite::ToSql> = params_vec.iter().map(|p| p.as_ref()).collect();

        let rows = stmt
            .query_map(param_refs.as_slice(), |row| {
                Ok(AuditEventRecord {
                    id: row.get(0)?,
                    ts: parse_datetime(&row.get::<_, String>(1)?),
                    session_uuid: row.get(2)?,
                    user: row.get(3)?,
                    host: row.get(4)?,
                    event_type: {
                        let raw = row.get::<_, String>(5)?;
                        raw.parse().unwrap_or_else(|_| {
                            tracing::warn!(raw = %raw, "unknown audit event_type in db, falling back to Command");
                            AuditEventType::Command
                        })
                    },
                    command: row.get(6)?,
                    source: row.get(7)?,
                    return_code: row.get(8)?,
                    ai_tool: row.get(9)?,
                    ai_args: row.get(10)?,
                    ai_result: row.get(11)?,
                    decision: row.get(12)?,
                    user_choice: row.get(13)?,
                    matched_rule: row.get(14)?,
                    risk_level: row.get(15)?,
                })
            })
            .map_err(|e| AishError::Session(format!("failed to query audit events: {e}")))?;

        let mut events = Vec::new();
        for row in rows {
            events.push(
                row.map_err(|e| AishError::Session(format!("failed to read audit row: {e}")))?,
            );
        }
        Ok(events)
    }
}

/// Thin wrapper that makes [`SessionStore`] usable as a thread-safe
/// [`AuditSink`].  Opens a **separate** SQLite connection to the same
/// database file — WAL mode allows multiple connections safely.
pub struct AuditStore(std::sync::Mutex<SessionStore>);

impl AuditStore {
    /// Open a dedicated connection for audit writes.
    pub fn open(path: Option<&Path>) -> Result<Self> {
        let store = SessionStore::open(path)?;
        Ok(Self(std::sync::Mutex::new(store)))
    }

    /// Query audit events (delegates to the inner [`SessionStore`]).
    pub fn query(&self, q: &AuditQuery) -> Result<Vec<AuditEventRecord>> {
        let guard = self
            .0
            .lock()
            .map_err(|e| AishError::Session(format!("audit store lock poisoned: {e}")))?;
        guard.query_audit_events(q)
    }
}

impl AuditSink for AuditStore {
    fn record(&self, event: AuditEvent) {
        let store = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(e) = store.add_audit_event(&event) {
            tracing::warn!(%e, "failed to write audit event");
        }
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

    #[test]
    fn audit_store_writes_and_queries_events() {
        use aish_core::{AuditEvent, AuditEventType, AuditSink};

        let temp = tempfile::tempdir().unwrap();
        let db_path = temp.path().join("audit.db");
        let audit = AuditStore::open(Some(&db_path)).unwrap();

        let now = Utc::now();
        audit.record(AuditEvent::command(
            now,
            Some("sess-1".into()),
            Some("root".into()),
            Some("prod-host".into()),
            "ls -la".into(),
            "user".into(),
            0,
        ));
        audit.record(AuditEvent::ai_tool(
            now,
            Some("sess-1".into()),
            Some("root".into()),
            Some("prod-host".into()),
            "bash".into(),
            r#"{"command":"rm -rf /tmp/x"}"#.into(),
            "done".into(),
        ));
        audit.record(AuditEvent::security_decision(
            now,
            Some("sess-1".into()),
            Some("root".into()),
            Some("prod-host".into()),
            Some("rm -rf /tmp".into()),
            "confirm".into(),
            Some("yes".into()),
            Some("H-001".into()),
            Some("HIGH".into()),
        ));

        let all = audit
            .query(&AuditQuery {
                limit: 100,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(all.len(), 3);

        let commands = audit
            .query(&AuditQuery {
                event_type: Some(AuditEventType::Command),
                limit: 100,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].command.as_deref(), Some("ls -la"));
        assert_eq!(commands[0].source.as_deref(), Some("user"));

        let user_filtered = audit
            .query(&AuditQuery {
                user: Some("root".into()),
                limit: 100,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(user_filtered.len(), 3);

        let other_user = audit
            .query(&AuditQuery {
                user: Some("nobody".into()),
                limit: 100,
                ..Default::default()
            })
            .unwrap();
        assert!(other_user.is_empty());
    }
}
