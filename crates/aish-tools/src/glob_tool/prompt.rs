pub(crate) const DESCRIPTION: &str = "Find files matching glob patterns.";

pub(crate) const PROMPT: &str = r#"Use this tool to enumerate file paths by glob pattern.

Usage:
- Prefer this tool when you need matching file names, not file contents.
- Use recursive patterns such as **/*.rs when searching across a tree.
- Use the root parameter to limit search scope when the user names a directory."#;

pub(crate) fn parameters() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "pattern": {
                "type": "string",
                "description": "Glob pattern such as **/*.py or src/**/*.md."
            },
            "root": {
                "type": "string",
                "description": "Optional search root directory. Defaults to the current working directory."
            }
        },
        "required": ["pattern"]
    })
}
