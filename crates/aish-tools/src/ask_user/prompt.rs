pub(crate) const DESCRIPTION: &str = "Ask the user a focused clarifying question.";

pub(crate) const PROMPT: &str = r#"Use this tool only when a small amount of user input is needed to continue.

Usage:
- Ask one focused question at a time.
- Prefer options when the likely answers are known.
- Use allow_freeform_input=false only when the user must choose from the provided options.
- Do not ask for secrets such as passwords, API keys, or tokens."#;

pub(crate) fn parameters() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "prompt": {
                "type": "string",
                "description": "The question to ask the user."
            },
            "options": {
                "type": "array",
                "description": "Optional choices to offer the user. If omitted or empty, ask_user uses text input.",
                "items": {
                    "type": "object",
                    "properties": {
                        "value": {
                            "type": "string",
                            "description": "Stable option value returned in tool metadata."
                        },
                        "label": {
                            "type": "string",
                            "description": "User-facing label shown in the choice list."
                        },
                        "description": {
                            "type": "string",
                            "description": "Optional extra detail shown below the option."
                        },
                        "recommended": {
                            "type": "boolean",
                            "description": "Mark this option visibly as the recommended default."
                        }
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
            "placeholder": {
                "type": "string",
                "description": "Placeholder text."
            },
            "allow_freeform_input": {
                "type": "boolean",
                "description": "When options are present, allow the user to choose Other and type a custom answer. Defaults to true.",
                "default": true
            },
            "required": {
                "type": "boolean",
                "description": "Whether the user must provide an answer. Defaults to true.",
                "default": true
            },
            "allow_cancel": {
                "type": "boolean",
                "description": "Whether the user can cancel or skip the question. Defaults to true.",
                "default": true
            },
            "min_length": {
                "type": "integer",
                "minimum": 0,
                "description": "Minimum length for text input. Defaults to 0.",
                "default": 0
            }
        },
        "required": ["prompt"]
    })
}
