//! Ask-user tool using the `inquire` crate for interactive prompts.
//!
//! choice_or_text: `inquire::Select` with a "(custom input)" entry at the
//! bottom. Selecting it opens `inquire::Text`; pressing Esc there returns
//! to the Select list (loop), not cancels the whole dialog.
//!
//! text_input: `inquire::Text` with optional default.

use std::io::{self, Write};

use aish_llm::{Tool, ToolResult};

const CUSTOM_INPUT_LABEL: &str = "(type custom answer)";

pub struct AskUserTool;

impl Default for AskUserTool {
    fn default() -> Self {
        Self::new()
    }
}

impl AskUserTool {
    pub fn new() -> Self {
        Self
    }
}

impl Tool for AskUserTool {
    fn name(&self) -> &str {
        "ask_user"
    }

    fn description(&self) -> &str {
        "Ask the user a question to clarify their intent. Use 'text_input' for free-form questions \
         or 'choice_or_text' for multiple-choice with custom input option."
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
                "placeholder": {
                    "type": "string",
                    "description": "Placeholder text"
                },
                "required": {
                    "type": "boolean",
                    "description": "Whether the user must provide an answer (default: true)",
                    "default": true
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
            .unwrap_or("text_input");
        let prompt = match args.get("prompt").and_then(|v| v.as_str()) {
            Some(p) => p,
            None => return ToolResult::error("Missing 'prompt' parameter"),
        };
        let title = args.get("title").and_then(|v| v.as_str());
        let default = args.get("default").and_then(|v| v.as_str());
        let allow_cancel = args
            .get("allow_cancel")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let min_length = args.get("min_length").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

        match kind {
            "choice_or_text" => {
                self.handle_choice_or_text(title, prompt, &args, default, allow_cancel)
            }
            "text_input" => {
                self.handle_text_input(title, prompt, default, allow_cancel, min_length)
            }
            _ => ToolResult::error(format!("Unknown kind: {}", kind)),
        }
    }
}

// ---------- Slot for identifying which item was selected ----------

#[derive(Clone)]
enum Slot {
    Opt(usize),
    Custom,
}

impl AskUserTool {
    fn handle_choice_or_text(
        &self,
        title: Option<&str>,
        prompt: &str,
        args: &serde_json::Value,
        default: Option<&str>,
        allow_cancel: bool,
    ) -> ToolResult {
        let options = match args.get("options").and_then(|v| v.as_array()) {
            Some(opts) if !opts.is_empty() => opts,
            _ => return ToolResult::error("options must be a non-empty list for choice_or_text"),
        };

        let display_prompt = match title {
            Some(t) => format!("{}: {}", t, prompt),
            None => prompt.to_string(),
        };

        // Build items: real options + custom-input slot at the bottom.
        let mut items: Vec<(String, Slot)> = options
            .iter()
            .enumerate()
            .map(|(i, opt)| {
                let label = opt.get("label").and_then(|v| v.as_str()).unwrap_or("?");
                let desc = opt.get("description").and_then(|v| v.as_str());
                let display = match desc {
                    Some(d) => format!("{} - {}", label, d),
                    None => label.to_string(),
                };
                (display, Slot::Opt(i))
            })
            .collect();
        items.push((CUSTOM_INPUT_LABEL.to_string(), Slot::Custom));

        let labels: Vec<String> = items.iter().map(|(l, _)| l.clone()).collect();

        // Default cursor position.
        let starting_cursor = if let Some(dv) = default {
            items
                .iter()
                .position(|(_, s)| {
                    matches!(s, Slot::Opt(i)
                        if options[*i].get("value").and_then(|v| v.as_str()) == Some(dv))
                })
                .unwrap_or(0)
        } else {
            0
        };

        let help_msg = if allow_cancel {
            "Esc to cancel, Enter to select"
        } else {
            "Enter to select"
        };

        // Loop: Select → (custom → Text → Esc back to Select) → done
        loop {
            let select_result = inquire::Select::new(&display_prompt, labels.clone())
                .with_starting_cursor(starting_cursor)
                .with_help_message(help_msg)
                .prompt();

            match select_result {
                Ok(chosen_label) => {
                    let idx = items
                        .iter()
                        .position(|(l, _)| l == &chosen_label)
                        .unwrap_or(0);

                    match &items[idx].1 {
                        Slot::Opt(i) => {
                            let value = options[*i]
                                .get("value")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            return ToolResult::success(value.to_string());
                        }
                        Slot::Custom => {
                            // Open text input; Esc returns to Select.
                            let text_result = inquire::Text::new("Enter your answer:")
                                .with_help_message(if allow_cancel {
                                    "Esc to go back"
                                } else {
                                    "Esc to go back to options"
                                })
                                .prompt();

                            match text_result {
                                Ok(text) => {
                                    let trimmed = text.trim().to_string();
                                    if trimmed.is_empty() {
                                        // Empty input — go back to select.
                                        continue;
                                    }
                                    return ToolResult::success(format!("User input: {}", trimmed));
                                }
                                Err(_) => {
                                    // Esc pressed — go back to Select.
                                    continue;
                                }
                            }
                        }
                    }
                }
                Err(_) => {
                    // Esc pressed at Select level.
                    if allow_cancel {
                        if let Some(d) = default {
                            return ToolResult::success(d.to_string());
                        }
                        return ToolResult::success("(cancelled)".to_string());
                    }
                    // Not allowed to cancel — loop back.
                    continue;
                }
            }
        }
    }

