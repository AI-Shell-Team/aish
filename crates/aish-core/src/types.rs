use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Command execution
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommandStatus {
    Success,
    Cancelled,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandResult {
    pub status: CommandStatus,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub offload: Option<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Security
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SandboxOffAction {
    Allow,
    Confirm,
    Block,
}

// ---------------------------------------------------------------------------
// Memory
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryCategory {
    Preference,
    Environment,
    Solution,
    Pattern,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryType {
    Llm,
    Shell,
    Knowledge,
}

// ---------------------------------------------------------------------------
// Skills
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SkillSource {
    Local,
    User,
    Claude,
    Builtin,
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolPreflightAction {
    Execute,
    Confirm,
    ShortCircuit,
}

// ---------------------------------------------------------------------------
// Plan mode
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanPhase {
    Normal,
    Planning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanApprovalStatus {
    Draft,
    AwaitingUser,
    ChangesRequested,
    Approved,
    Cancelled,
}

impl std::fmt::Display for PlanApprovalStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlanApprovalStatus::Draft => write!(f, "draft"),
            PlanApprovalStatus::AwaitingUser => write!(f, "awaiting_user"),
            PlanApprovalStatus::ChangesRequested => write!(f, "changes_requested"),
            PlanApprovalStatus::Approved => write!(f, "approved"),
            PlanApprovalStatus::Cancelled => write!(f, "cancelled"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanModeState {
    pub phase: PlanPhase,
    pub plan_id: Option<String>,
    pub artifact_path: Option<String>,
    pub draft_revision: i32,
    pub approval_status: PlanApprovalStatus,
    pub summary: Option<String>,
    pub approved_artifact_path: Option<String>,
    pub approved_revision: Option<i32>,
    pub approved_artifact_hash: Option<String>,
    pub approval_feedback_summary: Option<String>,
    pub source_session_uuid: String,
    pub updated_at: String,
}

impl Default for PlanModeState {
    fn default() -> Self {
        Self {
            phase: PlanPhase::Normal,
            plan_id: None,
            artifact_path: None,
            draft_revision: 0,
            approval_status: PlanApprovalStatus::Draft,
            summary: None,
            approved_artifact_path: None,
            approved_revision: None,
            approved_artifact_hash: None,
            approval_feedback_summary: None,
            source_session_uuid: String::new(),
            updated_at: String::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// LLM events
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LlmEventType {
    OpStart,
    OpEnd,
    GenerationStart,
    GenerationEnd,
    ContentDelta,
    ReasoningStart,
    ReasoningDelta,
    ReasoningEnd,
    ToolExecutionStart,
    ToolExecutionEnd,
    Error,
    ToolConfirmationRequired,
    InteractionRequired,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmEvent {
    pub event_type: LlmEventType,
    pub data: serde_json::Value,
    pub timestamp: f64,
    pub metadata: Option<serde_json::Value>,
}
