pub(crate) const DESCRIPTION: &str = "\
Find file paths by glob pattern. In the main session, prefer Agent(subagent_type=explore) for \
open-ended multi-round path discovery instead of many globs here. Inside a sub-agent, prefer one \
broad recursive pattern per root over many narrow globs on the same tree.";

pub(crate) const PROMPT: &str = r#"Use this tool to enumerate file paths by glob pattern.

Usage:
- Prefer matching file names, not file contents (use grep for content search).
- Use one broad recursive pattern (e.g. /etc/**/*ssh*) before trying many narrow patterns.
- Set root to limit scope when the task names a directory.
- Parallel globs are fine when roots or patterns are independent; do not repeat the same search."#;

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
