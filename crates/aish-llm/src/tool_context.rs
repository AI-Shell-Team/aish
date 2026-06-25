use crate::session::LlmSession;

/// Tool execution policy for the current session context.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ToolExecutionPolicy {
    /// When true, bash preflight blocks commands that are not read-only.
    pub enforce_read_only_bash: bool,
}

/// Context passed to tools during preflight and execution.
pub struct ToolContext<'a> {
    pub session: &'a LlmSession,
    pub parent: Option<&'a LlmSession>,
    pub policy: ToolExecutionPolicy,
}

impl<'a> ToolContext<'a> {
    pub fn for_session(session: &'a LlmSession) -> Self {
        Self {
            session,
            parent: None,
            policy: session.tool_execution_policy(),
        }
    }

    pub fn with_policy(session: &'a LlmSession, policy: ToolExecutionPolicy) -> Self {
        Self {
            session,
            parent: None,
            policy,
        }
    }
}
