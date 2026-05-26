use aish_core::MemoryType;
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionContextMessage {
    pub role: String,
    pub content: String,
    pub memory_type: MemoryType,
    pub name: Option<String>,
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionStateSnapshot {
    pub cwd: Option<String>,
    pub summary_preview: Option<String>,
    #[serde(default)]
    pub context_messages_snapshot: Vec<SessionContextMessage>,
    pub updated_at: Option<DateTime<Utc>>,
}

impl SessionRecord {
    pub fn state_snapshot(&self) -> SessionStateSnapshot {
        match serde_json::from_value(self.state.clone()) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                tracing::warn!(%error, "failed to parse session state snapshot; using default");
                SessionStateSnapshot::default()
            }
        }
    }
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
