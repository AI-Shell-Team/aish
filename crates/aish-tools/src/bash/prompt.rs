pub(crate) const DESCRIPTION: &str = "Execute a bash command and return the output.";

pub(crate) const PROMPT: &str = r#"Use this tool to run shell commands.

Usage:
- Explain non-trivial commands before running them.
- Prefer read_file, grep, or glob when those tools directly match the task.
- Use timeout only when a bounded runtime is expected.
- Do not retry commands the user rejected or cancelled."#;

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
