pub(crate) const DESCRIPTION: &str = "\
Invoke a skill within the main conversation. Use only skills listed in the current turn's \
system-reminder; do not guess skill names from memory.";

pub(crate) const PROMPT: &str = r#"Use this tool to invoke user-available skills.

Usage:
- Invoke a skill before answering when it directly matches the user's request.
- In `args`, pass the user's concrete task and any concise arguments needed by the selected skill. Skills that run in a sub-agent cannot see the parent conversation.
- Write skill outputs only to dedicated subdirectories under the current working directory when files are needed."#;

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
                "description": "The user's concrete task plus optional arguments for the skill. Required for skills that run in a sub-agent."
            }
        },
        "required": ["skill_name"]
    })
}
