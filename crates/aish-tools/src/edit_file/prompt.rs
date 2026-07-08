pub(crate) const DESCRIPTION: &str = "\
Edit a file by replacing exact text. Use after read_file when modifying existing files.";

pub(crate) const PROMPT: &str = r#"Use this tool to make exact string replacements in text files.

Usage:
- old_string must match exactly.
- Provide enough surrounding context when replacing a repeated string.
- Use replace_all only when every occurrence should change."#;

pub(crate) fn parameters() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "Path to the file."
            },
            "old_string": {
                "type": "string",
                "description": "Exact text to replace."
            },
            "new_string": {
                "type": "string",
                "description": "Replacement text."
            },
            "replace_all": {
                "type": "boolean",
                "description": "Replace all occurrences. Defaults to false."
            }
        },
        "required": ["path", "old_string", "new_string"]
    })
}
