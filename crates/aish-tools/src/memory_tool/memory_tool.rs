use aish_core::{MemoryCategory, MemoryScope};
use aish_i18n;
use aish_llm::{PreflightResult, PreflightSecurityContext, SecurityPanelMode, Tool, ToolResult};
use aish_memory::ttl::resolve_ttl;

use super::prompt;
/// Callback type for memory search operations.
pub type MemorySearchFn = Box<dyn Fn(&str, usize) -> Vec<MemorySearchResult> + Send + Sync>;
/// Callback type for memory store operations.
/// Returns the assigned ID as a string.
/// Arguments: content, category, source, importance, scope, ttl_seconds.
pub type MemoryStoreFn =
    Box<dyn Fn(&str, &str, &str, f32, &str, Option<u64>) -> String + Send + Sync>;
/// Callback type for memory delete operations.
pub type MemoryDeleteFn = Box<dyn Fn(usize) -> bool + Send + Sync>;
/// Callback type for memory list operations.
pub type MemoryListFn = Box<dyn Fn(usize) -> Vec<MemorySearchResult> + Send + Sync>;

/// A single memory search result enriched with provenance and expiry info.
#[derive(Debug, Clone)]
pub struct MemorySearchResult {
    pub id: usize,
    pub content: String,
    pub category: String,
    pub source: String,
    pub scope: String,
    pub created_at: Option<String>,
    pub expires_at: Option<String>,
    pub expired: bool,
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
            store: Box::new(|_, _, _, _, _, _| aish_i18n::t("tools.memory.not_available")),
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
        prompt::DESCRIPTION
    }

    fn parameters(&self) -> serde_json::Value {
        prompt::parameters()
    }

    fn prompt(&self) -> &str {
        prompt::PROMPT
    }

    fn preflight(&self, args: &serde_json::Value) -> PreflightResult {
        let action = match args.get("action").and_then(|v| v.as_str()) {
            Some(a) => a,
            None => {
                return PreflightResult::Block {
                    message: aish_i18n::t("tools.memory.missing_action"),
                    security: Some(PreflightSecurityContext::fallback(
                        "memory",
                        None,
                        aish_i18n::t("tools.memory.missing_action"),
                        SecurityPanelMode::Blocked,
                    )),
                }
            }
        };

        match action {
            "store" => {
                let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
                if content.trim().is_empty() {
                    return PreflightResult::Block {
                        message: aish_i18n::t("tools.memory.store_missing_content"),
                        security: Some(PreflightSecurityContext::fallback(
                            "memory",
                            None,
                            aish_i18n::t("tools.memory.store_missing_content"),
                            SecurityPanelMode::Blocked,
                        )),
                    };
                }
                let (category, scope, ttl) = store_policy(&args);
                let permanent = args
                    .get("permanent")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let reason = args.get("reason").and_then(|v| v.as_str()).unwrap_or("");

                let mut msg = format!(
                    "{}\n  {}\n  {}: {} | {}: {}",
                    aish_i18n::t("tools.memory.confirm_store"),
                    content,
                    aish_i18n::t("tools.memory.field_category"),
                    category,
                    aish_i18n::t("tools.memory.field_scope"),
                    effective_scope(&scope),
                );
                if permanent {
                    // Prominent marker: permanent stores skip all expiry.
                    msg.push_str(&format!(
                        " | {}: PERMANENT (no expiry)",
                        aish_i18n::t("tools.memory.field_ttl")
                    ));
                } else if let Some(t) = ttl {
                    msg.push_str(&format!(
                        " | {}: {}s",
                        aish_i18n::t("tools.memory.field_ttl"),
                        t
                    ));
                }
                if !reason.is_empty() {
                    msg.push_str(&format!(
                        "\n  {}: {}",
                        aish_i18n::t("tools.memory.field_reason"),
                        reason
                    ));
                }

                PreflightResult::Confirm {
                    message: msg.clone(),
                    security: Some(PreflightSecurityContext::fallback(
                        "memory",
                        Some(content.to_string()),
                        msg,
                        SecurityPanelMode::Confirm,
                    )),
                }
            }
            "forget" => {
                let id = match args.get("memory_id").and_then(|v| v.as_u64()) {
                    Some(id) => id,
                    None => {
                        return PreflightResult::Block {
                            message: aish_i18n::t("tools.memory.forget_missing_id"),
                            security: Some(PreflightSecurityContext::fallback(
                                "memory",
                                None,
                                aish_i18n::t("tools.memory.forget_missing_id"),
                                SecurityPanelMode::Blocked,
                            )),
                        }
                    }
                };
                let reason = args.get("reason").and_then(|v| v.as_str()).unwrap_or("");
                let mut msg = format!("{} #{}", aish_i18n::t("tools.memory.confirm_forget"), id,);
                if !reason.is_empty() {
                    msg.push_str(&format!(
                        "\n  {}: {}",
                        aish_i18n::t("tools.memory.field_reason"),
                        reason
                    ));
                }

                PreflightResult::Confirm {
                    message: msg.clone(),
                    security: Some(PreflightSecurityContext::fallback(
                        "memory",
                        Some(id.to_string()),
                        msg,
                        SecurityPanelMode::Confirm,
                    )),
                }
            }
            // search and list are read-only — no confirmation needed
            _ => PreflightResult::Allow,
        }
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
                let output: Vec<String> = results.iter().map(format_search_result).collect();
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
                let (category, scope, ttl_seconds) = store_policy(&args);
                let id = (self.store)(content, &category, "explicit", 0.8, &scope, ttl_seconds);
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
                let output: Vec<String> = results.iter().map(format_list_result).collect();
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

/// Format a search result with source and expiry info.
fn format_search_result(r: &MemorySearchResult) -> String {
    let mut line = format!("  [{}] {} (id={})", r.category, r.content, r.id);
    if r.expired {
        line.push_str(&format!(" [{}]", aish_i18n::t("tools.memory.expired_tag")));
    }
    if let Some(ref exp) = r.expires_at {
        if !r.expired {
            line.push_str(&format!(
                " ({}: {})",
                aish_i18n::t("tools.memory.field_expires"),
                exp
            ));
        }
    }
    line
}

/// Format a list result with source, scope, and age info.
fn format_list_result(r: &MemorySearchResult) -> String {
    let mut line = format!("  #{} [{}] {} [{}]", r.id, r.category, r.content, r.scope);
    if !r.source.is_empty() {
        line.push_str(&format!(" ({})", r.source));
    }
    if let Some(ref created) = r.created_at {
        line.push_str(&format!(" @{}", created));
    }
    if r.expired {
        line.push_str(&format!(" [{}]", aish_i18n::t("tools.memory.expired_tag")));
    } else if let Some(ref exp) = r.expires_at {
        line.push_str(&format!(
            " ({}: {})",
            aish_i18n::t("tools.memory.field_expires"),
            exp
        ));
    }
    line
}

/// Extract the store-policy tuple (category, scope, effective TTL) from a
/// store action's args. Shared by preflight (confirmation display) and
/// execute (actual write) so the user always confirms the TTL that will be
/// persisted.
fn store_policy(args: &serde_json::Value) -> (String, String, Option<u64>) {
    let category = args
        .get("category")
        .and_then(|v| v.as_str())
        .unwrap_or("other")
        .to_string();
    let scope = args
        .get("scope")
        .and_then(|v| v.as_str())
        .unwrap_or("user")
        .to_string();
    let proposed = args.get("ttl_seconds").and_then(|v| v.as_u64());
    // An explicit user-requested permanent store bypasses TTL defaults and
    // caps. The preflight panel highlights it so the user stays in control.
    if args
        .get("permanent")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return (category, scope, None);
    }
    // Resolve against the *parsed* category/scope so TTL matches what the
    // shell callback will actually persist.
    let ttl = resolve_ttl(proposed, &parse_category(&category), &parse_scope(&scope));
    (category, scope, ttl)
}

