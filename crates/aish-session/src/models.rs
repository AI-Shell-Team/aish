use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A persisted session record stored in SQLite.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub session_uuid: String,
    pub created_at: DateTime<Utc>,
    pub model: String,
    pub api_base: Option<String>,
    pub run_user: Option<String>,
    pub state: serde_json::Value,
}

/// A single command history entry associated with a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub id: Option<i64>,
    pub session_uuid: String,
    pub command: String,
    /// Origin of the command: "user", "ai", or "builtin".
    pub source: String,
    pub returncode: Option<i32>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub created_at: DateTime<Utc>,
}
