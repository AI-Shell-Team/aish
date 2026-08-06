pub(crate) const DESCRIPTION: &str = "\
Create or overwrite a text file — the correct way to create files. Never use \
bash (echo >, cat >, printf >, tee) to create files; use this tool instead.";

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
