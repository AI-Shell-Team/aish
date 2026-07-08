pub(crate) const DESCRIPTION: &str = "\
Search file contents using a regex pattern. In the main session, prefer Agent(subagent_type=explore) \
for open-ended multi-round content search instead of many greps here. Inside a sub-agent, narrow \
scope with root/include and prefer glob first when paths are unknown.";

pub(crate) const PROMPT: &str = r#"Use this tool to search text inside files.

Usage:
- Use regex patterns for content search.
- When paths are unknown, use glob first to find candidate files, then grep with a tighter scope.
- Use root to limit the directory being searched.
- Use include to restrict matches to file names such as *.conf or *.yaml.
- Avoid repeating the same pattern across overlapping trees."#;

pub(crate) fn parameters() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "pattern": {
                "type": "string",
                "description": "Regex pattern to search for."
            },
            "root": {
                "type": "string",
                "description": "Optional search root directory. Defaults to the current working directory."
            },
            "include": {
                "type": "string",
                "description": "Optional glob filter for file names, e.g. *.py or *.rs."
            }
        },
        "required": ["pattern"]
    })
}
