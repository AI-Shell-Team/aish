pub(crate) const DESCRIPTION: &str = "Ask the user a focused clarifying question.";

pub(crate) const PROMPT: &str = r#"Use this tool only when a small amount of user input is needed to continue.

Usage:
- Ask one focused question at a time.
- Prefer options when the likely answers are known.
- Use kind=text_input for open-ended answers.
- Use kind=choice_or_text when providing options while allowing a custom answer.
- Do not ask for secrets such as passwords, API keys, or tokens."#;

pub(crate) fn parameters() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "kind": {
                "type": "string",
                "enum": ["text_input", "choice_or_text"],
                "description": "Interaction type: text_input for free-form, choice_or_text for options with custom input.",
                "default": "text_input"
            },
            "prompt": {
                "type": "string",
                "description": "Question to ask the user."
            },
            "options": {
                "type": "array",
                "description": "Predefined options for choice_or_text.",
                "items": {
                    "type": "object",
                    "properties": {
                        "value": {"type": "string"},
                        "label": {"type": "string"},
                        "description": {"type": "string"}
                    },
                    "required": ["value", "label"]
                }
            },
            "title": {
                "type": "string",
                "description": "Optional title for the question."
            },
            "default": {
                "type": "string",
                "description": "Default value."
            },
            "allow_cancel": {
                "type": "boolean",
                "description": "Whether the user can cancel or skip. Defaults to true.",
                "default": true
            },
            "min_length": {
                "type": "integer",
                "description": "Minimum length for text input. Defaults to 0.",
                "default": 0
            }
        },
        "required": ["prompt"]
    })
}
