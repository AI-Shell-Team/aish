pub(crate) const DESCRIPTION: &str = "\
Modify an existing file by replacing exact text — the correct way to edit \
files. Never use bash (sed -i, echo >, cat >>, tee) to modify files; always \
use this tool instead.";

pub(crate) const PROMPT: &str = r#"Use this tool to make exact string replacements in text files.

Usage:
- old_string must match exactly.
- Provide enough surrounding context when replacing a repeated string.
- Use replace_all only when every occurrence should change.
- Pass the tag from read_file's [path#TAG] header to anchor the edit; if the
  file changed since you read it the edit is rejected and you must re-read."#;

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
            },
            "tag": {
                "type": "string",
                "description": "Snapshot tag from read_file's [path#TAG] header. Anchors the edit: if the file changed since you read it, the edit is rejected."
            }
        },
        "required": ["path", "old_string", "new_string"]
    })
}
