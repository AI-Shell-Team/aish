pub(crate) const DESCRIPTION: &str = "Execute a bash command and return the output.";

pub(crate) const PROMPT: &str = r#"Use this tool to run shell commands in the current SSH-backed session.

Usage:
- Explain non-trivial commands before running them.
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
                "description": "Timeout in seconds.",
                "default": 120
            }
        },
        "required": ["command"]
    })
}
