use aish_llm::{Tool, ToolResult};

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
            store: Box::new(|_, _, _, _| "memory not available".to_string()),
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
        "Search, store, or manage long-term memories. Use 'search' to find relevant past knowledge, \
         'store' to save important information, 'list' to see recent memories, \
         'forget' to remove outdated info."
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
            None => return ToolResult::error("Missing 'action' parameter"),
        };

        match action {
            "search" => {
                let query = match args.get("query").and_then(|v| v.as_str()) {
                    Some(q) => q,
                    None => return ToolResult::error("Missing 'query' for search"),
                };
                let results = (self.search)(query, 10);
                if results.is_empty() {
                    return ToolResult::success("No matching memories found.");
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
                    None => return ToolResult::error("Missing 'content' for store"),
                };
                let category = args
                    .get("category")
                    .and_then(|v| v.as_str())
                    .unwrap_or("other");
                let id = (self.store)(content, category, "explicit", 0.8);
                ToolResult::success(format!("Stored as memory #{}.", id))
            }
            "forget" => {
                let id = match args.get("memory_id").and_then(|v| v.as_u64()) {
                    Some(id) => id as usize,
                    None => return ToolResult::error("Missing 'memory_id' for forget"),
                };
                if (self.delete)(id) {
                    ToolResult::success(format!("Forgot memory #{}.", id))
                } else {
                    ToolResult::error(format!("Memory #{} not found.", id))
                }
            }
            "list" => {
                let results = (self.list)(10);
                if results.is_empty() {
                    return ToolResult::success("No memories yet.");
                }
                let output: Vec<String> = results
                    .iter()
                    .map(|r| format!("  #{} [{}] {}", r.id, r.category, r.content))
                    .collect();
                ToolResult::success(output.join("\n"))
            }
            _ => ToolResult::error(format!(
                "Unknown action: {}. Use search/store/forget/list.",
                action
            )),
        }
    }
}
