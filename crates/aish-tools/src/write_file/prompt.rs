pub(crate) const DESCRIPTION: &str = "Write text content to a file.";

pub(crate) const PROMPT: &str = r#"Use this tool to create or overwrite a text file.

Usage:
- Provide the destination path and full content.
- Parent directories are created when needed.
- Prefer edit_file for targeted changes to existing files."#;

pub(crate) fn parameters() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "Path to the file to write."
            },
            "content": {
                "type": "string",
                "description": "Content to write."
            }
        },
        "required": ["path", "content"]
    })
}