/// Short lowercase scope name matching what the shell callback persists
/// after its own parse (invalid values fall back to "user").
fn effective_scope(scope: &str) -> &str {
    match parse_scope(scope) {
        MemoryScope::User => "user",
        MemoryScope::Host => "host",
        MemoryScope::Project => "project",
    }
}

/// Parse a category string (from the LLM tool call) into a MemoryCategory.
/// Falls back to `Other` for unrecognized values, matching the shell-side
/// `parse_category_str`.
fn parse_category(s: &str) -> MemoryCategory {
    match s.to_lowercase().as_str() {
        "preference" => MemoryCategory::Preference,
        "environment" => MemoryCategory::Environment,
        "solution" => MemoryCategory::Solution,
        "pattern" => MemoryCategory::Pattern,
        _ => MemoryCategory::Other,
    }
}

/// Parse a scope string (from the LLM tool call) into a MemoryScope.
/// Falls back to `User` for unrecognized values — never promote host facts
/// to global scope by default.
fn parse_scope(s: &str) -> MemoryScope {
    match s.to_lowercase().as_str() {
        "host" => MemoryScope::Host,
        "project" => MemoryScope::Project,
        _ => MemoryScope::User,
    }
}

#[cfg(test)]
mod ttl_policy_tests {
    use super::*;

