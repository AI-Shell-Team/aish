pub(crate) const DESCRIPTION: &str = "Search file contents using a regex pattern.";

pub(crate) const PROMPT: &str = r#"Use this tool to search text inside files.

Usage:
- Use regex patterns for content search.
- Use root to limit the directory being searched.
- Use include to restrict matches to file names such as *.rs or *.py.
- Use glob when you only need matching file paths."#;

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
