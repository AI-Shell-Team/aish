use aish_core::{AuditEventType, MemoryType};
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
    /// UUID of the session this one was forked from (`None` for a root session).
    #[serde(default)]
    pub parent_session_uuid: Option<String>,
    /// History row id within the parent at which this branch diverges.
    #[serde(default)]
    pub branch_point_message_id: Option<i64>,
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

/// A persisted audit event row (maps to the `audit_events` table).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEventRecord {
    pub id: i64,
    pub ts: DateTime<Utc>,
    pub session_uuid: Option<String>,
    pub user: Option<String>,
    pub host: Option<String>,
    pub event_type: AuditEventType,
    pub command: Option<String>,
    pub source: Option<String>,
    pub return_code: Option<i32>,
    pub ai_tool: Option<String>,
    pub ai_args: Option<String>,
    pub ai_result: Option<String>,
    pub decision: Option<String>,
    pub user_choice: Option<String>,
    pub matched_rule: Option<String>,
    pub risk_level: Option<String>,
}

/// Optional filters for querying audit events.
#[derive(Debug, Clone)]
pub struct AuditQuery {
    pub user: Option<String>,
    pub host: Option<String>,
    pub event_type: Option<AuditEventType>,
    pub since: Option<DateTime<Utc>>,
    pub limit: usize,
}

impl Default for AuditQuery {
    fn default() -> Self {
        Self {
            user: None,
            host: None,
            event_type: None,
            since: None,
            limit: 100,
        }
    }
}

impl AuditQuery {
    pub fn new() -> Self {
        Self::default()
    }
}
