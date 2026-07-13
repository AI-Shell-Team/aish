pub(crate) const DESCRIPTION: &str = "\
Execute a bash command and return the output. Prefer for direct shell commands and scripts; \
use read_file, grep, or glob for file content; use Python for structured scripts; use Agent \
(subagent_type=explore) for open-ended multi-round path/code search; use Agent \
(subagent_type=troubleshoot) for open-ended system/service diagnosis; use Agent to delegate \
other isolated sub-tasks.";

pub(crate) const PROMPT: &str = r#"Use this tool to run shell commands.

Usage:
- Explain non-trivial commands before running them.
- Use timeout only when a bounded runtime is expected.
- Do not retry commands the user rejected or cancelled.
- Prefer read_file, grep, or glob for file content; use bash for one-shot commands or read-only \
discovery (find, ls, stat) when those tools are a better fit."#;

pub(crate) fn parameters() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "command": {
                "type": "string",
                "description": "Bash command to execute."
            },
            "timeout": {
                "type": "integer",
                "minimum": 1,
                "description": "Timeout in seconds. If omitted, the command runs until completion or cancellation."
            }
        },
        "required": ["command"]
    })
}
