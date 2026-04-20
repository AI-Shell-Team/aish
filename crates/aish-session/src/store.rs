use std::path::Path;

use chrono::Utc;
use rusqlite::params;
use tracing::debug;
use uuid::Uuid;

use aish_core::{AishError, Result};

use crate::models::{HistoryEntry, SessionRecord};

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
        let state = serde_json::Value::Object(Default::default());
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

    /// List the most recent sessions, ordered by creation time descending.
    pub fn list_sessions(&self, limit: usize) -> Result<Vec<SessionRecord>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT session_uuid, created_at, model, api_base, run_user, state
             FROM sessions ORDER BY created_at DESC LIMIT ?1",
            )
            .map_err(|e| AishError::Session(format!("failed to prepare list_sessions: {e}")))?;

        let rows = stmt
            .query_map(params![limit], |row| {
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

        Ok(self.conn.last_insert_rowid())
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
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&chrono::Utc))
        .unwrap_or_else(|_| chrono::Utc::now())
}
