//! Ask-user tool for local interactive prompts.
//!
//! choice_or_text: shared selection panel with inline custom input.
//!
//! text_input: `inquire::Text` with optional default.

use std::io::{self, Write};

use aish_i18n;
use aish_llm::{Tool, ToolResult};
use aish_ui::{ChoiceOutcome, ChoicePanel, PanelOutcome, PanelRuntime, SearchSelectItem};

/// Cached translated description.
static DESCRIPTION: std::sync::OnceLock<String> = std::sync::OnceLock::new();

fn get_description() -> &'static str {
    DESCRIPTION.get_or_init(|| aish_i18n::t("tools.ask_user.description"))
}

fn get_custom_input_label() -> String {
    aish_i18n::t("tools.ask_user.custom_input_label")
}

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
            None => return ToolResult::error(aish_i18n::t("tools.ask_user.missing_prompt")),
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
            _ => {
                let mut args_map = std::collections::HashMap::new();
                args_map.insert("kind".to_string(), kind.to_string());
                ToolResult::error(aish_i18n::t_with_args(
                    "tools.ask_user.unknown_kind",
                    &args_map,
                ))
            }
        }
    }
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
            _ => return ToolResult::error(aish_i18n::t("tools.ask_user.options_not_empty")),
        };

        // Loop only handles required prompts that cannot be cancelled.
        loop {
            match self.run_choice_panel(title, prompt, options, default, allow_cancel) {
                Ok(PanelOutcome::Submitted(ChoiceOutcome::Selected(value))) => {
                    return ToolResult::success(value);
                }
                Ok(PanelOutcome::Submitted(ChoiceOutcome::CustomInput(text))) => {
                    return ToolResult::success(format_custom_user_input(text));
                }
                Ok(PanelOutcome::Cancelled) => {
                    // Esc pressed at Select level.
                    if allow_cancel {
                        if let Some(d) = default {
                            return ToolResult::success(d.to_string());
                        }
                        return ToolResult::success(aish_i18n::t("tools.ask_user.cancelled"));
                    }
                    // Not allowed to cancel — loop back.
                    continue;
                }
                Err(_) => {
                    return self.fallback_choice_or_text(
                        title,
                        prompt,
                        options,
                        default,
                        allow_cancel,
                    )
                }
            }
        }
    }

    fn run_choice_panel(
        &self,
        title: Option<&str>,
        prompt: &str,
        options: &[serde_json::Value],
        default: Option<&str>,
        allow_cancel: bool,
    ) -> Result<PanelOutcome<ChoiceOutcome>, aish_ui::PanelError> {
        let panel_items = options
            .iter()
            .map(|option| {
                let value = option_value(option).to_string();
                let label = option_label(option).to_string();
                let mut item = SearchSelectItem::new(value.clone(), label.clone());
                if let Some(description) = option_description(option) {
                    item = item.with_detail(description.to_string());
                }
                item.with_search_text(format!(
                    "{} {} {}",
                    value,
                    label,
                    option_description(option).unwrap_or("")
                ))
            })
            .collect();

        let help_msg = if allow_cancel {
            aish_i18n::t("tools.ask_user.help_select_with_cancel")
        } else {
            aish_i18n::t("tools.ask_user.help_select_no_cancel")
        };
        let panel = ChoicePanel::new(title.unwrap_or_default(), prompt, panel_items)
            .with_custom_label(get_custom_input_label())
            .with_selected_value(default)
            .with_allow_cancel(allow_cancel)
            .with_custom_input_footer(if allow_cancel {
                aish_i18n::t("tools.ask_user.custom_input_help_cancel")
            } else {
                aish_i18n::t("tools.ask_user.custom_input_help_no_cancel")
            })
            .with_footer(help_msg);

        PanelRuntime::new().run(panel)
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

        let help_msg = if allow_cancel {
            aish_i18n::t("tools.ask_user.custom_input_help_cancel")
        } else {
            String::new()
        };

        let mut text = inquire::Text::new(&display_prompt).with_help_message(&help_msg);
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
                        return ToolResult::success(aish_i18n::t("tools.ask_user.cancelled"));
                    }
                    return ToolResult::error(aish_i18n::t("tools.ask_user.answer_required"));
                }
                if trimmed.len() < min_length {
                    let mut args_map = std::collections::HashMap::new();
                    args_map.insert("min_length".to_string(), min_length.to_string());
                    return ToolResult::error(aish_i18n::t_with_args(
                        "tools.ask_user.answer_too_short",
                        &args_map,
                    ));
                }
                let mut args_map = std::collections::HashMap::new();
                args_map.insert("input".to_string(), trimmed.clone());
                ToolResult::success(aish_i18n::t_with_args(
                    "tools.ask_user.user_input_prefix",
                    &args_map,
                ))
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
            return ToolResult::error(aish_i18n::t("tools.ask_user.read_input_failed"));
        }
        let answer = answer.trim().to_string();

        if answer.is_empty() {
            if let Some(d) = default {
                return ToolResult::success(d.to_string());
            }
            if allow_cancel {
                return ToolResult::success("(cancelled)".to_string());
            }
            return ToolResult::error(aish_i18n::t("tools.ask_user.answer_required"));
        }

        if answer.len() < min_length {
            let mut args_map = std::collections::HashMap::new();
            args_map.insert("min_length".to_string(), min_length.to_string());
            return ToolResult::error(aish_i18n::t_with_args(
                "tools.ask_user.answer_too_short",
                &args_map,
            ));
        }

        let mut args_map = std::collections::HashMap::new();
        args_map.insert("input".to_string(), answer.clone());
        ToolResult::success(aish_i18n::t_with_args(
            "tools.ask_user.user_input_prefix",
            &args_map,
        ))
    }

    fn fallback_choice_or_text(
        &self,
        title: Option<&str>,
        prompt: &str,
        options: &[serde_json::Value],
        default: Option<&str>,
        allow_cancel: bool,
    ) -> ToolResult {
        if let Some(t) = title {
            println!("\x1b[1m{}\x1b[0m", t);
        }
        println!("\x1b[36m{}\x1b[0m", prompt);
        for (index, option) in options.iter().enumerate() {
            match option_description(option) {
                Some(description) => println!(
                    "  \x1b[33m{}.\x1b[0m {} - {}",
                    index + 1,
                    option_label(option),
                    description
                ),
                None => println!("  \x1b[33m{}.\x1b[0m {}", index + 1, option_label(option)),
            }
        }
        println!(
            "  \x1b[33m0.\x1b[0m \x1b[2m{}\x1b[0m",
            get_custom_input_label()
        );
        if allow_cancel {
            println!("  \x1b[2m(press Enter with empty input to cancel)\x1b[0m");
        }
        print!("Your answer: ");
        let _ = io::stdout().flush();

        let mut answer = String::new();
        if io::stdin().read_line(&mut answer).is_err() {
            return ToolResult::error(aish_i18n::t("tools.ask_user.read_input_failed"));
        }
        let answer = answer.trim().to_string();

        if answer.is_empty() {
            if let Some(d) = default {
                return ToolResult::success(d.to_string());
            }
            if allow_cancel {
                return ToolResult::success(aish_i18n::t("tools.ask_user.cancelled"));
            }
            return ToolResult::error(aish_i18n::t("tools.ask_user.answer_required"));
        }

        if let Ok(selection) = answer.parse::<usize>() {
            if selection > 0 && selection <= options.len() {
                return ToolResult::success(option_value(&options[selection - 1]).to_string());
            }
            if selection == 0 {
                return self.fallback_custom_input(default, allow_cancel);
            }
        }

        ToolResult::success(format_custom_user_input(answer))
    }

    fn fallback_custom_input(&self, default: Option<&str>, allow_cancel: bool) -> ToolResult {
        print!("{}: ", aish_i18n::t("tools.ask_user.custom_input_prompt"));
        let _ = io::stdout().flush();
        let mut answer = String::new();
        if io::stdin().read_line(&mut answer).is_err() {
            return ToolResult::error(aish_i18n::t("tools.ask_user.read_input_failed"));
        }
        let answer = answer.trim().to_string();
        if answer.is_empty() {
            if let Some(d) = default {
                return ToolResult::success(d.to_string());
            }
            if allow_cancel {
                return ToolResult::success(aish_i18n::t("tools.ask_user.cancelled"));
            }
            return ToolResult::error(aish_i18n::t("tools.ask_user.answer_required"));
        }
        ToolResult::success(format_custom_user_input(answer))
    }
}

fn option_value(option: &serde_json::Value) -> &str {
    option.get("value").and_then(|v| v.as_str()).unwrap_or("")
}

fn option_label(option: &serde_json::Value) -> &str {
    option.get("label").and_then(|v| v.as_str()).unwrap_or("?")
}

fn option_description(option: &serde_json::Value) -> Option<&str> {
    option.get("description").and_then(|v| v.as_str())
}

fn format_custom_user_input(input: String) -> String {
    let mut args_map = std::collections::HashMap::new();
    args_map.insert("input".to_string(), input);
    aish_i18n::t_with_args("tools.ask_user.user_input_prefix", &args_map)
}
