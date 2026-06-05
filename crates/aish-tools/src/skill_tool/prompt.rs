pub(crate) const DESCRIPTION: &str = "Invoke a skill within the main conversation.";

pub(crate) const PROMPT: &str = r#"Use this tool to invoke user-available skills.

Usage:
- Invoke a skill before answering when it directly matches the user's request.
- Pass only concise arguments needed by the selected skill.
- Do not invent skill names; use only skills that are available in the current session."#;

pub(crate) fn parameters() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "skill_name": {
                "type": "string",
                "description": "Name of the skill to invoke."
            },
            "args": {
                "type": "string",
                "description": "Optional arguments for the skill."
            }
        },
        "required": ["skill_name"]
    })
}
