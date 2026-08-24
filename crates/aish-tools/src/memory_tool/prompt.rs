pub(crate) const DESCRIPTION: &str = "Search, store, list, or forget long-term memories.";

pub(crate) const PROMPT: &str = r#"Use this tool for long-term memory operations.

Usage:
- Use search to recall relevant saved knowledge before acting on user preferences or prior context.
- Use store only for durable facts that are likely useful later. Storing and forgetting require user confirmation.
- Use list to inspect recent memories, including expired ones.
- Use forget to remove outdated or incorrect memory entries by id. Forgetting requires user confirmation.
- scope: "user" (default) for personal facts, "host" for machine-specific facts, "project" for codebase facts.
- ttl_seconds: optional expiry. Expired entries are not injected into context. Use /memory to review expired entries.
- reason: briefly explain why the memory is being stored or forgotten."#;

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
            "scope": {
                "type": "string",
                "enum": ["user", "host", "project"],
                "description": "Scope of the memory. Defaults to user. Use host for machine-specific facts, project for codebase facts."
            },
            "ttl_seconds": {
                "type": "integer",
                "description": "Optional time-to-live in seconds. When set, the memory expires after this duration and is excluded from context injection. Use 604800 for 7 days, 2592000 for 30 days."
            },
            "reason": {
                "type": "string",
                "description": "Brief reason for storing or forgetting this memory. Shown in the confirmation prompt."
            },
            "memory_id": {
                "type": "integer",
                "description": "Memory id to forget for the forget action."
            }
        },
        "required": ["action"]
    })
}
