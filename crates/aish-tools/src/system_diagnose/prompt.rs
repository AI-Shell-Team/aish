pub(crate) const DESCRIPTION: &str = "Run an isolated system diagnosis agent.";

pub(crate) const PROMPT: &str = r#"Use this tool for deeper system diagnosis tasks.

Usage:
- Use it when the user asks why a system, service, filesystem, network, or performance issue is happening.
- Provide a concise query describing the symptoms and relevant context.
- Do not use it for simple one-command checks that can be handled directly."#;

pub(crate) fn parameters() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "query": {
                "type": "string",
                "description": "Diagnostic query or system issue to analyze."
            }
        },
        "required": ["query"]
    })
}
