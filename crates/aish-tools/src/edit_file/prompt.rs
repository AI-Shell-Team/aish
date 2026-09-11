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
  file changed since you read it the edit is rejected and you must re-read.
- For large files (>256 KiB), use start_line and end_line to confine the
  replacement to a specific line range — the only way to edit files larger
  than 256 KiB. Lines are 1-based and inclusive.
  When start_line is given, old_string must match within lines start_line..=end_line only."#;

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
            },
            "start_line": {
                "type": "integer",
                "description": "Start line of the edit range (1-based, inclusive). Use for large files: only lines start_line..=end_line are searched. If omitted, the whole file is searched.",
                "minimum": 1
            },
            "end_line": {
                "type": "integer",
                "description": "End line of the edit range (1-based, inclusive). Defaults to start_line if omitted.",
                "minimum": 1
            }
        },
        "required": ["path", "old_string", "new_string"]
    })
}
