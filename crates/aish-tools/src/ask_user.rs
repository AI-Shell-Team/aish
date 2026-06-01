use std::io;
use std::sync::{Arc, OnceLock};

use aish_i18n::{t, t_with_args};
use aish_llm::{Tool, ToolResult};
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct AskUserOption {
    pub value: String,
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub recommended: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskUserRequest {
    pub prompt: String,
    pub options: Vec<AskUserOption>,
    pub title: Option<String>,
    pub default: Option<String>,
    pub placeholder: Option<String>,
    pub allow_freeform_input: bool,
    pub required: bool,
    pub allow_cancel: bool,
    pub min_length: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AskUserResponse {
    Selected {
        value: String,
        label: String,
        description: Option<String>,
    },
    Text(String),
    Cancelled,
}

pub type AskUserRuntime = Arc<dyn Fn(&AskUserRequest) -> io::Result<AskUserResponse> + Send + Sync>;

#[derive(Debug, Deserialize)]
struct RawAskUserRequest {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    prompt: Option<String>,
    #[serde(default)]
    options: Vec<AskUserOption>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    default: Option<String>,
    #[serde(default)]
    placeholder: Option<String>,
    #[serde(default)]
    allow_freeform_input: Option<bool>,
    #[serde(default = "default_required")]
    required: bool,
    #[serde(default = "default_allow_cancel")]
    allow_cancel: bool,
    #[serde(default)]
    min_length: usize,
}

fn default_required() -> bool {
    true
}

fn default_allow_cancel() -> bool {
    true
}

fn default_allow_freeform_input() -> bool {
    true
}

static DESCRIPTION: OnceLock<String> = OnceLock::new();

pub(crate) fn ask_user_description() -> &'static str {
    DESCRIPTION.get_or_init(|| t("tools.ask_user.description"))
}

pub(crate) fn ask_user_parameters() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "prompt": {
                "type": "string",
                "description": t("tools.ask_user.param.prompt")
            },
            "options": {
                "type": "array",
                "description": t("tools.ask_user.param.options"),
                "items": {
                    "type": "object",
                    "properties": {
                        "value": {
                            "type": "string",
                            "description": t("tools.ask_user.param.option_value")
                        },
                        "label": {
                            "type": "string",
                            "description": t("tools.ask_user.param.option_label")
                        },
                        "description": {
                            "type": "string",
                            "description": t("tools.ask_user.param.option_description")
                        },
                        "recommended": {
                            "type": "boolean",
                            "description": t("tools.ask_user.param.option_recommended")
                        }
                    },
                    "required": ["value", "label"]
                }
            },
            "title": {
                "type": "string",
                "description": t("tools.ask_user.param.title")
            },
            "default": {
                "type": "string",
                "description": t("tools.ask_user.param.default")
            },
            "placeholder": {
                "type": "string",
                "description": t("tools.ask_user.param.placeholder")
            },
            "allow_freeform_input": {
                "type": "boolean",
                "description": t("tools.ask_user.param.allow_freeform_input"),
                "default": true
            },
            "required": {
                "type": "boolean",
                "description": t("tools.ask_user.param.required"),
                "default": true
            },
            "allow_cancel": {
                "type": "boolean",
                "description": t("tools.ask_user.param.allow_cancel"),
                "default": true
            },
            "min_length": {
                "type": "integer",
                "minimum": 0,
                "description": t("tools.ask_user.param.min_length"),
                "default": 0
            }
        },
        "required": ["prompt"]
    })
}

pub struct AskUserTool {
    runtime: Option<AskUserRuntime>,
}

impl Default for AskUserTool {
    fn default() -> Self {
        Self::new()
    }
}

impl AskUserTool {
    pub fn new() -> Self {
        Self { runtime: None }
    }

    pub fn with_runtime(runtime: AskUserRuntime) -> Self {
        Self {
            runtime: Some(runtime),
        }
    }

    pub fn set_runtime(&mut self, runtime: AskUserRuntime) {
        self.runtime = Some(runtime);
    }
}

impl Tool for AskUserTool {
    fn name(&self) -> &str {
        "ask_user"
    }

    fn description(&self) -> &str {
        ask_user_description()
    }

    fn parameters(&self) -> serde_json::Value {
        ask_user_parameters()
    }

