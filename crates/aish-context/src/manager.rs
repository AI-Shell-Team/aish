use std::collections::HashMap;

use aish_core::MemoryType;
use aish_core::{PlanModeState, PlanPhase};
use tracing::debug;

use crate::budget::{ContextBudgetPolicy, ContextBudgetState, ContextPressureLevel};
use crate::types::ContextMessage;

const CLEARED_SHELL_OUTPUT: &str =
    "[old shell/tool output cleared by context microcompact; key metadata retained]";
const CLEARED_LLM_OUTPUT: &str = "[old low-value output cleared by context microcompact]";
const SUMMARY_TAG: &str = "<conversation-summary";

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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MicrocompactReport {
    pub changed_messages: usize,
    pub reclaimed_tokens: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FullCompactReport {
    pub compacted_messages: usize,
    pub reclaimed_tokens: usize,
    pub summary_tokens: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContextCompactReport {
    pub before_tokens: usize,
    pub after_tokens: usize,
    pub pressure_before: Option<ContextPressureLevel>,
    pub pressure_after: Option<ContextPressureLevel>,
    pub microcompact: MicrocompactReport,
    pub full_compact: Option<FullCompactReport>,
    pub skipped_full_compact_due_to_fuse: bool,
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
    budget_policy: ContextBudgetPolicy,
    compact_consecutive_failures: usize,
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
            messages: Vec::with_capacity(64),
            max_llm_messages: 50,
            max_shell_messages: 20,
            max_knowledge_messages: 10,
            token_budget: None,
            model: String::new(),
            knowledge_cache: HashMap::new(),
            budget_policy: ContextBudgetPolicy::default(),
            compact_consecutive_failures: 0,
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
            budget_policy: ContextBudgetPolicy::default(),
            compact_consecutive_failures: 0,
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
                tool_calls: None,
                reasoning_content: None,
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
                if let Some(ref calls) = m.tool_calls {
                    obj.insert("tool_calls".into(), serde_json::json!(calls));
                }
                serde_json::Value::Object(obj)
            })
            .collect()
    }

    pub fn messages_snapshot(&self) -> Vec<ContextMessage> {
        self.messages.clone()
    }

    pub fn replace_messages(&mut self, messages: Vec<ContextMessage>) {
        self.messages = messages;
    }

    /// Estimate the token count for a piece of text using the cl100k_base
    /// tokenizer.
    ///
    /// Falls back to a rough `len / 4` heuristic when the tokenizer is
    /// unavailable.
    pub fn estimate_tokens(&self, text: &str) -> usize {
        if !self.budget_policy.enable_token_estimation {
            return estimate_tokens_rough(text);
        }
        let bpe = tiktoken_rs::cl100k_base_singleton();
        let guard = bpe.lock();
        guard.encode_with_special_tokens(text).len()
    }

    pub fn estimate_message_tokens(&self, msg: &ContextMessage) -> usize {
        // Tool-call arguments (bash/write_file payloads) and reasoning
        // content must count toward the budget or compaction triggers late.
        let tool_calls_len: usize = msg
            .tool_calls
            .iter()
            .flatten()
            .map(|tc| tc.name.len() + tc.arguments.len())
            .sum();
        let reasoning_len = msg.reasoning_content.as_ref().map(|r| r.len()).unwrap_or(0);
        self.estimate_tokens(&msg.content) + (tool_calls_len + reasoning_len) / 4
    }

    /// Return the total number of stored messages.
    pub fn get_context_size(&self) -> usize {
        self.messages.len()
    }

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

    pub fn budget_state(&self) -> ContextBudgetState {
        let estimated_tokens: usize = self
            .messages
            .iter()
            .map(|m| self.estimate_message_tokens(m))
            .sum();
        self.budget_policy.state_for_tokens(estimated_tokens)
    }

    pub fn budget_policy(&self) -> &ContextBudgetPolicy {
        &self.budget_policy
    }

    pub fn set_budget_policy(&mut self, policy: ContextBudgetPolicy) {
        self.budget_policy = policy;
    }

    pub fn compact_consecutive_failures(&self) -> usize {
        self.compact_consecutive_failures
    }

    pub fn record_compact_failure(&mut self) {
        self.compact_consecutive_failures = self.compact_consecutive_failures.saturating_add(1);
    }

    pub fn reset_compact_failures(&mut self) {
        self.compact_consecutive_failures = 0;
    }

    pub fn full_compact_candidate_messages(&self) -> Vec<ContextMessage> {
        let recent_start = self.snapped_recent_start();
        self.messages
            .iter()
            .take(recent_start)
            .filter(|m| m.role != "system" && !m.content.starts_with(SUMMARY_TAG))
            .cloned()
            .collect()
    }

    /// Raw recent-window boundary snapped outward so an assistant tool-call
    /// group is never split: either the whole group stays in the kept recent
    /// tail, or the whole group is summarized away. Both
    /// `full_compact_candidate_messages` and `rewrite_with_summary` must use
    /// this same boundary or a straddling group would be duplicated into
    /// both the summary and the retained tail.
    fn snapped_recent_start(&self) -> usize {
        let keep_recent = self.budget_policy.micro_keep_recent_messages.max(2);
        let raw = self.messages.len().saturating_sub(keep_recent);
        let mut boundary = raw;
        while boundary > 0 && self.messages[boundary - 1].role == "tool" {
            boundary -= 1;
        }
        if boundary > 0
            && self.messages[boundary - 1]
                .tool_calls
                .as_ref()
                .is_some_and(|c| !c.is_empty())
        {
            boundary -= 1;
        }
        boundary
    }

    pub fn apply_full_compact_summary(
        &mut self,
        summary: String,
    ) -> Result<FullCompactReport, String> {
        self.rewrite_with_summary(summary, self.full_compact_candidate_messages())
    }

    pub fn compact_for_send(&mut self, plan_state: Option<&PlanModeState>) -> ContextCompactReport {
        let before_state = self.budget_state();
        let before_tokens = before_state.estimated_tokens;
        let mut report = ContextCompactReport {
            before_tokens,
            pressure_before: Some(before_state.pressure),
            ..ContextCompactReport::default()
        };

        if !self.budget_policy.enabled {
            report.after_tokens = before_tokens;
            report.pressure_after = Some(before_state.pressure);
            return report;
        }

        if matches!(
            before_state.pressure,
            ContextPressureLevel::Warning
                | ContextPressureLevel::AutoCompact
                | ContextPressureLevel::Blocking
        ) {
            report.microcompact = self.microcompact();
        }

        let after_micro_state = self.budget_state();
        if after_micro_state.is_above_auto_compact_threshold
            && self.budget_policy.full_compact_enabled
        {
            if self.compact_consecutive_failures >= self.budget_policy.max_consecutive_failures {
                report.skipped_full_compact_due_to_fuse = true;
                tracing::warn!(
                    failures = self.compact_consecutive_failures,
                    "context full compact skipped because failure fuse is open"
                );
            } else {
                match self.full_compact(plan_state) {
                    Ok(full_report) => {
                        self.reset_compact_failures();
                        report.full_compact = Some(full_report);
                    }
                    Err(err) => {
                        self.record_compact_failure();
                        tracing::warn!(
                            error = %err,
                            failures = self.compact_consecutive_failures,
                            "context full compact failed; keeping original context"
                        );
                    }
                }
            }
        }

        let after_state = self.budget_state();
        report.after_tokens = after_state.estimated_tokens;
        report.pressure_after = Some(after_state.pressure);
        report
    }

    pub fn microcompact(&mut self) -> MicrocompactReport {
        let mut changed_messages = 0usize;
        let keep_recent_messages = self.budget_policy.micro_keep_recent_messages;
        let recent_start = self.messages.len().saturating_sub(keep_recent_messages);

        // Cache-warm suffix guard (modeled on oh-my-pi's
        // cacheWarmSuffixTokens): rewriting a message whose suffix already
        // went on the wire forces the provider to re-write that entire
        // suffix at cache-write premium. Messages deeper than the
        // cache_warm_suffix_tokens budget from the end are left for full
        // compaction (which rebuilds the cache anyway); microcompact only
        // reclaims from the cold tail. The suffix sums double as the
        // before-token total, saving a separate full tokenization pass.
        let mut suffix_tokens = vec![0usize; self.messages.len() + 1];
        for idx in (0..self.messages.len()).rev() {
            suffix_tokens[idx] =
                suffix_tokens[idx + 1] + self.estimate_message_tokens(&self.messages[idx]);
        }
        let before_tokens = suffix_tokens[0];
        let warm_threshold = self.budget_policy.cache_warm_suffix_tokens;
        let cache_warm =
            |idx: usize| suffix_tokens.get(idx + 1).copied().unwrap_or(0) > warm_threshold;

        let shell_indices: Vec<usize> = self
            .messages
            .iter()
            .enumerate()
            .filter_map(|(idx, msg)| {
                (msg.memory_type == MemoryType::Shell && msg.role != "system").then_some(idx)
            })
            .collect();
        let shell_keep_start = shell_indices
            .len()
            .saturating_sub(self.budget_policy.shell_keep_recent_commands);
        let shell_keep: std::collections::HashSet<usize> = shell_indices
            .iter()
            .skip(shell_keep_start)
            .copied()
            .collect();

        for idx in 0..self.messages.len() {
            if cache_warm(idx) {
                continue;
            }
            let msg = &mut self.messages[idx];
            if msg.content.starts_with(SUMMARY_TAG) {
                continue;
            }
            if msg.role == "system" {
                // Aged per-turn transient blocks (env cwd updates, memory
                // recall) are compacted to a minimal placeholder; every
                // other system message (static core, skills, summaries)
                // stays verbatim. No length guard: aging, not size, is
                // the trigger.
                if idx < recent_start && is_transient_system(&msg.content) {
                    msg.content = "[cleared]".to_string();
                    changed_messages += 1;
                }
                continue;
            }

            let replacement = if msg.memory_type == MemoryType::Shell && !shell_keep.contains(&idx)
            {
                compact_shell_content(&msg.content)
            } else if idx < recent_start && is_low_value_output(&msg.content) {
                compact_low_value_content(&msg.content)
            } else {
                None
            };

            if let Some(content) = replacement {
                if content.len() < msg.content.len() {
                    msg.content = content;
                    changed_messages += 1;
                }
            }
        }

        let after_tokens = self.total_estimated_tokens();
        let reclaimed_tokens = before_tokens.saturating_sub(after_tokens);
        if changed_messages > 0 {
            tracing::info!(
                changed_messages,
                reclaimed_tokens,
                "context microcompact completed"
            );
        }
        MicrocompactReport {
            changed_messages,
            reclaimed_tokens,
        }
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

    /// Inject knowledge if not already present with the given tag.
    /// Returns true if content was actually added/updated, false if unchanged.
    /// Uses the knowledge_cache to detect changes. When content changes, the
    /// previous cached content is used to locate and replace the old message.
    pub fn inject_knowledge_stable(&mut self, tag: &str, content: &str) -> bool {
        let cache_key = tag.to_string();
        if let Some(cached) = self.knowledge_cache.get(&cache_key) {
            if cached == content {
                return false; // No change — cache stable
            }
            // Content changed — remove the old message by matching its exact content
            let old = cached.clone();
            self.messages
                .retain(|m| !(m.memory_type == MemoryType::Knowledge && m.content == old));
        }
        if !content.is_empty() {
            self.add_message("system", content, MemoryType::Knowledge);
        }
        self.knowledge_cache.insert(cache_key, content.to_string());
        true
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Remove the oldest non-system messages of a given type until we are
    /// within the specified limit.
    ///
    /// Assistant tool-call messages and their tool results form atomic
    /// groups: removing either side would leave a dangling tool_call_id or
    /// an orphan tool result, both rejected by provider APIs. Groups are
    /// counted and removed as a unit.
    fn trim_by_type(&mut self, memory_type: MemoryType, limit: usize) {
        let groups = tool_call_groups(&self.messages);
        let count: usize = groups
            .iter()
            .filter(|g| {
                g.iter().any(|&i| {
                    self.messages[i].memory_type == memory_type && self.messages[i].role != "system"
                })
            })
            .map(|g| g.len())
            .sum();

        if count <= limit {
            return;
        }

        let to_remove = count - limit;
        let mut removed = 0usize;
        let mut remove_indices: Vec<usize> = Vec::new();
        for group in &groups {
            if removed >= to_remove {
                break;
            }
            if group.iter().any(|&i| {
                self.messages[i].memory_type == memory_type && self.messages[i].role != "system"
            }) {
                removed += group.len();
                remove_indices.extend_from_slice(group);
            }
        }
        self.retain_indices(&remove_indices);
    }

    /// Remove the oldest non-system messages until the total token count is
    /// within the budget.
    ///
    /// Assistant tool-call groups are removed as atomic units so the
    /// remaining sequence stays protocol-valid.
    fn trim_to_token_budget(&mut self, budget: usize) {
        let total_tokens: usize = self
            .messages
            .iter()
            .map(|m| self.estimate_tokens(&m.content))
            .sum();

        if total_tokens <= budget {
            return;
        }

        // Walk groups forward to compute how many tokens we need to shed,
        // recording which messages should be removed. We stop as soon as
        // the budget is satisfied.
        let mut current = total_tokens;
        let mut remove_indices: Vec<usize> = Vec::new();

        for group in tool_call_groups(&self.messages) {
            if current <= budget {
                break;
            }
            if group.len() == 1 && self.messages[group[0]].role == "system" {
                continue;
            }
            let group_tokens: usize = group
                .iter()
                .map(|&i| self.estimate_tokens(&self.messages[i].content))
                .sum();
            current = current.saturating_sub(group_tokens);
            debug!(
                roles = ?group.iter().map(|&i| self.messages[i].role.as_str()).collect::<Vec<_>>(),
                group_tokens,
                "trimmed message group for token budget"
            );
            remove_indices.extend_from_slice(&group);
        }
        self.retain_indices(&remove_indices);
    }

    /// Remove the given message indices (single O(n) retain pass).
    fn retain_indices(&mut self, remove: &[usize]) {
        let to_drop: std::collections::HashSet<usize> = remove.iter().copied().collect();
        let mut idx = 0;
        self.messages.retain(|_| {
            let keep = !to_drop.contains(&idx);
            idx += 1;
            keep
        });
    }

    fn total_estimated_tokens(&self) -> usize {
        self.messages
            .iter()
            .map(|m| self.estimate_message_tokens(m))
            .sum()
    }

    fn full_compact(
        &mut self,
        plan_state: Option<&PlanModeState>,
    ) -> Result<FullCompactReport, String> {
        let old_messages = self.full_compact_candidate_messages();

        if old_messages.is_empty() {
            return Err("no old messages available for full compact".to_string());
        }

        let summary = build_ops_summary(
            &old_messages,
            plan_state,
            self.budget_policy.summary_max_tokens,
        );
        if summary.trim().is_empty() {
            return Err("compact summary is empty".to_string());
        }

        self.rewrite_with_summary(summary, old_messages)
    }

    fn rewrite_with_summary(
        &mut self,
        summary: String,
        old_messages: Vec<ContextMessage>,
    ) -> Result<FullCompactReport, String> {
        if old_messages.is_empty() {
            return Err("no old messages available for full compact".to_string());
        }
        let summary = normalize_summary(summary);
        if summary.trim().is_empty() {
            return Err("compact summary is empty".to_string());
        }

        let before_tokens = self.total_estimated_tokens();
        let recent_start = self.snapped_recent_start();

        let mut new_messages = Vec::with_capacity(32);
        for (idx, msg) in self.messages.iter().enumerate() {
            if idx >= recent_start {
                break;
            }
            if msg.role == "system"
                && !msg.content.starts_with(SUMMARY_TAG)
                && !is_transient_system(&msg.content)
            {
                new_messages.push(msg.clone());
            }
        }
        new_messages.push(ContextMessage {
            role: "system".to_string(),
            content: summary,
            memory_type: MemoryType::Knowledge,
            name: None,
            tool_call_id: None,
            tool_calls: None,
            reasoning_content: None,
        });
        new_messages.extend(self.messages.iter().skip(recent_start).cloned());

        self.messages = new_messages;
        let after_tokens = self.total_estimated_tokens();
        let summary_tokens = self
            .messages
            .iter()
            .find(|m| m.content.starts_with(SUMMARY_TAG))
            .map(|m| self.estimate_message_tokens(m))
            .unwrap_or_default();
        let reclaimed_tokens = before_tokens.saturating_sub(after_tokens);
        tracing::info!(
            compacted_messages = old_messages.len(),
            reclaimed_tokens,
            "context full compact completed"
        );

        Ok(FullCompactReport {
            compacted_messages: old_messages.len(),
            reclaimed_tokens,
            summary_tokens,
        })
    }
}

/// Partition message indices into atomic groups.
///
/// An assistant message carrying tool_calls starts a group that extends
/// through all immediately-following `tool` messages (their results).
/// Everything else forms single-message groups. Trimming and compaction
/// operate on whole groups so provider tool-call pairing is never broken.
fn tool_call_groups(messages: &[ContextMessage]) -> Vec<Vec<usize>> {
    let mut groups = Vec::new();
    let mut i = 0;
    while i < messages.len() {
        if messages[i]
            .tool_calls
            .as_ref()
            .is_some_and(|calls| !calls.is_empty())
        {
            let start = i;
            i += 1;
            while i < messages.len() && messages[i].role == "tool" {
                i += 1;
            }
            groups.push((start..i).collect::<Vec<usize>>());
        } else {
            groups.push(vec![i]);
            i += 1;
        }
    }
    groups
}

fn estimate_tokens_rough(text: &str) -> usize {
    (text.len() / 4).max(1)
}

fn normalize_summary(summary: String) -> String {
    let trimmed = summary.trim();
    if trimmed.starts_with(SUMMARY_TAG) {
        return trimmed.to_string();
    }
    format!(
        "<conversation-summary source=\"model_auto_compact\">\n{}\n</conversation-summary>",
        trimmed
    )
}

fn is_low_value_output(content: &str) -> bool {
    content.contains("<stdout>")
        || content.contains("<stderr>")
        || content.contains("<offload>")
        || content.contains("</stdout>")
        || content.contains("</stderr>")
        || content.len() > 8_000
}

/// Per-turn transient system blocks. They ride at the natural turn position
/// for cache stability but must not survive full compaction — the current
/// turn's env/recall is re-appended every turn anyway.
fn is_transient_system(content: &str) -> bool {
    content.contains("**Environment Update:**") || content.contains("<long-term-memory")
}

fn compact_shell_content(content: &str) -> Option<String> {
    if content.len() <= 512 && !is_low_value_output(content) {
        return None;
    }
    let mut retained = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("Command:")
            || trimmed.starts_with("Exit code:")
            || trimmed.contains("<return_code>")
            || trimmed.contains("</return_code>")
            || trimmed.contains("<offload>")
            || trimmed.contains("</offload>")
            || trimmed.contains("path")
        {
            retained.push(trimmed.to_string());
        }
    }
    let stderr_tail = extract_tag_tail(content, "stderr", 12);
    if !stderr_tail.is_empty() {
        retained.push("stderr tail:".to_string());
        retained.extend(stderr_tail);
    }
    if retained.is_empty() {
        retained.push(CLEARED_SHELL_OUTPUT.to_string());
    } else {
        retained.insert(0, CLEARED_SHELL_OUTPUT.to_string());
    }
    Some(retained.join("\n"))
}

