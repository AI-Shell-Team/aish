pub(crate) const DESCRIPTION: &str = "\
Execute Python code and return the result. Prefer for scripted data processing, formatting, \
calculations, and multi-step logic instead of bash pipelines.";

pub(crate) const PROMPT: &str = r#"Use this tool for small Python snippets that are better expressed as code than shell pipelines.

Usage:
- Print values that should be returned to the conversation.
- Keep snippets focused and self-contained.
- Do not use this tool for long-running or interactive programs."#;

pub(crate) fn parameters() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "code": {
                "type": "string",
                "description": "Python code to execute."
            }
        },
        "required": ["code"]
    })
}
