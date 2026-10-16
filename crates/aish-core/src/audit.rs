use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Audit event types
// ---------------------------------------------------------------------------

/// Category of an audit event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditEventType {
    /// A shell command was executed (user-typed or AI-triggered).
    Command,
    /// An AI tool was invoked via the function-calling loop.
    AiTool,
    /// A security decision was made (allow / confirm / block), optionally
    /// resolved by the user's yes/no confirmation.
    SecurityDecision,
}

impl std::fmt::Display for AuditEventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuditEventType::Command => write!(f, "command"),
            AuditEventType::AiTool => write!(f, "ai_tool"),
            AuditEventType::SecurityDecision => write!(f, "security_decision"),
        }
    }
}

impl std::str::FromStr for AuditEventType {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "command" => Ok(Self::Command),
            "ai_tool" => Ok(Self::AiTool),
            "security_decision" => Ok(Self::SecurityDecision),
            other => Err(format!("unknown audit event type: {other}")),
        }
    }
}

// ---------------------------------------------------------------------------
// Audit event
// ---------------------------------------------------------------------------

/// A single audit record.
///
/// Fields are optional because different event types populate different subsets.
/// All free-text fields **must** be redacted of secrets before reaching the sink.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    /// When the event occurred (UTC, RFC 3339).
    pub timestamp: DateTime<Utc>,
    /// Associated session UUID if available.
    pub session_uuid: Option<String>,
    /// Who triggered the event (username).
    pub user: Option<String>,
    /// Which machine (local hostname or SSH host_key).
    pub host: Option<String>,
    /// Event category.
    pub event_type: AuditEventType,
    // --- Command fields (event_type = Command) ---
    /// Command text that was executed.
    pub command: Option<String>,
    /// Origin: "user", "ai", or "builtin".
    pub source: Option<String>,
    /// Process exit code.
    pub return_code: Option<i32>,
    // --- AI tool fields (event_type = AiTool) ---
    /// Name of the invoked tool.
    pub ai_tool: Option<String>,
    /// Tool arguments (JSON string, redacted).
    pub ai_args: Option<String>,
    /// Tool result summary (redacted).
    pub ai_result: Option<String>,
    // --- Security decision fields (event_type = SecurityDecision) ---
    /// Decision made: "allow", "confirm", or "block".
    pub decision: Option<String>,
    /// User's confirmation choice: "yes" or "no" (null if no prompt was shown).
    pub user_choice: Option<String>,
    /// ID of the matched security rule.
    pub matched_rule: Option<String>,
    /// Risk level: "LOW", "MEDIUM", or "HIGH".
    pub risk_level: Option<String>,
    // --- Execution timing / outcome (any event type) ---
    /// Wall-clock duration of the audited operation, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Structured result status, free-form: normally the status the tool
    /// reported in its result meta (`command_status`: "ok", "timeout",
    /// "cancelled", "exit_nonzero", ...), otherwise the producer's own fallback
    /// "ok" / "error" for a tool that reports no status of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
}

impl AuditEvent {
    const MAX_FIELD_LEN: usize = 4096;

    fn truncate(mut s: String) -> String {
        if s.len() <= Self::MAX_FIELD_LEN {
            return s;
        }
        let mut end = Self::MAX_FIELD_LEN;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
        s.push_str("...(truncated)");
        s
    }

    /// Create a command-execution audit event.
    #[allow(clippy::too_many_arguments)]
    pub fn command(
        timestamp: DateTime<Utc>,
        session_uuid: Option<String>,
        user: Option<String>,
        host: Option<String>,
        command: String,
        source: String,
        return_code: i32,
    ) -> Self {
        Self {
            timestamp,
            session_uuid,
            user,
            host,
            event_type: AuditEventType::Command,
            command: Some(Self::truncate(command)),
            source: Some(source),
            return_code: Some(return_code),
            ai_tool: None,
            ai_args: None,
            ai_result: None,
            decision: None,
            user_choice: None,
            matched_rule: None,
            risk_level: None,
            duration_ms: None,
            outcome: None,
        }
    }

    /// Create an AI-tool-call audit event.
    #[allow(clippy::too_many_arguments)]
    pub fn ai_tool(
        timestamp: DateTime<Utc>,
        session_uuid: Option<String>,
        user: Option<String>,
        host: Option<String>,
        tool_name: String,
        args: String,
        result: String,
    ) -> Self {
        Self {
            timestamp,
            session_uuid,
            user,
            host,
            event_type: AuditEventType::AiTool,
            command: None,
            source: Some("ai".to_string()),
            return_code: None,
            ai_tool: Some(tool_name),
            ai_args: Some(Self::truncate(args)),
            ai_result: Some(Self::truncate(result)),
            decision: None,
            user_choice: None,
            matched_rule: None,
            risk_level: None,
            duration_ms: None,
            outcome: None,
        }
    }

