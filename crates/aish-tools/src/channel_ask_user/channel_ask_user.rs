//! Channel-based ask_user tool for SSH sessions.
//!
//! Instead of using `inquire` (which requires direct terminal control), this
//! tool communicates with the forwarding loop via channels. When the LLM
//! calls `ask_user`, the tool sends the question through a channel and blocks
//! until the forwarding loop provides the user's answer.

use aish_llm::{Tool, ToolResult};
use aish_pty::{AiEvent, AskUserAnswer, AskUserOption, AskUserRequest};

use super::prompt;

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
        prompt::DESCRIPTION
    }

    fn parameters(&self) -> serde_json::Value {
        prompt::parameters()
    }

    fn prompt(&self) -> &str {
        prompt::PROMPT
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
        let title = args
            .get("title")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
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
                    _ => {
                        return ToolResult::error(aish_i18n::t("tools.ask_user.options_not_empty"))
                    }
                };
                let parsed_options: Vec<AskUserOption> = options
                    .iter()
                    .filter_map(|item| {
                        let value = item.get("value").and_then(|v| v.as_str())?;
                        let label = item.get("label").and_then(|v| v.as_str()).unwrap_or(value);
                        let description = item
                            .get("description")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
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
