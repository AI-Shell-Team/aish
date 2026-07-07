//! Prompt and schema for the `Agent` tool.

pub const DESCRIPTION: &str = "\
Spawn a synchronous sub-agent to handle an isolated sub-task. Only the final conclusion \
is returned to the parent session; intermediate tool output stays in the sub-session.\n\n\
Available subagent types:";

pub fn parameters() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "description": {
                "type": "string",
                "description": "3-5 word task summary for UI display"
            },
            "prompt": {
                "type": "string",
                "description": "Detailed task description for the sub-agent"
            },
            "subagent_type": {
                "type": "string",
                "enum": ["explore"],
                "description": "Built-in sub-agent type to spawn"
            }
        },
        "required": ["description", "prompt", "subagent_type"]
    })
}
