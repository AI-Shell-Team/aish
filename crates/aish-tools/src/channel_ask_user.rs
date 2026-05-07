//! Channel-based ask_user tool for SSH sessions.
//!
//! Instead of using `inquire` (which requires direct terminal control), this
//! tool communicates with the forwarding loop via channels. When the LLM
//! calls `ask_user`, the tool sends the question through a channel and blocks
//! until the forwarding loop provides the user's answer.

use aish_llm::{Tool, ToolResult};
use aish_pty::{AskUserAnswer, AskUserOption, AskUserRequest, AiEvent};

/// Shared translated description — same as AskUserTool.
static DESCRIPTION: std::sync::OnceLock<String> = std::sync::OnceLock::new();

fn get_description() -> &'static str {
    DESCRIPTION.get_or_init(|| aish_i18n::t("tools.ask_user.description"))
}

pub struct ChannelAskUserTool {
    question_sender: std::sync::mpsc::Sender<AiEvent>,
    answer_receiver: std::sync::Mutex<std::sync::mpsc::Receiver<AskUserAnswer>>,
}

impl ChannelAskUserTool {
    pub fn new(
        question_sender: std::sync::mpsc::Sender<AiEvent>,
        answer_receiver: std::sync::mpsc::Receiver<AskUserAnswer>,
    ) -> Self {
        Self {
            question_sender,
            answer_receiver: std::sync::Mutex::new(answer_receiver),
        }
    }

    fn send_and_wait(&self, request: AskUserRequest) -> ToolResult {
        if self
            .question_sender
            .send(AiEvent::AskUser(request))
            .is_err()
        {
            return ToolResult::error("Channel closed");
        }

        match self.answer_receiver.lock().unwrap().recv() {
            Ok(AskUserAnswer::Response(answer)) => {
                // Match local AskUserTool: prefix with "用户输入: " via i18n
                let mut args_map = std::collections::HashMap::new();
                args_map.insert("input".to_string(), answer);
                ToolResult::success(aish_i18n::t_with_args(
                    "tools.ask_user.user_input_prefix",
                    &args_map,
                ))
            }
            Ok(AskUserAnswer::Cancelled) => {
                // Match local AskUserTool: cancelled returns success, not error
                ToolResult::success(aish_i18n::t("tools.ask_user.cancelled"))
            }
            Err(_) => ToolResult::error("Channel closed"),
        }
    }
}

impl Tool for ChannelAskUserTool {
    fn name(&self) -> &str {
        "ask_user"
    }

    fn description(&self) -> &str {
        get_description()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "kind": {
                    "type": "string",
                    "enum": ["text_input", "choice_or_text"],
                    "description": "Interaction type: text_input for free-form, choice_or_text for options with custom input"
                },
                "prompt": {
                    "type": "string",
                    "description": "The question to ask the user"
                },
                "options": {
                    "type": "array",
                    "description": "Predefined options for choice_or_text",
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
                    "description": "Optional title for the question"
                },
                "default": {
                    "type": "string",
                    "description": "Default value"
                },
                "allow_cancel": {
                    "type": "boolean",
                    "description": "Whether the user can cancel/skip (default: true)",
                    "default": true
                },
                "min_length": {
                    "type": "integer",
                    "description": "Minimum length for text input (default: 0)",
                    "default": 0
                }
            },
            "required": ["kind", "prompt"]
        })
    }

    fn execute(&self, args: serde_json::Value) -> ToolResult {
        let kind = args
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("text_input")
            .to_string();
        let prompt = match args.get("prompt").and_then(|v| v.as_str()) {
            Some(p) => p.to_string(),
            None => return ToolResult::error(aish_i18n::t("tools.ask_user.missing_prompt")),
        };
        let title = args.get("title").and_then(|v| v.as_str()).map(|s| s.to_string());
        let default = args
            .get("default")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let allow_cancel = args
            .get("allow_cancel")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let min_length = args.get("min_length").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

        match kind.as_str() {
            "choice_or_text" => {
                let options = match args.get("options").and_then(|v| v.as_array()) {
                    Some(opts) if !opts.is_empty() => opts,
                    _ => return ToolResult::error(aish_i18n::t("tools.ask_user.options_not_empty")),
                };
                let parsed_options: Vec<AskUserOption> = options
                    .iter()
                    .filter_map(|item| {
                        let value = item.get("value").and_then(|v| v.as_str())?;
                        let label = item.get("label").and_then(|v| v.as_str()).unwrap_or(value);
                        let description = item.get("description").and_then(|v| v.as_str()).map(|s| s.to_string());
                        Some(AskUserOption {
                            value: value.to_string(),
                            label: label.to_string(),
                            description,
                        })
                    })
                    .collect();

                let request = AskUserRequest {
                    kind,
                    prompt,
                    options: Some(parsed_options),
                    title,
                    default,
                    allow_cancel,
                    min_length,
                };
                self.send_and_wait(request)
            }
            "text_input" => {
                let request = AskUserRequest {
                    kind,
                    prompt,
                    options: None,
                    title,
                    default,
                    allow_cancel,
                    min_length,
                };
                self.send_and_wait(request)
            }
            _ => {
                let mut args_map = std::collections::HashMap::new();
                args_map.insert("kind".to_string(), kind);
                ToolResult::error(aish_i18n::t_with_args(
                    "tools.ask_user.unknown_kind",
                    &args_map,
                ))
            }
        }
    }
}