    fn execute(&self, args: serde_json::Value) -> ToolResult {
        let request = match parse_args(args) {
            Ok(request) => request,
            Err(message) => return ToolResult::error(message),
        };

        let Some(runtime) = self.runtime.as_ref() else {
            return ToolResult::error(t("tools.ask_user.runtime_not_configured"));
        };

        match runtime(&request) {
            Ok(response) => answer_to_result(response),
            Err(err) => ToolResult::error(t_with_args(
                "tools.ask_user.execute_failed",
                &std::collections::HashMap::from([("error".to_string(), err.to_string())]),
            )),
        }
    }
}

pub(crate) fn parse_args(value: serde_json::Value) -> Result<AskUserRequest, String> {
    let raw: RawAskUserRequest = serde_json::from_value(value).map_err(|err| err.to_string())?;
    let request = normalize_args(raw)?;

    if request.prompt.trim().is_empty() {
        return Err(t("tools.ask_user.validation.prompt_empty"));
    }

    let mut option_values = std::collections::HashSet::new();
    let mut option_labels = std::collections::HashSet::new();
    let mut recommended_count = 0usize;

    for option in &request.options {
        if option.value.trim().is_empty() {
            return Err(t("tools.ask_user.validation.option_value_empty"));
        }
        if option.label.trim().is_empty() {
            return Err(t("tools.ask_user.validation.option_label_empty"));
        }
        if !option_values.insert(option.value.trim().to_string()) {
            return Err(t("tools.ask_user.validation.option_values_unique"));
        }
        if !option_labels.insert(option.label.trim().to_string()) {
            return Err(t("tools.ask_user.validation.option_labels_unique"));
        }
        if option.recommended {
            recommended_count += 1;
        }
        if recommended_count > 1 {
            return Err(t("tools.ask_user.validation.recommended_unique"));
        }
    }

    if !request.options.is_empty() {
        if let Some(default) = &request.default {
            let default = default.trim();
            if !request
                .options
                .iter()
                .any(|option| option.value.trim() == default)
            {
                return Err(t("tools.ask_user.validation.default_must_match_option"));
            }
        }
    }

    Ok(request)
}

fn normalize_args(raw: RawAskUserRequest) -> Result<AskUserRequest, String> {
    if let Some(kind) = raw.kind.as_deref() {
        match kind {
            "text_input" => {
                if !raw.options.is_empty() {
                    tracing::warn!(
                        "ask_user received legacy kind=text_input with options; inferring choice mode from options"
                    );
                }
            }
            "choice_or_text" => {
                if raw.options.is_empty() {
                    return Err(t(
                        "tools.ask_user.validation.legacy_choice_requires_options",
                    ));
                }
                if matches!(raw.allow_freeform_input, Some(false)) {
                    tracing::warn!(
                        "ask_user received legacy kind=choice_or_text with allow_freeform_input=false; using explicit field value"
                    );
                }
            }
            _ => {
                return Err(t_with_args(
                    "tools.ask_user.unknown_kind",
                    &std::collections::HashMap::from([("kind".to_string(), kind.to_string())]),
                ));
            }
        }
    }

    let prompt = raw
        .prompt
        .ok_or_else(|| t("tools.ask_user.validation.prompt_empty"))?;

    Ok(AskUserRequest {
        prompt,
        options: raw.options,
        title: raw.title,
        default: raw.default,
        placeholder: raw.placeholder,
        allow_freeform_input: raw
            .allow_freeform_input
            .unwrap_or_else(default_allow_freeform_input),
        required: raw.required,
        allow_cancel: raw.allow_cancel,
        min_length: raw.min_length,
    })
}