    /// Capture what the store callback receives for (scope, ttl).
    fn tool_with_capture(
        captured: std::sync::Arc<std::sync::Mutex<Vec<(String, Option<u64>)>>>,
    ) -> MemoryTool {
        MemoryTool::new(
            Box::new(|_, _| Vec::new()),
            Box::new(move |_c, _cat, _src, _imp, scope, ttl| {
                captured.lock().unwrap().push((scope.to_string(), ttl));
                "1".to_string()
            }),
            Box::new(|_| false),
            Box::new(|_| Vec::new()),
        )
    }

    #[test]
    fn store_missing_ttl_uses_category_default() {
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let tool = tool_with_capture(captured.clone());
        let res = tool.execute(serde_json::json!({
            "action": "store",
            "content": "prod db endpoint",
            "category": "environment",
        }));
        assert!(res.ok);
        let got = captured.lock().unwrap();
        // Environment without a model proposal gets the 7-day default,
        // not permanent.
        assert_eq!(got[0].1, Some(7 * 24 * 3600));
    }

    #[test]
    fn store_overlong_environment_proposal_clamped() {
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let tool = tool_with_capture(captured.clone());
        let res = tool.execute(serde_json::json!({
            "action": "store",
            "content": "test host ip",
            "category": "environment",
            "ttl_seconds": 10 * 365 * 24 * 3600,
        }));
        assert!(res.ok);
        let got = captured.lock().unwrap();
        assert_eq!(got[0].1, Some(aish_memory::ttl::ENVIRONMENT_CAP_SECS));
    }

    #[test]
    fn store_host_scope_silent_model_never_permanent() {
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let tool = tool_with_capture(captured.clone());
        let res = tool.execute(serde_json::json!({
            "action": "store",
            "content": "machine fact",
            "category": "preference",
            "scope": "host",
        }));
        assert!(res.ok);
        let got = captured.lock().unwrap();
        // Host facts are capped regardless of category.
        assert_eq!(got[0].0, "host");
        assert_eq!(got[0].1, Some(aish_memory::ttl::HOST_SCOPE_CAP_SECS));
    }

    #[test]
    fn store_explicit_permanent_bypasses_environment_default() {
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let tool = tool_with_capture(captured.clone());
        let res = tool.execute(serde_json::json!({
            "action": "store",
            "content": "prod master db host",
            "category": "environment",
            "permanent": true,
        }));
        assert!(res.ok);
        let got = captured.lock().unwrap();
        // User explicitly requested permanent: no TTL despite the volatile
        // category's 7-day default.
        assert_eq!(got[0].1, None);
    }

    #[test]
    fn store_explicit_permanent_bypasses_host_cap() {
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let tool = tool_with_capture(captured.clone());
        let res = tool.execute(serde_json::json!({
            "action": "store",
            "content": "machine fact",
            "category": "environment",
            "scope": "host",
            "permanent": true,
        }));
        assert!(res.ok);
        let got = captured.lock().unwrap();
        assert_eq!(got[0].1, None);
    }

    #[test]
    fn store_permanent_false_falls_back_to_policy() {
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let tool = tool_with_capture(captured.clone());
        let res = tool.execute(serde_json::json!({
            "action": "store",
            "content": "staging endpoint",
            "category": "environment",
            "permanent": false,
        }));
        assert!(res.ok);
        let got = captured.lock().unwrap();
        assert_eq!(got[0].1, Some(7 * 24 * 3600));
    }

    #[test]
    fn store_stable_preference_without_ttl_stays_permanent() {
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let tool = tool_with_capture(captured.clone());
        let res = tool.execute(serde_json::json!({
            "action": "store",
            "content": "prefers vim",
            "category": "preference",
        }));
        assert!(res.ok);
        let got = captured.lock().unwrap();
        assert_eq!(got[0].1, None);
    }
}