fn compact_low_value_content(content: &str) -> Option<String> {
    if !is_low_value_output(content) {
        return None;
    }
    let mut retained = vec![CLEARED_LLM_OUTPUT.to_string()];
    let stderr_tail = extract_tag_tail(content, "stderr", 8);
    if !stderr_tail.is_empty() {
        retained.push("stderr tail:".to_string());
        retained.extend(stderr_tail);
    }
    Some(retained.join("\n"))
}

fn extract_tag_tail(content: &str, tag: &str, max_lines: usize) -> Vec<String> {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);
    let Some(start) = content.find(&open) else {
        return Vec::new();
    };
    let body_start = start + open.len();
    let body_end = content[body_start..]
        .find(&close)
        .map(|idx| body_start + idx)
        .unwrap_or(content.len());
    let lines: Vec<&str> = content[body_start..body_end]
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    lines
        .iter()
        .rev()
        .take(max_lines)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|line| (*line).to_string())
        .collect()
}

fn build_ops_summary(
    old_messages: &[ContextMessage],
    plan_state: Option<&PlanModeState>,
    summary_max_tokens: usize,
) -> String {
    let mut user_items = Vec::with_capacity(32);
    let mut assistant_items = Vec::with_capacity(32);
    let mut shell_items = Vec::with_capacity(32);

    for msg in old_messages {
        let preview = summarize_line(&msg.content, 220);
        match msg.memory_type {
            MemoryType::Shell => shell_items.push(preview),
            MemoryType::Knowledge => {}
            MemoryType::Llm => match msg.role.as_str() {
                "user" => user_items.push(preview),
                "assistant" => assistant_items.push(preview),
                _ => {}
            },
        }
    }

    let mut lines = Vec::with_capacity(32);
    lines.push("<conversation-summary source=\"auto_compact\">".to_string());
    lines.push("Summary:".to_string());
    lines.push(format!(
        "- Compacted older context messages: {}.",
        old_messages.len()
    ));
    if let Some(state) = plan_state {
        if state.phase == PlanPhase::Planning {
            lines.push(format!(
                "- Plan mode is active; plan_id={:?}, artifact_path={:?}, draft_revision={}.",
                state.plan_id, state.artifact_path, state.draft_revision
            ));
        }
    }
    append_summary_section(&mut lines, "Earlier user requests", &user_items, 6);
    append_summary_section(
        &mut lines,
        "Earlier assistant responses",
        &assistant_items,
        4,
    );
    append_summary_section(&mut lines, "Older shell observations", &shell_items, 8);
    lines.push("</conversation-summary>".to_string());

    let mut summary = lines.join("\n");
    let max_chars = summary_max_tokens.saturating_mul(4).max(512);
    if summary.len() > max_chars {
        let mut end = max_chars.min(summary.len());
        while end > 0 && !summary.is_char_boundary(end) {
            end -= 1;
        }
        summary.truncate(end);
        summary.push_str("\n</conversation-summary>");
    }
    summary
}