    fn handle_text_input(
        &self,
        title: Option<&str>,
        prompt: &str,
        default: Option<&str>,
        allow_cancel: bool,
        min_length: usize,
    ) -> ToolResult {
        let display_prompt = match title {
            Some(t) => format!("{}: {}", t, prompt),
            None => prompt.to_string(),
        };

        let help_msg = if allow_cancel { "Esc to cancel" } else { "" };

        let mut text = inquire::Text::new(&display_prompt).with_help_message(help_msg);
        if let Some(d) = default {
            text = text.with_default(d);
        }

        match text.prompt() {
            Ok(answer) => {
                let trimmed = answer.trim().to_string();
                if trimmed.is_empty() {
                    if let Some(d) = default {
                        return ToolResult::success(d.to_string());
                    }
                    if allow_cancel {
                        return ToolResult::success("(cancelled)".to_string());
                    }
                    return ToolResult::error("Answer is required");
                }
                if trimmed.len() < min_length {
                    return ToolResult::error(format!(
                        "Answer too short (min {} characters)",
                        min_length
                    ));
                }
                ToolResult::success(format!("User input: {}", trimmed))
            }
            Err(_) => self.fallback_text_input(title, prompt, default, allow_cancel, min_length),
        }
    }

    // ---------- stdin fallback (non-interactive / pipe) ----------

    fn fallback_text_input(
        &self,
        title: Option<&str>,
        prompt: &str,
        default: Option<&str>,
        allow_cancel: bool,
        min_length: usize,
    ) -> ToolResult {
        if let Some(t) = title {
            println!("\x1b[1m{}\x1b[0m", t);
        }
        println!("\x1b[36m{}\x1b[0m", prompt);
        if allow_cancel {
            println!("  \x1b[2m(press Enter with empty input to cancel)\x1b[0m");
        }
        if let Some(d) = default {
            print!("\x1b[2m[default: {}]\x1b[0m Your answer: ", d);
        } else {
            print!("Your answer: ");
        }
        let _ = io::stdout().flush();

        let mut answer = String::new();
        if io::stdin().read_line(&mut answer).is_err() {
            return ToolResult::error("Failed to read user input");
        }
        let answer = answer.trim().to_string();

        if answer.is_empty() {
            if let Some(d) = default {
                return ToolResult::success(d.to_string());
            }
            if allow_cancel {
                return ToolResult::success("(cancelled)".to_string());
            }
            return ToolResult::error("Answer is required");
        }

        if answer.len() < min_length {
            return ToolResult::error(format!("Answer too short (min {} characters)", min_length));
        }

        ToolResult::success(format!("User input: {}", answer))
    }
}
