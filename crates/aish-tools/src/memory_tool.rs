use aish_i18n;
use aish_llm::{Tool, ToolResult};

/// Cached translated description.
static DESCRIPTION: std::sync::OnceLock<String> = std::sync::OnceLock::new();

fn get_description() -> &'static str {
    DESCRIPTION.get_or_init(|| aish_i18n::t("tools.memory.description"))
}

/// Callback type for memory operations.
pub type MemorySearchFn = Box<dyn Fn(&str, usize) -> Vec<MemorySearchResult> + Send + Sync>;
pub type MemoryStoreFn = Box<dyn Fn(&str, &str, &str, f32) -> String + Send + Sync>;
pub type MemoryDeleteFn = Box<dyn Fn(usize) -> bool + Send + Sync>;
pub type MemoryListFn = Box<dyn Fn(usize) -> Vec<MemorySearchResult> + Send + Sync>;

/// A single memory search result.
#[derive(Debug, Clone)]
pub struct MemorySearchResult {
    pub id: usize,
    pub content: String,
    pub category: String,
}

/// Tool for searching, storing, and managing long-term memories.
pub struct MemoryTool {
    search: MemorySearchFn,
    store: MemoryStoreFn,
    delete: MemoryDeleteFn,
    list: MemoryListFn,
}

impl MemoryTool {
    pub fn new(
        search: MemorySearchFn,
        store: MemoryStoreFn,
        delete: MemoryDeleteFn,
        list: MemoryListFn,
    ) -> Self {
        Self {
            search,
            store,
            delete,
            list,
        }
    }

    /// Create a no-op memory tool that always returns empty results.
    pub fn noop() -> Self {
        Self {
            search: Box::new(|_, _| Vec::new()),
            store: Box::new(|_, _, _, _| aish_i18n::t("tools.memory.not_available")),
            delete: Box::new(|_| false),
            list: Box::new(|_| Vec::new()),
        }
    }
}

impl Tool for MemoryTool {
    fn name(&self) -> &str {
        "memory"
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
                    "enum": ["search", "store", "forget", "list"],
                    "description": "Memory operation to perform"
                },
                "query": {
                    "type": "string",
                    "description": "Search query (for 'search' action)"
                },
                "content": {
                    "type": "string",
                    "description": "Content to store (for 'store' action)"
                },
                "category": {
                    "type": "string",
                    "enum": ["preference", "environment", "solution", "pattern", "other"],
                    "description": "Category for stored memory (default: other)"
                },
                "memory_id": {
                    "type": "integer",
                    "description": "Memory ID to forget (for 'forget' action)"
                }
            },
            "required": ["action"]
        })
    }

    fn execute(&self, args: serde_json::Value) -> ToolResult {
        let action = match args.get("action").and_then(|v| v.as_str()) {
            Some(a) => a,
            None => return ToolResult::error(aish_i18n::t("tools.memory.missing_action")),
        };

        match action {
            "search" => {
                let query = match args.get("query").and_then(|v| v.as_str()) {
                    Some(q) => q,
                    None => {
                        return ToolResult::error(aish_i18n::t("tools.memory.search_missing_query"))
                    }
                };
                let results = (self.search)(query, 10);
                if results.is_empty() {
                    return ToolResult::success(aish_i18n::t("tools.memory.no_results"));
                }
                let output: Vec<String> = results
                    .iter()
                    .map(|r| format!("  [{}] {} (id={})", r.category, r.content, r.id))
                    .collect();
                ToolResult::success(output.join("\n"))
            }
            "store" => {
                let content = match args.get("content").and_then(|v| v.as_str()) {
                    Some(c) => c,
                    None => {
                        return ToolResult::error(aish_i18n::t(
                            "tools.memory.store_missing_content",
                        ))
                    }
                };
                let category = args
                    .get("category")
                    .and_then(|v| v.as_str())
                    .unwrap_or("other");
                let id = (self.store)(content, category, "explicit", 0.8);
                let mut args_map = std::collections::HashMap::new();
                args_map.insert("id".to_string(), id.clone());
                ToolResult::success(aish_i18n::t_with_args("tools.memory.stored", &args_map))
            }
            "forget" => {
                let id = match args.get("memory_id").and_then(|v| v.as_u64()) {
                    Some(id) => id as usize,
                    None => {
                        return ToolResult::error(aish_i18n::t("tools.memory.forget_missing_id"))
                    }
                };
                if (self.delete)(id) {
                    let mut args_map = std::collections::HashMap::new();
                    args_map.insert("id".to_string(), id.to_string());
                    ToolResult::success(aish_i18n::t_with_args("tools.memory.forgot", &args_map))
                } else {
                    let mut args_map = std::collections::HashMap::new();
                    args_map.insert("id".to_string(), id.to_string());
                    ToolResult::error(aish_i18n::t_with_args("tools.memory.not_found", &args_map))
                }
            }
            "list" => {
                let results = (self.list)(10);
                if results.is_empty() {
                    return ToolResult::success(aish_i18n::t("tools.memory.empty"));
                }
                let output: Vec<String> = results
                    .iter()
                    .map(|r| format!("  #{} [{}] {}", r.id, r.category, r.content))
                    .collect();
                ToolResult::success(output.join("\n"))
            }
            _ => {
                let mut args_map = std::collections::HashMap::new();
                args_map.insert("action".to_string(), action.to_string());
                ToolResult::error(aish_i18n::t_with_args(
                    "tools.memory.unknown_action",
                    &args_map,
                ))
            }
        }
    }
}
