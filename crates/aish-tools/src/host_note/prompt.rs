pub(crate) const DESCRIPTION: &str = "Store, list, or forget notes about the current remote host.";

pub(crate) const PROMPT: &str = r#"Use this tool for notes scoped to the current remote host.

Usage:
- Use store when the user provides durable facts about the host, services, paths, issues, or operational conventions.
- Use list before acting when host-specific context may matter.
- Use forget with a keyword to remove stale or incorrect notes."#;

pub(crate) fn parameters() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "action": {
                "type": "string",
                "enum": ["store", "list", "forget"],
                "description": "Host note operation to perform."
            },
            "content": {
                "type": "string",
                "description": "Note content to save for the store action."
            },
            "keyword": {
                "type": "string",
                "description": "Keyword used to match notes for the forget action."
            }
        },
        "required": ["action"]
    })
}