    /// Create a security-decision audit event.
    #[allow(clippy::too_many_arguments)]
    pub fn security_decision(
        timestamp: DateTime<Utc>,
        session_uuid: Option<String>,
        user: Option<String>,
        host: Option<String>,
        command: Option<String>,
        decision: String,
        user_choice: Option<String>,
        matched_rule: Option<String>,
        risk_level: Option<String>,
    ) -> Self {
        Self {
            timestamp,
            session_uuid,
            user,
            host,
            event_type: AuditEventType::SecurityDecision,
            command: command.map(Self::truncate),
            source: None,
            return_code: None,
            ai_tool: None,
            ai_args: None,
            ai_result: None,
            decision: Some(decision),
            user_choice,
            matched_rule,
            risk_level,
            duration_ms: None,
            outcome: None,
        }
    }

    /// Attach the measured duration of the audited operation, in milliseconds.
    pub fn with_duration_ms(mut self, ms: u64) -> Self {
        self.duration_ms = Some(ms);
        self
    }

    /// Attach a structured result status (e.g. "ok", "error", "timeout",
    /// "cancelled", "exit_nonzero").
    pub fn with_outcome(mut self, outcome: impl Into<String>) -> Self {
        self.outcome = Some(outcome.into());
        self
    }
}

// ---------------------------------------------------------------------------
// Audit sink trait
// ---------------------------------------------------------------------------

/// Sink that persists audit events.
///
/// Implementations are expected to be cheap to clone (e.g. wrap `Arc` internally)
/// and thread-safe. A failed audit write should be logged but must not block the
/// operation being audited — audit is best-effort.
pub trait AuditSink: Send + Sync {
    /// Persist a single audit event. Errors should be handled internally.
    fn record(&self, event: AuditEvent);
}

/// No-op sink used when auditing is disabled.
#[derive(Debug, Clone, Default)]
pub struct NullAuditSink;

impl AuditSink for NullAuditSink {
    fn record(&self, _event: AuditEvent) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_type_round_trip() {
        for variant in [
            AuditEventType::Command,
            AuditEventType::AiTool,
            AuditEventType::SecurityDecision,
        ] {
            let s = variant.to_string();
            let back: AuditEventType = s.parse().unwrap();
            assert_eq!(variant, back);
        }
    }

    #[test]
    fn command_event_sets_correct_fields() {
        let ev = AuditEvent::command(
            Utc::now(),
            Some("s1".into()),
            Some("root".into()),
            Some("host1".into()),
            "ls -la".into(),
            "user".into(),
            0,
        );
        assert_eq!(ev.event_type, AuditEventType::Command);
        assert_eq!(ev.command.as_deref(), Some("ls -la"));
        assert_eq!(ev.source.as_deref(), Some("user"));
        assert_eq!(ev.return_code, Some(0));
        assert!(ev.ai_tool.is_none());
    }

    #[test]
    fn null_sink_swallows_events() {
        let sink = NullAuditSink;
        sink.record(AuditEvent::command(
            Utc::now(),
            None,
            None,
            None,
            "test".into(),
            "user".into(),
            0,
        ));
    }

    #[test]
    fn builders_attach_duration_and_outcome() {
        let ev = AuditEvent::ai_tool(
            Utc::now(),
            Some("s1".into()),
            None,
            None,
            "bash".into(),
            "{}".into(),
            "done".into(),
        )
        .with_duration_ms(1234)
        .with_outcome("exit_nonzero");
        assert_eq!(ev.duration_ms, Some(1234));
        assert_eq!(ev.outcome.as_deref(), Some("exit_nonzero"));

        // Constructors leave both fields empty by default.
        let plain =
            AuditEvent::command(Utc::now(), None, None, None, "ls".into(), "user".into(), 0);
        assert!(plain.duration_ms.is_none());
        assert!(plain.outcome.is_none());
    }

    #[test]
    fn legacy_json_without_new_fields_deserializes_to_none() {
        let json = r#"{
            "timestamp": "2026-01-01T00:00:00Z",
            "session_uuid": "s1",
            "user": "root",
            "host": "h1",
            "event_type": "command",
            "command": "ls -la",
            "source": "user",
            "return_code": 0,
            "ai_tool": null,
            "ai_args": null,
            "ai_result": null,
            "decision": null,
            "user_choice": null,
            "matched_rule": null,
            "risk_level": null
        }"#;
        let ev: AuditEvent = serde_json::from_str(json).unwrap();
        assert_eq!(ev.command.as_deref(), Some("ls -la"));
        assert!(ev.duration_ms.is_none());
        assert!(ev.outcome.is_none());
    }

    #[test]
    fn new_fields_are_skipped_when_none() {
        let ev = AuditEvent::command(Utc::now(), None, None, None, "ls".into(), "user".into(), 0);
        let json = serde_json::to_string(&ev).unwrap();
        assert!(!json.contains("duration_ms"));
        assert!(!json.contains("outcome"));
    }
}