pub(crate) fn answer_to_result(answer: AskUserResponse) -> ToolResult {
    match answer {
        AskUserResponse::Selected {
            value,
            label,
            description,
        } => ToolResult {
            ok: true,
            output: t_with_args(
                "tools.ask_user.result.selected",
                &std::collections::HashMap::from([
                    ("label".to_string(), label.clone()),
                    ("value".to_string(), value.clone()),
                ]),
            ),
            meta: Some(serde_json::json!({
                "tool": "ask_user",
                "status": "answered",
                "answer_type": "option",
                "value": value,
                "label": label,
                "description": description,
            })),
        },
        AskUserResponse::Text(value) => ToolResult {
            ok: true,
            output: if value.is_empty() {
                t("tools.ask_user.result.empty_answer")
            } else {
                t_with_args(
                    "tools.ask_user.result.text",
                    &std::collections::HashMap::from([("value".to_string(), value.clone())]),
                )
            },
            meta: Some(serde_json::json!({
                "tool": "ask_user",
                "status": "answered",
                "answer_type": "text",
                "value": value,
            })),
        },
        AskUserResponse::Cancelled => ToolResult {
            ok: true,
            output: t("tools.ask_user.result.cancelled"),
            meta: Some(serde_json::json!({
                "tool": "ask_user",
                "status": "cancelled",
            })),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameters_only_require_prompt() {
        let required = ask_user_parameters()["required"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(
            required,
            vec![serde_json::Value::String("prompt".to_string())]
        );
    }

    #[test]
    fn parse_text_input_defaults() {
        let request = parse_args(serde_json::json!({
            "prompt": "Where should I write it?"
        }))
        .unwrap();

        assert!(request.options.is_empty());
        assert!(request.allow_freeform_input);
        assert!(request.required);
        assert!(request.allow_cancel);
        assert_eq!(request.min_length, 0);
    }

    #[test]
    fn options_infer_choice_mode_without_kind() {
        let request = parse_args(serde_json::json!({
            "prompt": "Pick one",
            "options": [{"value": "a", "label": "A"}]
        }))
        .unwrap();

        assert_eq!(request.options.len(), 1);
    }

    #[test]
    fn legacy_choice_or_text_requires_options() {
        let err = parse_args(serde_json::json!({
            "kind": "choice_or_text",
            "prompt": "Pick one"
        }))
        .unwrap_err();

        assert_eq!(
            err,
            t("tools.ask_user.validation.legacy_choice_requires_options")
        );
    }

    #[test]
    fn option_values_must_be_unique() {
        let err = parse_args(serde_json::json!({
            "prompt": "Pick one",
            "options": [
                {"value": "a", "label": "A"},
                {"value": "a", "label": "B"}
            ]
        }))
        .unwrap_err();

        assert_eq!(err, t("tools.ask_user.validation.option_values_unique"));
    }

    #[test]
    fn option_labels_must_be_unique() {
        let err = parse_args(serde_json::json!({
            "prompt": "Pick one",
            "options": [
                {"value": "a", "label": "Same"},
                {"value": "b", "label": "Same"}
            ]
        }))
        .unwrap_err();

        assert_eq!(err, t("tools.ask_user.validation.option_labels_unique"));
    }

    #[test]
    fn only_one_recommended_option_is_allowed() {
        let err = parse_args(serde_json::json!({
            "prompt": "Pick one",
            "options": [
                {"value": "a", "label": "A", "recommended": true},
                {"value": "b", "label": "B", "recommended": true}
            ]
        }))
        .unwrap_err();

        assert_eq!(err, t("tools.ask_user.validation.recommended_unique"));
    }

    #[test]
    fn option_default_must_match_value() {
        let err = parse_args(serde_json::json!({
            "prompt": "Pick one",
            "options": [{"value": "a", "label": "A"}],
            "default": "missing"
        }))
        .unwrap_err();

        assert_eq!(
            err,
            t("tools.ask_user.validation.default_must_match_option")
        );
    }

    #[test]
    fn option_default_matches_trimmed_option_value() {
        let request = parse_args(serde_json::json!({
            "prompt": "Pick one",
            "options": [{"value": "a ", "label": "A"}],
            "default": "a"
        }))
        .unwrap();

        assert_eq!(request.default.as_deref(), Some("a"));
        assert_eq!(request.options[0].value, "a ");
    }

    #[test]
    fn selected_answer_has_structured_meta_without_kind() {
        let result = answer_to_result(AskUserResponse::Selected {
            value: "a".to_string(),
            label: "A".to_string(),
            description: Some("First".to_string()),
        });

        assert!(result.ok);
        let meta = result.meta.unwrap();
        assert_eq!(meta["tool"], "ask_user");
        assert_eq!(meta["status"], "answered");
        assert_eq!(meta["answer_type"], "option");
        assert_eq!(meta["value"], "a");
        assert!(meta.get("kind").is_none());
    }

    #[test]
    fn cancelled_answer_is_successful_tool_result() {
        let result = answer_to_result(AskUserResponse::Cancelled);

        assert!(result.ok);
        assert_eq!(result.meta.unwrap()["status"], "cancelled");
    }
}
