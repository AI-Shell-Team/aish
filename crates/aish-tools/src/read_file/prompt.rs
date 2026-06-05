pub(crate) const DESCRIPTION: &str = "Read text content from a file.";

pub(crate) const PROMPT: &str = r#"Use this tool to read text files.

Usage:
- Provide path to the file to read.
- Use offset and limit when you only need part of a larger file.
- Offset is a 0-based index; results display 1-based line numbers."#;

pub(crate) fn parameters() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "Path to the file to read."
            },
            "offset": {
                "type": "integer",
                "description": "Line offset to start reading from (0-based index). Results show 1-based line numbers.",
                "minimum": 0
            },
            "limit": {
                "type": "integer",
                "description": "Maximum number of lines to read.",
                "minimum": 1
            }
        },
        "required": ["path"]
    })
}
