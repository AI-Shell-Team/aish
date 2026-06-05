pub(crate) const DESCRIPTION: &str = "Submit the final answer to the user's question.";

pub(crate) const PROMPT: &str = r#"Use this tool when the task is complete and the final answer is ready.

Usage:
- Put the complete user-facing answer in answer.
- Do not call this tool while more tool work is still needed."#;

pub(crate) fn parameters() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "answer": {
                "type": "string",
                "description": "Complete final answer to present to the user."
            }
        },
        "required": ["answer"]
    })
}
