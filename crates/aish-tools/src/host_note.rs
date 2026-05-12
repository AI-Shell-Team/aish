//! Host note tool for SSH sessions.
//!
//! Lets the AI read, store, and forget notes about the remote host,
//! similar to the local memory tool but scoped per-host.

use aish_llm::{Tool, ToolResult};

static DESCRIPTION: std::sync::OnceLock<String> = std::sync::OnceLock::new();

fn get_description() -> &'static str {
    DESCRIPTION.get_or_init(|| aish_i18n::t("tools.host_note.description"))
}

pub type HostNoteStoreFn = Box<dyn Fn(&str) -> String + Send + Sync>;
pub type HostNoteListFn = Box<dyn Fn() -> Vec<HostNoteEntry> + Send + Sync>;
pub type HostNoteForgetFn = Box<dyn Fn(&str) -> String + Send + Sync>;

#[derive(Debug, Clone)]
pub struct HostNoteEntry {
    pub id: u64,
    pub content: String,
}

pub struct HostNoteTool {
    store: HostNoteStoreFn,
    list: HostNoteListFn,
    forget: HostNoteForgetFn,
}

impl HostNoteTool {
    pub fn new(
        store: HostNoteStoreFn,
        list: HostNoteListFn,
        forget: HostNoteForgetFn,
    ) -> Self {
        Self { store, list, forget }
    }
}

impl Tool for HostNoteTool {
    fn name(&self) -> &str {
        "host_note"
    }

    fn description(&self) -> &str {
        get_description()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["store", "list", "forget"],
                    "description": aish_i18n::t("tools.host_note.param.action")
                },
                "content": {
                    "type": "string",
                    "description": aish_i18n::t("tools.host_note.param.content")
                },
                "keyword": {
                    "type": "string",
                    "description": aish_i18n::t("tools.host_note.param.keyword")
                }
            },
            "required": ["action"]
        })
    }

    fn execute(&self, args: serde_json::Value) -> ToolResult {
        let action = match args.get("action").and_then(|v| v.as_str()) {
            Some(a) => a,
            None => return ToolResult::error(aish_i18n::t("tools.host_note.missing_action")),
        };

        match action {
            "store" => {
                let content = match args.get("content").and_then(|v| v.as_str()) {
                    Some(c) => c,
                    None => {
                        return ToolResult::error(
                            aish_i18n::t("tools.host_note.store_missing_content"),
                        )
                    }
                };
                let msg = (self.store)(content);
                ToolResult::success(msg)
            }
            "list" => {
                let notes = (self.list)();
                if notes.is_empty() {
                    return ToolResult::success(aish_i18n::t("tools.host_note.empty"));
                }
                let output: Vec<String> = notes
                    .iter()
                    .map(|n| format!("  #{} {}", n.id, n.content))
                    .collect();
                ToolResult::success(output.join("\n"))
            }
            "forget" => {
                let keyword = match args.get("keyword").and_then(|v| v.as_str()) {
                    Some(k) => k,
                    None => {
                        return ToolResult::error(
                            aish_i18n::t("tools.host_note.forget_missing_keyword"),
                        )
                    }
                };
                let msg = (self.forget)(keyword);
                ToolResult::success(msg)
            }
            _ => {
                let mut args_map = std::collections::HashMap::new();
                args_map.insert("action".to_string(), action.to_string());
                ToolResult::error(aish_i18n::t_with_args(
                    "tools.host_note.unknown_action",
                    &args_map,
                ))
            }
        }
    }
}
