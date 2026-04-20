use std::collections::HashMap;

use aish_core::MemoryType;
use tracing::debug;

use crate::types::ContextMessage;

/// Statistics about the current context state.
#[derive(Debug, Clone)]
pub struct ContextStats {
    pub total_messages: usize,
    pub llm_messages: usize,
    pub shell_messages: usize,
    pub knowledge_messages: usize,
    pub system_messages: usize,
    pub estimated_tokens: usize,
}

/// Manages the conversation context window with per-type message limits and
/// optional token budget.
pub struct ContextManager {
    messages: Vec<ContextMessage>,
    max_llm_messages: usize,
    max_shell_messages: usize,
    max_knowledge_messages: usize,
    token_budget: Option<usize>,
    model: String,
    knowledge_cache: HashMap<String, String>,
}

impl Default for ContextManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ContextManager {
    /// Create a new manager with default limits.
    ///
    /// Defaults: `max_llm_messages=50`, `max_shell_messages=20`,
    /// `max_knowledge_messages=10`.
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            max_llm_messages: 50,
            max_shell_messages: 20,
            max_knowledge_messages: 10,
            token_budget: None,
            model: String::new(),
            knowledge_cache: HashMap::new(),
        }
    }

    /// Create a manager with custom per-type message limits.
    pub fn with_limits(max_llm: usize, max_shell: usize, max_knowledge: usize) -> Self {
        Self {
            messages: Vec::new(),
            max_llm_messages: max_llm,
            max_shell_messages: max_shell,
            max_knowledge_messages: max_knowledge,
            token_budget: None,
            model: String::new(),
            knowledge_cache: HashMap::new(),
        }
    }

    /// Add a pre-built context message.
    pub fn add_memory(&mut self, memory_type: MemoryType, msg: ContextMessage) {
        debug!(
            role = %msg.role,
            ?memory_type,
            len = msg.content.len(),
            "adding context message"
        );
        self.messages.push(msg);
    }

    /// Convenience helper: create and append a message in one call.
    pub fn add_message(&mut self, role: &str, content: &str, memory_type: MemoryType) {
        self.add_memory(
            memory_type.clone(),
            ContextMessage {
                role: role.to_string(),
                content: content.to_string(),
                memory_type,
                name: None,
                tool_call_id: None,
            },
        );
    }

    /// Convert stored messages to the OpenAI chat message format.
    ///
    /// Only the fields relevant to the API (`role`, `content`, `name`,
    /// `tool_call_id`) are included; internal metadata like `memory_type` is
    /// stripped.
    pub fn as_messages(&self) -> Vec<serde_json::Value> {
        self.messages
            .iter()
            .map(|m| {
                let mut obj = serde_json::Map::new();
                obj.insert("role".into(), serde_json::Value::String(m.role.clone()));
                obj.insert(
                    "content".into(),
                    serde_json::Value::String(m.content.clone()),
                );
                if let Some(ref name) = m.name {
                    obj.insert("name".into(), serde_json::Value::String(name.clone()));
                }
                if let Some(ref id) = m.tool_call_id {
                    obj.insert("tool_call_id".into(), serde_json::Value::String(id.clone()));
                }
                serde_json::Value::Object(obj)
            })
            .collect()
    }

    /// Estimate the token count for a piece of text using the cl100k_base
    /// tokenizer.
    ///
    /// Falls back to a rough `len / 4` heuristic when the tokenizer is
    /// unavailable.
    pub fn estimate_tokens(&self, text: &str) -> usize {
        let bpe = tiktoken_rs::cl100k_base_singleton();
        let guard = bpe.lock();
        guard.encode_with_special_tokens(text).len()
    }

    /// Return the total number of stored messages.
    pub fn get_context_size(&self) -> usize {
        self.messages.len()
    }

    /// Get statistics about the current context state.
    pub fn get_context_stats(&self) -> ContextStats {
        let mut stats = ContextStats {
            total_messages: self.messages.len(),
            llm_messages: 0,
            shell_messages: 0,
            knowledge_messages: 0,
            system_messages: 0,
            estimated_tokens: 0,
        };

        for msg in &self.messages {
            match msg.memory_type {
                MemoryType::Llm => stats.llm_messages += 1,
                MemoryType::Shell => stats.shell_messages += 1,
                MemoryType::Knowledge => stats.knowledge_messages += 1,
            }
            if msg.role == "system" {
                stats.system_messages += 1;
            }
            stats.estimated_tokens += self.estimate_tokens(&msg.content);
        }

        stats
    }

    /// Auto-trim messages per type when the configured limit is exceeded, and
    /// additionally trim to the token budget if one is set.
    ///
    /// Trimming removes the oldest messages first. System messages are never
    /// removed.
    pub fn trim(&mut self) {
        // Per-type trimming.
        self.trim_by_type(MemoryType::Llm, self.max_llm_messages);
        self.trim_by_type(MemoryType::Shell, self.max_shell_messages);
        self.trim_by_type(MemoryType::Knowledge, self.max_knowledge_messages);

        // Token-budget trimming (if configured).
        if let Some(budget) = self.token_budget {
            self.trim_to_token_budget(budget);
        }
    }

    /// Remove all messages.
    pub fn clear(&mut self) {
        self.messages.clear();
    }

    /// Remove knowledge-type messages whose content starts with a specific tag.
    /// Used to refresh memory recall and skill injection without accumulating
    /// duplicates.
    pub fn clear_knowledge(&mut self, tag: &str) {
        let prefix = format!("<{}", tag);
        self.messages.retain(|m| {
            !(m.memory_type == MemoryType::Knowledge && m.content.starts_with(&prefix))
        });
    }

    /// Set the model name (used for future tokeniser selection).
    pub fn set_model(&mut self, model: &str) {
        self.model = model.to_string();
    }

    /// Set the token budget for context trimming.
    pub fn set_token_budget(&mut self, budget: Option<usize>) {
        self.token_budget = budget;
    }

    /// Read-only access to the knowledge cache.
    pub fn knowledge_cache(&self) -> &HashMap<String, String> {
        &self.knowledge_cache
    }

    /// Mutable access to the knowledge cache.
    pub fn knowledge_cache_mut(&mut self) -> &mut HashMap<String, String> {
        &mut self.knowledge_cache
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Remove the oldest non-system messages of a given type until we are
    /// within the specified limit.
    fn trim_by_type(&mut self, memory_type: MemoryType, limit: usize) {
        let count = self
            .messages
            .iter()
            .filter(|m| m.memory_type == memory_type && m.role != "system")
            .count();

        if count <= limit {
            return;
        }

        let to_remove = count - limit;
        let mut removed = 0;

        self.messages.retain(|m| {
            if removed >= to_remove {
                return true;
            }
            if m.memory_type == memory_type && m.role != "system" {
                removed += 1;
                debug!(role = %m.role, ?memory_type, "trimmed message");
                false
            } else {
                true
            }
        });
    }

    /// Remove the oldest non-system messages until the total token count is
    /// within the budget.
    ///
    /// Uses `retain()` for a single O(n) pass instead of O(n^2) repeated
    /// `remove()` calls.
    fn trim_to_token_budget(&mut self, budget: usize) {
        // Calculate total tokens.
        let total_tokens: usize = self
            .messages
            .iter()
            .map(|m| self.estimate_tokens(&m.content))
            .sum();

        if total_tokens <= budget {
            return;
        }

        // Walk forward to compute how many tokens we need to shed, recording
        // which messages should be removed. We stop as soon as the budget is
        // satisfied.
        let mut current = total_tokens;
        let mut should_remove = vec![false; self.messages.len()];

        for (i, m) in self.messages.iter().enumerate() {
            if current <= budget {
                break;
            }
            if m.role != "system" {
                let tokens = self.estimate_tokens(&m.content);
                current = current.saturating_sub(tokens);
                debug!(role = %m.role, tokens, "trimmed message for token budget");
                should_remove[i] = true;
            }
        }

        // Single-pass retain: O(n) instead of O(n^2).
        let mut idx = 0;
        self.messages.retain(|_| {
            let keep = !should_remove[idx];
            idx += 1;
            keep
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_has_defaults() {
        let mgr = ContextManager::new();
        assert_eq!(mgr.get_context_size(), 0);
        assert!(mgr.knowledge_cache.is_empty());
    }

    #[test]
    fn add_and_as_messages() {
        let mut mgr = ContextManager::new();
        mgr.add_message("user", "hello", MemoryType::Llm);
        mgr.add_message("assistant", "world", MemoryType::Llm);

        let msgs = mgr.as_messages();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[1]["content"], "world");
    }

    #[test]
    fn trim_respects_per_type_limits() {
        let mut mgr = ContextManager::with_limits(2, 2, 2);
        for i in 0..5 {
            mgr.add_message("user", &format!("msg-{i}"), MemoryType::Llm);
        }
        assert_eq!(mgr.get_context_size(), 5);
        mgr.trim();

        // Only the 2 newest Llm messages should remain.
        let llm_count = mgr
            .messages
            .iter()
            .filter(|m| m.memory_type == MemoryType::Llm)
            .count();
        assert_eq!(llm_count, 2);
    }

    #[test]
    fn trim_never_removes_system() {
        let mut mgr = ContextManager::with_limits(0, 0, 0);
        mgr.add_message("system", "you are helpful", MemoryType::Llm);
        mgr.add_message("user", "hi", MemoryType::Llm);
        mgr.trim();

        assert_eq!(mgr.get_context_size(), 1);
        assert_eq!(mgr.messages[0].role, "system");
    }

    #[test]
    fn clear_removes_all() {
        let mut mgr = ContextManager::new();
        mgr.add_message("user", "a", MemoryType::Llm);
        mgr.clear();
        assert_eq!(mgr.get_context_size(), 0);
    }

    #[test]
    fn estimate_tokens_reasonable() {
        let mgr = ContextManager::new();
        let tokens = mgr.estimate_tokens("Hello, world!");
        // Should be a small positive number.
        assert!(tokens > 0 && tokens < 20);
    }

    #[test]
    fn test_context_stats() {
        let mut cm = ContextManager::new();
        cm.add_message("system", "You are a shell", MemoryType::Llm);
        cm.add_message("user", "hello", MemoryType::Llm);
        cm.add_message("assistant", "hi there", MemoryType::Llm);
        cm.add_message("tool", "ls output", MemoryType::Shell);
        cm.add_message("system", "memory context", MemoryType::Knowledge);

        let stats = cm.get_context_stats();
        assert_eq!(stats.total_messages, 5);
        assert_eq!(stats.llm_messages, 3);
        assert_eq!(stats.shell_messages, 1);
        assert_eq!(stats.knowledge_messages, 1);
        assert_eq!(stats.system_messages, 2);
        assert!(stats.estimated_tokens > 0);
    }
}