fn append_summary_section(lines: &mut Vec<String>, title: &str, items: &[String], limit: usize) {
    if items.is_empty() {
        return;
    }
    lines.push(format!("{}:", title));
    for item in items
        .iter()
        .rev()
        .take(limit)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        lines.push(format!("- {}", item));
    }
}

fn summarize_line(content: &str, max_chars: usize) -> String {
    let one_line = content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(6)
        .collect::<Vec<_>>()
        .join(" | ");
    if one_line.len() <= max_chars {
        return one_line;
    }
    let mut end = max_chars.min(one_line.len());
    while end > 0 && !one_line.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &one_line[..end])
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
    fn inject_knowledge_stable_first_call_adds() {
        let mut mgr = ContextManager::new();
        let result =
            mgr.inject_knowledge_stable("skills", "<available-skills>\ntest\n</available-skills>");
        assert!(result, "first call should return true (content added)");
        assert_eq!(mgr.get_context_size(), 1);
        assert_eq!(
            mgr.messages[0].content,
            "<available-skills>\ntest\n</available-skills>"
        );
    }

    #[test]
    fn inject_knowledge_stable_same_content_is_noop() {
        let mut mgr = ContextManager::new();
        let content = "<available-skills>\ntest\n</available-skills>";
        mgr.inject_knowledge_stable("skills", content);
        assert_eq!(mgr.get_context_size(), 1);

        let result = mgr.inject_knowledge_stable("skills", content);
        assert!(!result, "second call with same content should return false");
        assert_eq!(
            mgr.get_context_size(),
            1,
            "message count should remain unchanged"
        );
    }

    #[test]
    fn inject_knowledge_stable_different_content_updates() {
        let mut mgr = ContextManager::new();
        mgr.inject_knowledge_stable("skills", "<available-skills>\nold\n</available-skills>");
        assert_eq!(mgr.get_context_size(), 1);

        let result =
            mgr.inject_knowledge_stable("skills", "<available-skills>\nnew\n</available-skills>");
        assert!(result, "call with different content should return true");
        assert_eq!(
            mgr.get_context_size(),
            1,
            "old message replaced, count stays 1"
        );
        assert_eq!(
            mgr.messages[0].content,
            "<available-skills>\nnew\n</available-skills>"
        );
    }

    #[test]
    fn inject_knowledge_stable_empty_content_clears() {
        let mut mgr = ContextManager::new();
        mgr.inject_knowledge_stable("skills", "<available-skills>\ntest\n</available-skills>");
        assert_eq!(mgr.get_context_size(), 1);

        let result = mgr.inject_knowledge_stable("skills", "");
        assert!(result, "clearing should return true");
        assert_eq!(mgr.get_context_size(), 0, "message should be removed");
    }

    #[test]
    fn inject_knowledge_stable_empty_twice_is_noop() {
        let mut mgr = ContextManager::new();
        mgr.inject_knowledge_stable("skills", "");
        let result = mgr.inject_knowledge_stable("skills", "");
        assert!(!result, "empty content called twice should return false");
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

    #[test]
    fn budget_state_uses_policy_thresholds() {
        let mut cm = ContextManager::new();
        cm.set_budget_policy(ContextBudgetPolicy {
            context_window_tokens: 2_000,
            reserved_output_tokens: 200,
            auto_compact_buffer_tokens: 200,
            warning_buffer_tokens: 200,
            blocking_buffer_tokens: 100,
            enable_token_estimation: false,
            ..ContextBudgetPolicy::default()
        });
        cm.add_message("user", &"x".repeat(7_000), MemoryType::Llm);
        let state = cm.budget_state();
        assert!(matches!(
            state.pressure,
            ContextPressureLevel::Warning
                | ContextPressureLevel::AutoCompact
                | ContextPressureLevel::Blocking
        ));
    }

    #[test]
    fn microcompact_clears_old_shell_output_but_keeps_recent() {
        let mut cm = ContextManager::new();
        cm.set_budget_policy(ContextBudgetPolicy {
            shell_keep_recent_commands: 1,
            enable_token_estimation: false,
            ..ContextBudgetPolicy::default()
        });
        cm.add_message(
            "user",
            &format!(
                "Command: journalctl\n<stdout>{}</stdout>\n<return_code>0</return_code>",
                "old noisy output\n".repeat(200)
            ),
            MemoryType::Shell,
        );
        cm.add_message(
            "user",
            "Command: uptime\n<stdout>recent output should stay</stdout>\n<return_code>0</return_code>",
            MemoryType::Shell,
        );

        let report = cm.microcompact();
        assert_eq!(report.changed_messages, 1);
        assert!(cm.messages[0]
            .content
            .contains("old shell/tool output cleared"));
        assert!(cm.messages[1].content.contains("recent output should stay"));
    }

    #[test]
    fn full_compact_rewrites_old_history_into_summary() {
        let mut cm = ContextManager::new();
        cm.set_budget_policy(ContextBudgetPolicy {
            context_window_tokens: 2_000,
            reserved_output_tokens: 200,
            auto_compact_buffer_tokens: 200,
            warning_buffer_tokens: 200,
            blocking_buffer_tokens: 100,
            micro_keep_recent_messages: 2,
            shell_keep_recent_commands: 1,
            enable_token_estimation: false,
            ..ContextBudgetPolicy::default()
        });
        cm.add_message("system", "system prompt", MemoryType::Llm);
        for idx in 0..8 {
            cm.add_message(
                "user",
                &format!("investigate service failure {idx} {}", "x".repeat(1200)),
                MemoryType::Llm,
            );
        }
        cm.add_message("assistant", "recent answer", MemoryType::Llm);
        cm.add_message("user", "recent follow up", MemoryType::Llm);

        let report = cm.compact_for_send(None);
        assert!(report.full_compact.is_some());
        assert!(cm
            .messages
            .iter()
            .any(|m| m.content.starts_with(SUMMARY_TAG)));
        assert_eq!(cm.messages.last().unwrap().content, "recent follow up");
    }

    #[test]
    fn full_compact_failure_increments_fuse() {
        let mut cm = ContextManager::new();
        cm.set_budget_policy(ContextBudgetPolicy {
            context_window_tokens: 1_500,
            reserved_output_tokens: 100,
            auto_compact_buffer_tokens: 100,
            warning_buffer_tokens: 100,
            blocking_buffer_tokens: 50,
            micro_keep_recent_messages: 10,
            max_consecutive_failures: 1,
            enable_token_estimation: false,
            ..ContextBudgetPolicy::default()
        });
        cm.add_message("system", "system prompt", MemoryType::Llm);
        cm.add_message("user", &"x".repeat(10_000), MemoryType::Llm);

        let first = cm.compact_for_send(None);
        assert!(first.full_compact.is_none());
        assert_eq!(cm.compact_consecutive_failures(), 1);
        let second = cm.compact_for_send(None);
        assert!(second.skipped_full_compact_due_to_fuse);
    }

    fn tool_call_msg(id: &str, name: &str) -> ContextMessage {
        ContextMessage {
            role: "assistant".to_string(),
            content: String::new(),
            memory_type: MemoryType::Llm,
            name: None,
            tool_call_id: None,
            tool_calls: Some(vec![aish_core::ContextToolCall {
                id: id.to_string(),
                name: name.to_string(),
                arguments: "{}".to_string(),
            }]),
            reasoning_content: None,
        }
    }

    fn tool_result_msg(id: &str, output: &str) -> ContextMessage {
        ContextMessage {
            role: "tool".to_string(),
            content: output.to_string(),
            memory_type: MemoryType::Llm,
            name: Some("bash".to_string()),
            tool_call_id: Some(id.to_string()),
            tool_calls: None,
            reasoning_content: None,
        }
    }

    #[test]
    fn trim_by_type_keeps_tool_call_groups_atomic() {
        let mut cm = ContextManager::with_limits(3, 10, 10);
        cm.add_message("user", "q1", MemoryType::Llm);
        cm.add_memory(MemoryType::Llm, tool_call_msg("t1", "bash"));
        cm.add_memory(MemoryType::Llm, tool_result_msg("t1", "output1"));
        cm.add_message("user", "q2", MemoryType::Llm);
        cm.add_memory(MemoryType::Llm, tool_call_msg("t2", "bash"));
        cm.add_memory(MemoryType::Llm, tool_result_msg("t2", "output2"));
        cm.trim();

        // With a limit of 3 messages the oldest group (q1 + t1 pair = 3
        // messages) is removed as a unit; the t2 pair stays intact.
        let roles: Vec<&str> = cm.messages.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, vec!["user", "assistant", "tool"]);
        let assistant = &cm.messages[1];
        assert!(assistant
            .tool_calls
            .as_ref()
            .is_some_and(|c| c[0].id == "t2"));
        assert_eq!(cm.messages[2].tool_call_id.as_deref(), Some("t2"));
    }

    #[test]
    fn trim_to_token_budget_removes_groups_atomically() {
        let mut cm = ContextManager::new();
        cm.set_token_budget(Some(1)); // force maximum trimming
        cm.add_message("user", &"x".repeat(64), MemoryType::Llm);
        cm.add_memory(MemoryType::Llm, tool_call_msg("t1", "bash"));
        cm.add_memory(MemoryType::Llm, tool_result_msg("t1", "output"));
        cm.trim();

        // Either the whole group survives or nothing does — never an
        // orphan tool result or a dangling tool_call.
        let has_tool = cm.messages.iter().any(|m| m.role == "tool");
        let has_tool_calls = cm
            .messages
            .iter()
            .any(|m| m.tool_calls.as_ref().is_some_and(|c| !c.is_empty()));
        assert_eq!(has_tool, has_tool_calls);
    }

    #[test]
    fn rewrite_with_summary_does_not_split_tool_call_group() {
        let mut cm = ContextManager::new();
        cm.set_budget_policy(ContextBudgetPolicy {
            context_window_tokens: 2_000,
            reserved_output_tokens: 200,
            auto_compact_buffer_tokens: 200,
            warning_buffer_tokens: 200,
            blocking_buffer_tokens: 100,
            micro_keep_recent_messages: 2,
            enable_token_estimation: false,
            ..ContextBudgetPolicy::default()
        });
        for idx in 0..8 {
            cm.add_message(
                "user",
                &format!("investigate {idx} {}", "x".repeat(1200)),
                MemoryType::Llm,
            );
        }
        cm.add_memory(MemoryType::Llm, tool_call_msg("t9", "bash"));
        cm.add_memory(MemoryType::Llm, tool_result_msg("t9", "recent output"));

        let report = cm.compact_for_send(None);
        assert!(report.full_compact.is_some());

        // The recent tool-call group must survive full compaction intact.
        let tail: Vec<&str> = cm.messages.iter().map(|m| m.role.as_str()).collect();
        let last_two = &tail[tail.len() - 2..];
        assert_eq!(last_two, vec!["assistant", "tool"]);
    }

    #[test]
    fn legacy_snapshot_without_tool_calls_deserializes() {
        let legacy = r#"{
            "role": "assistant",
            "content": "hello",
            "memory_type": "Llm",
            "name": null,
            "tool_call_id": null
        }"#;
        let msg: ContextMessage = serde_json::from_str(legacy).expect("legacy snapshot must parse");
        assert_eq!(msg.role, "assistant");
        assert!(msg.tool_calls.is_none());
    }

    #[test]
    fn as_messages_includes_tool_calls() {
        let mut cm = ContextManager::new();
        cm.add_memory(MemoryType::Llm, tool_call_msg("t1", "bash"));
        let messages = cm.as_messages();
        let tc = messages[0].get("tool_calls").expect("tool_calls present");
        let call = &tc[0];
        assert_eq!(call["id"], "t1");
        assert_eq!(call["name"], "bash");
    }

    #[test]
    fn estimate_message_tokens_counts_tool_call_arguments() {
        let cm = ContextManager::new();
        let plain = ContextMessage {
            role: "assistant".to_string(),
            content: "x".repeat(400),
            memory_type: MemoryType::Llm,
            name: None,
            tool_call_id: None,
            tool_calls: None,
            reasoning_content: None,
        };
        let with_calls = ContextMessage {
            tool_calls: Some(vec![aish_core::ContextToolCall {
                id: "t1".to_string(),
                name: "bash".to_string(),
                arguments: "y".repeat(400),
            }]),
            ..plain.clone()
        };
        assert!(cm.estimate_message_tokens(&with_calls) > cm.estimate_message_tokens(&plain));
    }

    #[test]
    fn full_compact_candidates_use_snapped_group_boundary() {
        let mut cm = ContextManager::new();
        cm.set_budget_policy(ContextBudgetPolicy {
            micro_keep_recent_messages: 2,
            enable_token_estimation: false,
            ..ContextBudgetPolicy::default()
        });
        cm.add_message("user", "q1", MemoryType::Llm);
        cm.add_memory(MemoryType::Llm, tool_call_msg("t9", "bash"));
        cm.add_memory(MemoryType::Llm, tool_result_msg("t9", "recent output"));

        // Raw boundary (len - keep_recent) = 1 lands between the assistant
        // and its result; the snapped boundary must exclude the whole group.
        let candidates = cm.full_compact_candidate_messages();
        assert!(candidates
            .iter()
            .all(|m| m.tool_calls.is_none() && m.role != "tool"));
    }

    #[test]
    fn microcompact_clears_aged_transient_system_blocks_only() {
        let mut cm = ContextManager::new();
        cm.set_budget_policy(ContextBudgetPolicy {
            micro_keep_recent_messages: 2,
            enable_token_estimation: false,
            ..ContextBudgetPolicy::default()
        });
        cm.add_message("system", "static skills appendix", MemoryType::Knowledge);
        cm.add_memory(
            MemoryType::Llm,
            ContextMessage {
                role: "system".to_string(),
                content: "**Environment Update:**\n- Current directory: /old/path".to_string(),
                memory_type: MemoryType::Llm,
                name: None,
                tool_call_id: None,
                tool_calls: None,
                reasoning_content: None,
            },
        );
        cm.add_memory(
            MemoryType::Llm,
            ContextMessage {
                role: "system".to_string(),
                content:
                    "<long-term-memory source=\"recall\">\n- [Env] old fact\n</long-term-memory>"
                        .to_string(),
                memory_type: MemoryType::Llm,
                name: None,
                tool_call_id: None,
                tool_calls: None,
                reasoning_content: None,
            },
        );
        // Recent tail keeps the current turn intact.
        cm.add_message("user", "current question", MemoryType::Llm);
        cm.add_message("assistant", "current answer", MemoryType::Llm);

        let report = cm.microcompact();
        assert_eq!(report.changed_messages, 2);
        assert_eq!(cm.messages[0].content, "static skills appendix");
        assert!(!cm.messages[1].content.contains("/old/path"));
        assert!(!cm.messages[2].content.contains("old fact"));
        assert_eq!(cm.messages[3].content, "current question");
    }

    #[test]
    fn microcompact_skips_messages_inside_warm_cache_suffix() {
        // An old low-value output followed by >8k tokens of conversation
        // sits in the provider's warm prefix: rewriting it would bust the
        // cache for the whole suffix. It must be skipped; only the cold
        // tail (small suffix) is compacted.
        let mut cm = ContextManager::new();
        cm.set_budget_policy(ContextBudgetPolicy {
            micro_keep_recent_messages: 2,
            enable_token_estimation: false,
            ..ContextBudgetPolicy::default()
        });
        // Warm message: old, low-value, big suffix.
        cm.add_message(
            "user",
            &format!("<stdout>{}</stdout>", "warm old output ".repeat(64)),
            MemoryType::Llm,
        );
        // >8k tokens after it (32k chars of content / 4).
        for i in 0..8 {
            cm.add_message(
                "assistant",
                &format!("turn-{i}-{}", "x".repeat(4_000)),
                MemoryType::Llm,
            );
        }
        // Cold message: low-value, tiny suffix.
        cm.add_message(
            "user",
            &format!("<stdout>{}</stdout>", "cold old output ".repeat(64)),
            MemoryType::Llm,
        );
        cm.add_message("assistant", "after cold", MemoryType::Llm);
        cm.add_message("user", "recent question", MemoryType::Llm);
        cm.add_message("assistant", "recent answer", MemoryType::Llm);

        let report = cm.microcompact();
        // Exactly the cold message is rewritten; the warm one is skipped.
        assert_eq!(report.changed_messages, 1);
        assert!(cm.messages[0].content.contains("warm old output"));
        assert!(cm.messages[9].content.contains("cleared"));
    }
}
