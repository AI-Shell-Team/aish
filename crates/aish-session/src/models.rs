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
    /// Tool calls attached to an assistant message. Optional so snapshots
    /// persisted before this field existed still deserialize.
    #[serde(default)]
    pub tool_calls: Option<Vec<aish_core::ContextToolCall>>,
    /// Reasoning content echoed back by reasoning-model providers.
    #[serde(default)]
    pub reasoning_content: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionStateSnapshot {
    pub cwd: Option<String>,
    pub summary_preview: Option<String>,
    #[serde(default)]
    pub context_messages_snapshot: Vec<SessionContextMessage>,
    pub updated_at: Option<DateTime<Utc>>,
    /// Task-level cumulative budget counters (issue #569), persisted so a
    /// resumed session keeps accruing against the same task budget instead
    /// of restarting it. Pure data: the runtime `Instant` anchor is
    /// process-local and is never serialized.
    #[serde(default)]
    pub task_budget: Option<TaskBudgetSnapshot>,
}

/// Serializable snapshot of the task budget state (issue #569).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TaskBudgetSnapshot {
    pub task_rounds: u64,
    pub tool_calls: u64,
    pub active_secs: u64,
    pub total_input: u64,
    pub total_output: u64,
    pub total_cache_read: u64,
    pub total_cache_write: u64,
    pub request_count: u64,
    /// Upper bounds persisted alongside the counters so resuming cannot
    /// bypass the budget with a config that no longer matches.
    pub max_rounds: Option<u64>,
    pub max_tool_calls: Option<u64>,
    pub max_tokens: Option<u64>,
    pub max_duration_secs: Option<u64>,
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
    /// Upper-bound timestamp filter (inclusive). Events at or before this
    /// time are returned.
    pub until: Option<DateTime<Utc>>,
    /// Restrict to events belonging to this session UUID.
    pub session_uuid: Option<String>,
    pub limit: usize,
}

impl Default for AuditQuery {
    fn default() -> Self {
        Self {
            user: None,
            host: None,
            event_type: None,
            since: None,
            until: None,
            session_uuid: None,
            limit: 100,
        }
    }
}

impl AuditQuery {
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(test)]
mod task_budget_snapshot_tests {
    use super::*;

    #[test]
    fn task_budget_snapshot_round_trips_through_json() {
        let snapshot = SessionStateSnapshot {
            cwd: Some("/tmp".into()),
            summary_preview: None,
            context_messages_snapshot: Vec::new(),
            updated_at: None,
            task_budget: Some(TaskBudgetSnapshot {
                task_rounds: 25,
                tool_calls: 31,
                active_secs: 95,
                total_input: 12_345,
                total_output: 6_789,
                total_cache_read: 999,
                total_cache_write: 111,
                request_count: 25,
                max_rounds: Some(200),
                max_tool_calls: None,
                max_tokens: Some(2_000_000),
                max_duration_secs: Some(3_600),
            }),
        };
        let json = serde_json::to_value(&snapshot).expect("serialize");
        // Legacy compatibility: an old snapshot without the field must parse.
        let legacy = serde_json::json!({"cwd": "/tmp"});
        let parsed_legacy: SessionStateSnapshot =
            serde_json::from_value(legacy).expect("legacy snapshot must parse");
        assert!(parsed_legacy.task_budget.is_none());

        let parsed: SessionStateSnapshot = serde_json::from_value(json).expect("deserialize");
        let budget = parsed.task_budget.expect("budget present");
        assert_eq!(budget.task_rounds, 25);
        assert_eq!(budget.tool_calls, 31);
        assert_eq!(budget.active_secs, 95);
        assert_eq!(budget.total_cache_read, 999);
        assert_eq!(budget.max_rounds, Some(200));
        assert_eq!(budget.max_duration_secs, Some(3_600));
    }
}
