pub(crate) const ENTER_DESCRIPTION: &str = "\
Enter plan mode for multi-step work that needs an approvable plan artifact under \
`.aish/plans/` before execution. Requires user approval to proceed after planning. \
For text-only plans, runbooks, or advice without a saved artifact, use \
Agent(subagent_type=plan) instead.";

pub(crate) const EXIT_DESCRIPTION: &str =
    "Exit plan mode and present the plan for approval and review.";
pub(crate) const TEMPLATES_DESCRIPTION: &str = "List available plan templates.";

pub(crate) const ENTER_PROMPT: &str = r#"Use this tool when implementation must wait on a user-approved plan file.

Usage:
- Enter plan mode before making changes for multi-step or risky work.
- During planning, use read-only tools plus write_file/edit_file for the plan artifact only.
- Exit plan mode when the plan is ready for user approval.

## When NOT to Use
- Text-only plan, runbook, or "plan only, do not execute" → Agent(subagent_type=plan)
- Pure exploration or codebase search → Agent(subagent_type=explore)
- Simple one-step tasks that do not need a saved plan artifact"#;

pub(crate) const EXIT_PROMPT: &str = r#"Use this tool when the plan is ready for review.

Usage:
- Ensure the plan artifact is complete before exiting plan mode.
- Include a concise summary when helpful.
- If feedback was provided, address it before re-submitting."#;

pub(crate) const TEMPLATES_PROMPT: &str =
    r#"Use this tool to inspect available plan templates before writing a plan artifact."#;

pub(crate) fn enter_parameters() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "topic": {
                "type": "string",
                "description": "Topic or task to plan."
            },
            "summary": {
                "type": "string",
                "description": "Optional brief summary of the planning goal."
            }
        },
        "required": ["topic"]
    })
}

pub(crate) fn exit_parameters() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "summary": {
                "type": "string",
                "description": "Optional brief summary of the plan."
            },
            "feedback": {
                "type": "string",
                "description": "Optional feedback from the user when changes are requested. Injected by the session layer."
            },
            "plan_content": {
                "type": "string",
                "description": "Optional full plan content for review. Injected by the session layer."
            }
        }
    })
}

pub(crate) fn templates_parameters() -> serde_json::Value {
    serde_json::json!({"type": "object", "properties": {}})
}
