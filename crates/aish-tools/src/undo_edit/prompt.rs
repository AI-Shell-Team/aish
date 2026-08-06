pub(crate) const DESCRIPTION: &str = "\
Undo the most recent edit_file or write_file change, restoring the file to its prior content.";

pub(crate) const PROMPT: &str = r#"Reverts the last file mutation made by edit_file or write_file.

Usage:
- With no args, undoes the most recent change across all files.
- Pass path to undo only the last change to that specific file.
- If the undone change created the file, undo deletes it."#;

pub(crate) fn parameters() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "Optional: undo only the last change to this path. Omit to undo the most recent change overall."
            }
        },
        "required": []
    })
}
