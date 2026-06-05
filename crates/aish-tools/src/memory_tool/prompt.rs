pub(crate) const DESCRIPTION: &str = "Search, store, list, or forget long-term memories.";

pub(crate) const PROMPT: &str = r#"Use this tool for long-term memory operations.

Usage:
- Use search to recall relevant saved knowledge before acting on user preferences or prior context.
- Use store only for durable facts that are likely useful later.
- Use list to inspect recent memories.
- Use forget to remove outdated or incorrect memory entries by id."#;

pub(crate) fn parameters() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "action": {
                "type": "string",
                "enum": ["search", "store", "forget", "list"],
                "description": "Memory operation to perform."
            },
            "query": {
                "type": "string",
                "description": "Search query for the search action."
            },
            "content": {
                "type": "string",
                "description": "Content to store for the store action."
            },
            "category": {
                "type": "string",
                "enum": ["preference", "environment", "solution", "pattern", "other"],
                "description": "Category for stored memory. Defaults to other."
            },
            "memory_id": {
                "type": "integer",
                "description": "Memory id to forget for the forget action."
            }
        },
        "required": ["action"]
    })
}
