use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use aish_config::MemoryConfig;
use aish_context::ContextMessage;
use aish_context::{
    ContextBudgetPolicy, ContextCompactReport, ContextManager, ContextPressureLevel,
};
use aish_core::{LlmEvent, MemoryCategory, MemoryType, PlanModeState, PlanPhase};
use aish_llm::{ChatMessage, LlmCallbackResult, LlmSession, MessageContent, Tool};
use aish_memory::MemoryManager;
use aish_prompts::PromptManager;
use aish_session::SessionContextMessage;
use aish_skills::SharedSkillManager;

/// Shared handle for the memory manager, accessible from both AiHandler and tools.
pub type SharedMemoryManager = Arc<Mutex<Option<MemoryManager>>>;

/// Classify a fact string into a memory category using keyword matching.
fn categorize_fact(fact: &str) -> MemoryCategory {
    let lower = fact.to_lowercase();

    // Preference keywords
    if lower.contains("prefer")
        || lower.contains("like")
        || lower.contains("always")
        || lower.contains("never")
        || lower.contains("favorite")
        || lower.contains("favourite")
        || lower.contains("default")
        || lower.contains("want")
        || lower.contains("don't like")
        || lower.contains("avoid")
    {
        return MemoryCategory::Preference;
    }

    // Environment keywords
    if lower.contains("port")
        || lower.contains("host")
        || lower.contains("ip ")
        || lower.contains("server")
        || lower.contains("database")
        || lower.contains("db ")
        || lower.contains("path")
        || lower.contains("directory")
        || lower.contains("folder")
        || lower.contains("version")
        || lower.contains("config")
        || lower.contains("url")
        || lower.contains("endpoint")
        || lower.contains("api ")
        || lower.contains("token")
        || lower.contains("key")
        || lower.contains("password")
        || lower.contains("credential")
    {
        return MemoryCategory::Environment;
    }

    // Solution keywords
    if lower.contains("fix")
        || lower.contains("solve")
        || lower.contains("resolved")
        || lower.contains("error")
        || lower.contains("issue")
        || lower.contains("bug")
        || lower.contains("workaround")
        || lower.contains("solution")
        || lower.contains("patch")
    {
        return MemoryCategory::Solution;
    }

    // Pattern keywords
    if lower.contains("pattern")
        || lower.contains("convention")
        || lower.contains("standard")
        || lower.contains("practice")
        || lower.contains("rule")
        || lower.contains("style")
        || lower.contains("workflow")
        || lower.contains("approach")
    {
        return MemoryCategory::Pattern;
    }

    MemoryCategory::Other
}

/// English stop words to filter from queries.
const STOP_WORDS: &[&str] = &[
    "a", "an", "the", "is", "are", "was", "were", "be", "been", "being", "have", "has", "had",
    "do", "does", "did", "will", "would", "could", "should", "may", "might", "can", "shall",
    "must", "how", "what", "where", "when", "why", "who", "which", "whom", "this", "that", "these",
    "those", "it", "its", "i", "me", "my", "we", "our", "you", "your", "he", "she", "they", "them",
    "their", "and", "or", "but", "not", "no", "nor", "so", "if", "then", "than", "too", "very",
    "just", "about", "above", "after", "before", "between", "into", "through", "during", "from",
    "with", "for", "at", "by", "to", "of", "in", "on", "up", "out", "off", "over", "under",
    "again", "all", "each", "every", "both", "few", "more", "most", "other", "some", "such",
    "only", "own", "same", "also", "there", "here",
];

/// Extract meaningful keywords from a query string for memory search.
fn extract_keywords(query: &str) -> Vec<String> {
    let lower = query.to_lowercase();
    let words: Vec<String> = lower
        .split(|c: char| c.is_whitespace() || ".,;:!?()[]{}\"'`/\\@#$%^&*+=|<>~".contains(c))
        .filter(|w| !w.is_empty() && w.len() >= 2)
        .filter(|w| !STOP_WORDS.contains(&&w[..]))
        .map(|w| w.to_string())
        .collect();

    let mut seen = std::collections::HashSet::new();
    words
        .into_iter()
        .filter(|w| seen.insert(w.clone()))
        .take(10)
        .collect()
}

/// Pre-compiled regex patterns for fact extraction.
static RETAIN_PATTERNS: LazyLock<Vec<regex::Regex>> = LazyLock::new(|| {
    let patterns: &[&str] = &[
        r"(?i)^(?:please\s+)?remember(?:\s+that)?\s+(.+)",
        r"(?i)^(?:please\s+)?note(?:\s+that)?\s+(.+)",
        r"(?i)^(for\s+future\s+reference[:,]?\s*)(.+)",
        r"(?i)^(i\s+prefer.+)",
        r"(?i)^(my\s+preferred.+)",
        r"(?i)^(my\s+project.+)",
        r"(?i)^(i\s+(?:always|never)\s+.+)",
        r"(?i)^(we\s+use.+)",
        r"(?i)^(our\s+.+\s+(?:is|are)\s+.+)",
        r"(?i)^(the\s+(?:api|server|database|port|host|endpoint)\s+.+)",
    ];
    patterns
        .iter()
        .filter_map(|pat| regex::Regex::new(pat).ok())
        .collect()
});

/// Extract retainable facts from user input using pattern matching.
/// Returns a list of facts that should be stored in long-term memory.
fn extract_retainable_facts(input: &str) -> Vec<String> {
    let mut facts = Vec::new();
    let cleaned = input.trim();

    for re in RETAIN_PATTERNS.iter() {
        if let Some(caps) = re.captures(cleaned) {
            let fact: Option<String> = caps
                .get(2)
                .or_else(|| caps.get(1))
                .map(|m: regex::Match<'_>| m.as_str().trim().to_string());
            if let Some(ref f) = fact {
                let f: &str = f.trim_end_matches(|c: char| ".!,;:".contains(c));
                if f.len() >= 8 && f.len() <= 240 {
                    facts.push(f.to_string());
                }
            }
        }
    }

    facts
}

/// Handles AI question interaction, including sending prompts to the LLM,
/// managing context, memory recall/retain, and skill injection.
pub struct AiHandler {
    llm_session: LlmSession,
    context_manager: ContextManager,
    memory_manager: SharedMemoryManager,
    skill_manager: SharedSkillManager,
    memory_config: MemoryConfig,
    prompt_manager: PromptManager,
    token_store: crate::token_store::TokenUsageStore,
}

impl AiHandler {
    pub fn new(
        llm_session: LlmSession,
        memory_manager: SharedMemoryManager,
        skill_manager: SharedSkillManager,
        memory_config: MemoryConfig,
        max_llm_messages: usize,
        max_shell_messages: usize,
        token_budget: Option<usize>,
        context_budget_policy: ContextBudgetPolicy,
    ) -> Self {
        let mut context_manager = ContextManager::with_limits(
            max_llm_messages,
            max_shell_messages,
            10, // max_knowledge_messages
        );
        context_manager.set_token_budget(token_budget);
        context_manager.set_budget_policy(context_budget_policy);
        Self {
            llm_session,
            context_manager,
            memory_manager,
            skill_manager,
            memory_config,
            prompt_manager: PromptManager::default_dir(),
            token_store: crate::token_store::TokenUsageStore::open(
                crate::token_store::TokenUsageStore::default_path(),
            ),
        }
    }

    /// Set the event callback for real-time LLM streaming display.
    pub fn set_event_callback(
        &mut self,
        cb: Arc<dyn Fn(LlmEvent) -> Option<LlmCallbackResult> + Send + Sync>,
    ) {
        self.llm_session.set_event_callback(cb);
    }

    /// Trigger cancellation of the current LLM operation.
    pub fn cancel(&self) {
        self.llm_session.cancellation_token().cancel();
    }

    /// Get a reference to the LLM session's cancellation token.
    pub fn cancellation_token(&self) -> &aish_llm::CancellationToken {
        self.llm_session.cancellation_token()
    }

    /// Get a shared reference to the cancellation token for use in tools.
    pub fn cancellation_token_arc(&self) -> std::sync::Arc<aish_llm::CancellationToken> {
        self.llm_session.cancellation_token_arc()
    }

    /// Add a shell command result to the LLM context so the AI can reference
    /// previous command output in follow-up questions.
    pub fn add_shell_context(&mut self, entry: &str) {
        self.context_manager
            .add_message("user", entry, MemoryType::Shell);
        self.context_manager.trim();
    }

    pub fn context_messages_snapshot(&self) -> Vec<ContextMessage> {
        self.context_manager.messages_snapshot()
    }

    pub fn restore_context_messages(&mut self, messages: Vec<ContextMessage>) {
        self.context_manager.replace_messages(messages);
        self.context_manager.trim();
    }

    pub fn export_session_context_snapshot(&self) -> Vec<SessionContextMessage> {
        self.context_manager
            .messages_snapshot()
            .into_iter()
            .map(|message| SessionContextMessage {
                role: message.role,
                content: message.content,
                memory_type: message.memory_type,
                name: message.name,
                tool_call_id: message.tool_call_id,
            })
            .collect()
    }

    pub fn restore_session_context_snapshot(&mut self, messages: Vec<SessionContextMessage>) {
        let restored = messages
            .into_iter()
            .map(|message| ContextMessage {
                role: message.role,
                content: message.content,
                memory_type: message.memory_type,
                name: message.name,
                tool_call_id: message.tool_call_id,
            })
            .collect();
        self.restore_context_messages(restored);
    }

    /// Get the current plan phase from the LLM session.
    pub fn plan_phase(&self) -> PlanPhase {
        self.llm_session.plan_state().lock().unwrap().phase.clone()
    }

    /// Transition to plan mode: set phase to Planning and initialize plan state.
    pub fn enter_plan_mode(&mut self, session_uuid: &str) {
        use aish_core::plan;
        let new_state = plan::create_new_plan_state(session_uuid);
        let plan_state = self.llm_session.plan_state();
        let mut state = plan_state.lock().unwrap();
        *state = new_state;
    }

    /// Transition out of plan mode: set phase back to Normal.
    pub fn exit_plan_mode(&mut self) {
        let plan_state = self.llm_session.plan_state();
        let mut state = plan_state.lock().unwrap();
        state.phase = PlanPhase::Normal;
    }

    /// Toggle between plan mode and normal mode.
    /// Returns the new phase after toggling.
    pub fn toggle_plan_mode(&mut self, session_uuid: &str) -> PlanPhase {
        let current = self.plan_phase();
        match current {
            PlanPhase::Planning => {
                self.exit_plan_mode();
                PlanPhase::Normal
            }
            PlanPhase::Normal => {
                self.enter_plan_mode(session_uuid);
                PlanPhase::Planning
            }
        }
    }

    /// Get a snapshot of the current plan state.
    pub fn plan_state(&self) -> PlanModeState {
        self.llm_session.plan_state().lock().unwrap().clone()
    }

    /// Get a handle to the underlying plan state mutex for direct mutation.
    pub fn plan_state_ptr(&self) -> Arc<Mutex<PlanModeState>> {
        self.llm_session.plan_state()
    }

    /// Update the model in the underlying LLM session.
    pub fn update_model(&mut self, model: &str, api_base: Option<&str>, api_key: Option<&str>) {
        self.llm_session.update_model(model, api_base, api_key);
    }

    pub fn register_tool(&mut self, tool: Box<dyn Tool>) {
        self.llm_session.register_tool(tool);
    }

    /// Return a snapshot of token usage statistics for the last 7 days.
    pub fn token_stats(&self) -> aish_llm::TokenStats {
        self.token_store.stats()
    }

    /// Return cumulative token usage for the current session (not persisted store).
    pub fn session_token_stats(&self) -> aish_llm::TokenStats {
        self.llm_session.token_stats()
    }

    /// Return locally estimated prompt tokens from the last API call.
    pub fn last_prompt_estimate(&self) -> u64 {
        self.llm_session.last_prompt_estimate()
    }

    /// Return current context budget state (estimated tokens, pressure, %).
    pub fn context_budget_state(&self) -> aish_context::ContextBudgetState {
        self.context_manager.budget_state()
    }

    /// Persist token usage delta from the current session to disk.
    pub fn persist_token_usage(&mut self) {
        let stats = self.llm_session.token_stats();
        self.token_store.record_session_delta(
            stats.total_input,
            stats.total_output,
            stats.request_count,
        );
    }

    /// Run read-only failure diagnosis via shell-only `command-diagnose` spawn.
    pub async fn handle_failure_diagnose(
        &mut self,
        command: &str,
        exit_code: i32,
        output: &str,
        cwd: &str,
    ) -> aish_core::Result<FailureDiagnoseParseResult> {
        use aish_llm::{spawn_definition, AgentDefinition, LoopStatus};

        let output_trunc = if output.len() > 4096 {
            &output[..floor_char_boundary(output, 4096)]
        } else {
            output
        };

        let query = format!(
            "Investigate why this command failed and put a diagnose_report JSON in your final message.\n\
             Command: {command}\nExit code: {exit_code}\nCWD: {cwd}"
        );

        let system_prompt =
            self.failure_diagnose_system_prompt(command, exit_code, output_trunc, cwd);
        let def = AgentDefinition::command_diagnose(system_prompt);

        let result = spawn_definition(&self.llm_session, &def, &query, |_sub, _specs| {}).await;

        self.persist_token_usage();

        match result.status {
            LoopStatus::Cancelled => Err(aish_core::AishError::Cancelled),
            LoopStatus::Fatal => Err(aish_core::AishError::Llm(format!(
                "command-diagnose failed: {}",
                result.text
            ))),
            LoopStatus::Complete | LoopStatus::Incomplete => {
                Ok(parse_diagnose_report_response(&result.text))
            }
        }
    }

    fn failure_diagnose_system_prompt(
        &mut self,
        command: &str,
        exit_code: i32,
        output: &str,
        cwd: &str,
    ) -> String {
        let role_prompt = self.prompt_manager.get("role").to_string();
        let mut vars = HashMap::new();
        vars.insert("role_prompt".to_string(), role_prompt);
        vars.insert("uname_info".to_string(), uname_info());
        vars.insert("user_nickname".to_string(), whoami());
        vars.insert("os_info".to_string(), os_info());
        vars.insert("basic_env_info".to_string(), basic_env_info());
        vars.insert("output_language".to_string(), output_language());
        vars.insert("failed_command".to_string(), command.to_string());
        vars.insert("exit_code".to_string(), exit_code.to_string());
        vars.insert("cwd".to_string(), cwd.to_string());
        vars.insert("command_output".to_string(), output.to_string());
        self.prompt_manager.render("failure_diagnose", &vars)
    }

    /// Handle an AI question: send to LLM and return the response text.
    pub async fn handle_question(&mut self, question: &str) -> aish_core::Result<String> {
        // Step 1: Process @skill references in the question
        let question_processed = self.inject_skill_prefix(question);

        // Step 2: Auto-recall relevant memories into context
        self.recall_memories(&question_processed);

        // Step 3: Inject loaded skills into context as knowledge
        self.inject_skills();

        // Step 4: Compact persistent context before building messages.
        let plan_state = self.plan_state();
        let compact_report = self.compact_context_before_send(&plan_state).await;
        if compact_report.microcompact.changed_messages > 0 || compact_report.full_compact.is_some()
        {
            tracing::info!(
                before_tokens = compact_report.before_tokens,
                after_tokens = compact_report.after_tokens,
                micro_changed = compact_report.microcompact.changed_messages,
                full_compact = compact_report.full_compact.is_some(),
                "AI context compacted before question send"
            );
        }

        // Step 5: Build context and system messages.
        // Split system message into static core (cached) and dynamic env block.
        // The static core goes as the main system_message; the env block is
        // prepended to context_messages so the cache breakpoint lands on the
        // stable prefix.
        let (static_core, env_block) = self.system_message_parts();
        let mut context_messages = self.build_context_messages();
        context_messages.insert(0, ChatMessage::system(&env_block));

        // Step 6: Extract images from the question text
        let extracted = crate::image::extract_images(&question_processed);
        for warning in &extracted.warnings {
            eprintln!("{}", warning);
        }
        if !extracted.attached.is_empty() {
            for img in &extracted.attached {
                if img.size_bytes >= 1024 {
                    eprintln!(
                        "{}",
                        crate::theme::faint(&format!(
                            "📎 Attached image: {} ({:.1} KB)",
                            img.filename,
                            img.size_bytes as f64 / 1024.0
                        ))
                    );
                } else {
                    eprintln!(
                        "{}",
                        crate::theme::faint(&format!(
                            "📎 Attached image: {} ({} B)",
                            img.filename, img.size_bytes
                        ))
                    );
                }
            }
            if extracted.attached.len() > 1 {
                eprintln!(
                    "{}",
                    crate::theme::faint(&format!(
                        "📎 {} images attached",
                        extracted.attached.len()
                    ))
                );
            }
        }

        // Step 7: Build user message (with or without images)
        let user_msg = if extracted.image_urls.is_empty() {
            ChatMessage::user(&question_processed)
        } else {
            ChatMessage::user_with_images(extracted.cleaned_text, extracted.image_urls)
        };
        let process_result = self
            .llm_session
            .process_input(&user_msg, &context_messages, Some(&static_core), true)
            .await?;
        let response = process_result.text;

        // Step 7: Store the exchange in context
        self.context_manager
            .add_message("user", &question_processed, MemoryType::Llm);
        self.context_manager
            .add_message("assistant", &response, MemoryType::Llm);
        self.context_manager.trim();

        // Step 8: Auto-retain user preferences/facts
        self.auto_retain_memory(&question_processed, &response);

        // Step 9: Persist token usage delta to disk
        self.persist_token_usage();

        Ok(response)
    }

    /// Handle error correction: analyze a failed command and suggest a fix.
    pub async fn handle_error_correction(
        &mut self,
        command: &str,
        exit_code: i32,
        stderr: &str,
    ) -> aish_core::Result<ErrorCorrectionResult> {
        let prompt = format!(
            "<command_result>\nCommand: {}\nExit code: {}\n</command_result>\n\n\
             Please analyze the error and suggest a fix. \
             Check the shell history context above for the actual error output.",
            command, exit_code
        );

        let context_messages = self.build_context_messages();
        let system_message = self.error_correction_system_message(command, exit_code, stderr);

        let user_msg = ChatMessage::user(&prompt);
        let process_result = self
            .llm_session
            .process_input(
                &user_msg,
                &context_messages,
                system_message.as_deref(),
                true,
            )
            .await?;
        let response = process_result.text;

        // Persist token usage delta to disk
        self.persist_token_usage();

        Ok(parse_error_correction_response(&response))
    }

    async fn compact_context_before_send(
        &mut self,
        plan_state: &PlanModeState,
    ) -> ContextCompactReport {
        let before_state = self.context_manager.budget_state();
        let before_tokens = before_state.estimated_tokens;
        let mut emitted_compaction_start = false;
        let mut report = ContextCompactReport {
            before_tokens,
            pressure_before: Some(before_state.pressure),
            ..ContextCompactReport::default()
        };

        let policy = self.context_manager.budget_policy().clone();
        if !policy.enabled {
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
            report.microcompact = self.context_manager.microcompact();
        }

        let after_micro_state = self.context_manager.budget_state();
        if after_micro_state.is_above_auto_compact_threshold && policy.full_compact_enabled {
            self.llm_session
                .emit_context_compaction_start("persistent_context", "full_compact");
            emitted_compaction_start = true;
            if self.context_manager.compact_consecutive_failures()
                >= policy.max_consecutive_failures
            {
                report.skipped_full_compact_due_to_fuse = true;
                tracing::warn!(
                    failures = self.context_manager.compact_consecutive_failures(),
                    "persistent context model compact skipped because failure fuse is open"
                );
            } else {
                let candidates = self.context_manager.full_compact_candidate_messages();
                if candidates.is_empty() {
                    self.context_manager.record_compact_failure();
                } else {
                    match self
                        .llm_session
                        .summarize_context_messages(
                            &candidates,
                            Some(plan_state),
                            policy.summary_max_tokens,
                        )
                        .await
                    {
                        Ok(summary) => {
                            match self.context_manager.apply_full_compact_summary(summary) {
                                Ok(full_report) => {
                                    self.context_manager.reset_compact_failures();
                                    report.full_compact = Some(full_report);
                                }
                                Err(err) => {
                                    self.context_manager.record_compact_failure();
                                    tracing::warn!(
                                        error = %err,
                                        failures = self.context_manager.compact_consecutive_failures(),
                                        "persistent context model compact summary could not be applied"
                                    );
                                }
                            }
                        }
                        Err(err) => {
                            self.context_manager.record_compact_failure();
                            tracing::warn!(
                                error = %err,
                                failures = self.context_manager.compact_consecutive_failures(),
                                "persistent context model compact failed; falling back to final trim"
                            );
                        }
                    }
                }
            }
        }

        let after_state = self.context_manager.budget_state();
        report.after_tokens = after_state.estimated_tokens;
        report.pressure_after = Some(after_state.pressure);
        let compaction_changed =
            report.microcompact.changed_messages > 0 || report.full_compact.is_some();
        if emitted_compaction_start || compaction_changed {
            let mode = if report.full_compact.is_some() {
                "full_compact"
            } else {
                "microcompact"
            };
            self.llm_session.emit_context_compaction_end(
                "persistent_context",
                mode,
                before_tokens,
                report.after_tokens,
                compaction_changed,
            );
        }
        report
    }

    /// Extract @skill_name references and inject skill prefix.
    /// Example: "@grep do this" → "use grep skill to do this.\n\ndo this"
    fn inject_skill_prefix(&self, text: &str) -> String {
        let manager = self.skill_manager.lock().unwrap_or_else(|e| e.into_inner());
        let available: Vec<String> = manager
            .list_skills()
            .iter()
            .map(|s| s.metadata.name.to_lowercase())
            .collect();
        drop(manager);

        if available.is_empty() {
            return text.to_string();
        }

        // Find all @word references
        let re = regex::Regex::new(r"@(\w+)").unwrap();
        let mut refs: Vec<String> = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for cap in re.captures_iter(text) {
            if let Some(name) = cap.get(1) {
                let name_lower = name.as_str().to_lowercase();
                if available.contains(&name_lower) && !seen.contains(&name_lower) {
                    refs.push(name_lower.clone());
                    seen.insert(name_lower);
                }
            }
        }

        if refs.is_empty() {
            return text.to_string();
        }

        let prefix: Vec<String> = refs
            .iter()
            .map(|name| format!("use {} skill to do this.", name))
            .collect();
        format!("{}\n\n{}", prefix.join(" "), text)
    }

    /// Recall relevant memories and inject them into the context as knowledge.
    fn recall_memories(&mut self, query: &str) {
        if !self.memory_config.auto_recall {
            return;
        }
        let mut guard = self.memory_manager.lock().unwrap();
        if let Some(ref mut mm) = *guard {
            let keywords = extract_keywords(query);
            let search_query = if keywords.is_empty() {
                query.to_string()
            } else {
                keywords.join(" ")
            };
            let results = mm.recall(&search_query, self.memory_config.recall_limit);
            if results.is_empty() {
                // No results — clear stale recall to keep context clean
                self.context_manager
                    .inject_knowledge_stable("memory_recall", "");
                return;
            }

            let text = results
                .iter()
                .map(|r| {
                    let cat = format_category(&r.category);
                    format!("- [{}] {}", cat, r.content)
                })
                .collect::<Vec<_>>()
                .join("\n");

            // Enforce token budget (~4 chars per token)
            let budget = self.memory_config.recall_token_budget * 4;
            let text = if text.len() > budget {
                let safe_end = floor_char_boundary(&text, budget);
                let truncated = &text[..safe_end];
                truncated
                    .rfind('\n')
                    .map(|i| text[..i].to_string())
                    .unwrap_or_else(|| truncated.to_string())
            } else {
                text
            };

            let content = format!(
                "<long-term-memory source=\"recall\">\n{}\n</long-term-memory>",
                text
            );
            // Stable injection — only replaces if content actually changed,
            // preserving cache prefix stability across calls.
            self.context_manager
                .inject_knowledge_stable("memory_recall", &content);
        }
    }

    /// Auto-retain user preferences and facts based on pattern matching.
    fn auto_retain_memory(&mut self, question: &str, _response: &str) {
        if !self.memory_config.auto_retain {
            return;
        }
        let mut guard = self.memory_manager.lock().unwrap();
        if let Some(ref mut mm) = *guard {
            let facts = extract_retainable_facts(question);
            for fact in facts {
                let category = categorize_fact(&fact);
                let _ = mm.store(&fact, category, "auto", 1.0);
            }
        }
    }

    /// Inject loaded skill descriptions into the context so the AI can use them.
    /// Uses stable injection — only updates when skills actually change.
    fn inject_skills(&mut self) {
        let content = {
            let manager = self.skill_manager.lock().unwrap_or_else(|e| e.into_inner());
            let skills = manager.list_skills();
            if skills.is_empty() {
                String::new()
            } else {
                let descriptions: Vec<String> = skills
                    .iter()
                    .map(|s| {
                        format!(
                            "## {}\n{}\nPath: {}",
                            s.metadata.name, s.metadata.description, s.file_path
                        )
                    })
                    .collect();
                format!(
                    "<available-skills>\n{}\n</available-skills>",
                    descriptions.join("\n\n")
                )
            }
        };

        self.context_manager
            .inject_knowledge_stable("skills", &content);
    }

    /// Build context messages from the context manager into ChatMessage format.
    fn build_context_messages(&self) -> Vec<ChatMessage> {
        self.context_manager
            .as_messages()
            .iter()
            .map(|v| {
                let role = v
                    .get("role")
                    .and_then(|r| r.as_str())
                    .unwrap_or("user")
                    .to_string();
                let content = v
                    .get("content")
                    .and_then(|c| c.as_str())
                    .map(|s| MessageContent::Text(s.to_string()));
                ChatMessage {
                    role,
                    content,
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                    reasoning_content: None,
                    cache_control: None,
                }
            })
            .collect()
    }

    /// Return the system message split into (static_core, env_block).
    /// The static core is stable across calls (enabling KV-cache prefix hits).
    /// The env_block contains per-call dynamic info (cwd).
    fn system_message_parts(&mut self) -> (String, String) {
        let core = self.prompt_manager.render_static_core(
            &uname_info(),
            &whoami(),
            &os_info(),
            &basic_env_info(),
            &output_language(),
        );
        let env = self.prompt_manager.render_env_block(&cwd());
        (core, env)
    }

    /// Return the system message for error correction mode.
    fn error_correction_system_message(
        &mut self,
        _command: &str,
        exit_code: i32,
        _stderr: &str,
    ) -> Option<String> {
        let role_prompt = self.prompt_manager.get("role").to_string();
        let mut vars = HashMap::new();
        vars.insert("role_prompt".to_string(), role_prompt);
        vars.insert("uname_info".to_string(), uname_info());
        vars.insert("user_nickname".to_string(), whoami());
        vars.insert("os_info".to_string(), os_info());
        vars.insert("basic_env_info".to_string(), basic_env_info());
        vars.insert("output_language".to_string(), output_language());
        vars.insert("exit_code".to_string(), exit_code.to_string());
        vars.insert("remote_env_info".to_string(), String::new());
        Some(self.prompt_manager.render("cmd_error", &vars))
    }
}

/// Confidence level in a failure diagnosis report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagnoseConfidence {
    High,
    Medium,
    Low,
}

/// Structured failure diagnosis report from the LLM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureDiagnoseReport {
    pub root_cause: String,
    pub evidence: Vec<String>,
    pub suggested_fix: Option<String>,
    pub verify_commands: Vec<String>,
    pub risk_notes: Option<String>,
    pub has_alternatives: bool,
    pub confidence: DiagnoseConfidence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyOutcome {
    Passed,
    Failed,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyStepResult {
    pub command: String,
    pub output: String,
    pub exit_code: i32,
    pub outcome: VerifyOutcome,
    pub block_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureDiagnoseConclusion {
    Fixed,
    PartialFailure,
    CannotDetermine,
}

/// How the LLM diagnose output was parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnoseParseOutcome {
    /// Structured JSON report.
    Structured,
    /// Plain-text fallback when the model did not return JSON.
    ProseFallback,
    /// JSON-like output that could not be parsed into a report.
    FormatError,
}

/// Parsed failure diagnosis report and parse metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureDiagnoseParseResult {
    pub report: FailureDiagnoseReport,
    pub outcome: DiagnoseParseOutcome,
}

/// Parse LLM output into a failure diagnosis report.
pub(crate) fn parse_diagnose_report_response(response: &str) -> FailureDiagnoseParseResult {
    if let Some(json) = extract_diagnose_report_json(response) {
        if let Some(report) = parse_diagnose_report_json(&json) {
            return FailureDiagnoseParseResult {
                report,
                outcome: DiagnoseParseOutcome::Structured,
            };
        }
    }

    let trimmed = response.trim();
    if looks_like_malformed_diagnose_report(trimmed) {
        return FailureDiagnoseParseResult {
            report: empty_diagnose_report(),
            outcome: DiagnoseParseOutcome::FormatError,
        };
    }

    FailureDiagnoseParseResult {
        report: FailureDiagnoseReport {
            root_cause: trimmed.to_string(),
            evidence: vec![],
            suggested_fix: None,
            verify_commands: vec![],
            risk_notes: None,
            has_alternatives: false,
            confidence: DiagnoseConfidence::Low,
        },
        outcome: DiagnoseParseOutcome::ProseFallback,
    }
}

fn empty_diagnose_report() -> FailureDiagnoseReport {
    FailureDiagnoseReport {
        root_cause: String::new(),
        evidence: vec![],
        suggested_fix: None,
        verify_commands: vec![],
        risk_notes: None,
        has_alternatives: false,
        confidence: DiagnoseConfidence::Low,
    }
}

fn looks_like_malformed_diagnose_report(text: &str) -> bool {
    let t = text.trim();
    t.starts_with('{')
        || t.starts_with("```")
        || t.contains("\"root_cause\"")
        || t.contains("\"type\"")
        || t.contains("\"diagnose_report\"")
}

fn extract_diagnose_report_json(response: &str) -> Option<serde_json::Value> {
    let code_block_re = regex::Regex::new(r"(?s)```(?:json)?\s*\n?(.*?)```").unwrap();
    for caps in code_block_re.captures_iter(response) {
        let content = caps.get(1)?.as_str().trim();
        if let Some(json) = diagnose_report_from_parsed_json(content) {
            return Some(json);
        }
    }

    if let Some(json) = extract_json_object_from_text(response) {
        if let Some(report) = diagnose_report_from_value(&json) {
            return Some(report);
        }
    }

    diagnose_report_from_parsed_json(response.trim())
}

fn diagnose_report_from_parsed_json(text: &str) -> Option<serde_json::Value> {
    let json = serde_json::from_str::<serde_json::Value>(text).ok()?;
    diagnose_report_from_value(&json)
}

fn diagnose_report_from_value(json: &serde_json::Value) -> Option<serde_json::Value> {
    if is_diagnose_report_candidate(json) {
        return Some(json.clone());
    }
    if let Some(answer) = json.get("answer").and_then(|v| v.as_str()) {
        return diagnose_report_from_parsed_json(answer.trim());
    }
    None
}

fn is_diagnose_report_candidate(json: &serde_json::Value) -> bool {
    let Some(obj) = json.as_object() else {
        return false;
    };
    match obj.get("type").and_then(|v| v.as_str()) {
        Some("diagnose_report") | None => {}
        Some(_) => return false,
    }
    obj.get("root_cause")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.trim().is_empty())
}

/// Brute-force scan: iterate every `{` × `}` combination and return the first
/// slice that parses as a valid JSON object. Used to recover JSON from prose-
/// wrapped or non-fenced model output.
pub(crate) fn extract_json_object_from_text(text: &str) -> Option<serde_json::Value> {
    for (start, _) in text.match_indices('{') {
        let slice = &text[start..];
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(slice) {
            return Some(value);
        }
        for (end, _) in slice.match_indices('}') {
            let candidate = &slice[..=end];
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(candidate) {
                return Some(value);
            }
        }
    }
    None
}

/// Whether `suggested_fix` is safe to offer as confirm-and-execute.
///
/// Must be a single pasteable shell command. Multi-option menus and instructional
/// prose are rejected so Workflow never feeds them to `execute_external_command`.
pub(crate) fn is_auto_executable_suggested_fix(cmd: &str) -> bool {
    static NUMBERED_ITEM: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(^|[\s；;。])\d+[\.\)、]\s").expect("numbered-item regex")
    });

    let cmd = cmd.trim();
    if cmd.is_empty() {
        return false;
    }
    if cmd.contains('\n') || cmd.contains('\r') {
        return false;
    }
    if NUMBERED_ITEM.is_match(cmd) {
        return false;
    }

    let lower = cmd.to_ascii_lowercase();
    const PROSE_MARKERS: &[&str] = &[
        "执行以下",
        "以下命令之一",
        "one of the following",
        "choose one of",
        "或者：",
        "或：",
    ];
    if PROSE_MARKERS
        .iter()
        .any(|m| cmd.contains(m) || lower.contains(m))
    {
        return false;
    }
    if (cmd.contains("临时") && cmd.contains("永久"))
        || (lower.contains("temporary") && lower.contains("permanent"))
    {
        return false;
    }
    true
}

/// Offer confirm-execute only for a unique, pasteable suggested fix.
pub(crate) fn should_offer_confirm_execute(
    suggested_fix: Option<&str>,
    has_alternatives: bool,
) -> bool {
    let Some(fix) = suggested_fix.map(str::trim).filter(|s| !s.is_empty()) else {
        return false;
    };
    if has_alternatives {
        return false;
    }
    is_auto_executable_suggested_fix(fix)
}

fn parse_diagnose_report_json(json: &serde_json::Value) -> Option<FailureDiagnoseReport> {
    if !is_diagnose_report_candidate(json) {
        return None;
    }
    let root_cause = json.get("root_cause")?.as_str()?.trim().to_string();
    if root_cause.is_empty() {
        return None;
    }
    let evidence: Vec<String> = json
        .get("evidence")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let suggested_fix = json
        .get("suggested_fix")
        .and_then(|v| {
            if v.is_null() {
                None
            } else {
                v.as_str().map(|s| s.trim().to_string())
            }
        })
        .filter(|s| !s.is_empty());
    let verify_commands: Vec<String> = json
        .get("verify_commands")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let risk_notes = json
        .get("risk_notes")
        .and_then(|v| {
            if v.is_null() {
                None
            } else {
                v.as_str().map(|s| s.trim().to_string())
            }
        })
        .filter(|s| !s.is_empty());
    let has_alternatives = json
        .get("has_alternatives")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let confidence = json
        .get("confidence")
        .and_then(|v| v.as_str())
        .map(parse_diagnose_confidence)
        .unwrap_or(DiagnoseConfidence::Medium);
    Some(FailureDiagnoseReport {
        root_cause,
        evidence,
        suggested_fix,
        verify_commands,
        risk_notes,
        has_alternatives,
        confidence,
    })
}

fn parse_diagnose_confidence(s: &str) -> DiagnoseConfidence {
    match s.to_ascii_lowercase().as_str() {
        "high" => DiagnoseConfidence::High,
        "low" => DiagnoseConfidence::Low,
        _ => DiagnoseConfidence::Medium,
    }
}

/// Map a verification command's exit code and output to pass/fail.
pub(crate) fn verify_outcome_from_execution(exit_code: i32, output: &str) -> VerifyOutcome {
    let effective = aish_pty::exit_code::infer_exit_code_from_output(exit_code, output);
    if effective == 0 {
        VerifyOutcome::Passed
    } else {
        VerifyOutcome::Failed
    }
}

pub(crate) fn effective_verify_exit_code(exit_code: i32, output: &str) -> i32 {
    aish_pty::exit_code::infer_exit_code_from_output(exit_code, output)
}

pub(crate) fn summarize_verification_conclusion(
    steps: &[VerifyStepResult],
) -> FailureDiagnoseConclusion {
    if steps.is_empty() {
        return FailureDiagnoseConclusion::CannotDetermine;
    }
    let mut any_ran = false;
    let mut any_failed = false;
    for step in steps {
        match step.outcome {
            VerifyOutcome::Passed => any_ran = true,
            VerifyOutcome::Failed => {
                any_ran = true;
                any_failed = true;
            }
            VerifyOutcome::Blocked => {}
        }
    }
    if !any_ran {
        FailureDiagnoseConclusion::CannotDetermine
    } else if any_failed {
        FailureDiagnoseConclusion::PartialFailure
    } else {
        FailureDiagnoseConclusion::Fixed
    }
}

/// Print a failure diagnosis report to stdout.
pub(crate) fn print_failure_diagnose_report(result: &FailureDiagnoseParseResult) {
    use aish_i18n::t;
    if result.outcome == DiagnoseParseOutcome::FormatError {
        println!(
            "{}",
            crate::theme::warning(&t("shell.failure_diagnose.parse_format_error"))
        );
        return;
    }
    let report = &result.report;
    println!("{}", t("shell.failure_diagnose.report_title"));
    if result.outcome == DiagnoseParseOutcome::ProseFallback {
        println!(
            "{}",
            crate::theme::warning(&t("shell.failure_diagnose.parse_prose_fallback"))
        );
    }
    println!(
        "{} {}",
        t("shell.failure_diagnose.root_cause"),
        report.root_cause
    );
    println!(
        "{} {}",
        t("shell.failure_diagnose.confidence"),
        match report.confidence {
            DiagnoseConfidence::High => t("shell.failure_diagnose.confidence_high"),
            DiagnoseConfidence::Medium => t("shell.failure_diagnose.confidence_medium"),
            DiagnoseConfidence::Low => t("shell.failure_diagnose.confidence_low"),
        }
    );
    println!("{}", t("shell.failure_diagnose.evidence"));
    if report.evidence.is_empty() {
        println!("  {}", t("shell.failure_diagnose.no_evidence"));
    } else {
        for (i, item) in report.evidence.iter().enumerate() {
            println!("  {}. {}", i + 1, item);
        }
    }
    if let Some(ref fix) = report.suggested_fix {
        println!("{} {}", t("shell.failure_diagnose.suggested_fix"), fix);
    } else {
        println!("{}", t("shell.failure_diagnose.no_suggested_fix"));
    }
    if !report.verify_commands.is_empty() {
        println!("{}", t("shell.failure_diagnose.verify_commands"));
        for cmd in &report.verify_commands {
            println!("  · {}", cmd);
        }
    }
    if let Some(ref notes) = report.risk_notes {
        println!("{} {}", t("shell.failure_diagnose.risk_notes"), notes);
    }
}

/// Format a model/API error from failure diagnosis for display.
pub(crate) fn format_failure_diagnose_error(error: &aish_core::AishError) -> String {
    use aish_i18n::t_with_args;
    let mut args = std::collections::HashMap::new();
    args.insert("error".to_string(), error.to_string());
    t_with_args("shell.failure_diagnose.error", &args)
}

/// Format a memory category for display.
fn format_category(cat: &MemoryCategory) -> &'static str {
    match cat {
        MemoryCategory::Preference => "Preference",
        MemoryCategory::Environment => "Environment",
        MemoryCategory::Solution => "Solution",
        MemoryCategory::Pattern => "Pattern",
        MemoryCategory::Other => "Other",
    }
}

/// Result of error correction analysis from the LLM.
pub struct ErrorCorrectionResult {
    /// The corrected command, if any.
    pub command: Option<String>,
    /// Description of the fix or why no fix is available.
    pub description: Option<String>,
}

/// Parse the LLM response for error correction, preferring JSON format.
/// Falls back to extracting a ```bash code block if JSON parsing fails.
pub(crate) fn parse_error_correction_response(response: &str) -> ErrorCorrectionResult {
    // Strategy: regex extracts the full content between ```...``` fences,
    // then serde_json handles actual JSON parsing. This avoids the fragility
    // of trying to match { brace boundaries } with regex (which breaks on
    // nested braces in string values like "use ${VAR}").
    let code_block_re =
        regex::Regex::new(r"(?s)```(?:json|bash|sh|shell|zsh)?\s*\n(.*?)\n```").unwrap();

    // Phase 1: Try each code block as JSON, then as a raw command.
    for caps in code_block_re.captures_iter(response) {
        let content = caps.get(1).unwrap().as_str().trim();

        // Try parsing as JSON
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(content) {
            if json.get("type").and_then(|v| v.as_str()) == Some("corrected_command") {
                let command = json
                    .get("command")
                    .and_then(|v| v.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
                let description = json
                    .get("description")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .filter(|s| !s.trim().is_empty());
                return ErrorCorrectionResult {
                    command,
                    description,
                };
            }
        }

        // If not JSON, treat as a raw command (first line only)
        if !content.is_empty() {
            let first_line = content.lines().next().unwrap_or(content).to_string();
            return ErrorCorrectionResult {
                command: Some(first_line),
                description: None,
            };
        }
    }

    // Phase 2: Try parsing the entire response as bare JSON (no fence).
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(response.trim()) {
        if json.get("type").and_then(|v| v.as_str()) == Some("corrected_command") {
            let command = json
                .get("command")
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            let description = json
                .get("description")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .filter(|s| !s.trim().is_empty());
            return ErrorCorrectionResult {
                command,
                description,
            };
        }
    }

    ErrorCorrectionResult {
        command: None,
        description: None,
    }
}

/// Get the current username.
pub(crate) fn whoami() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "user".to_string())
}

/// Get OS information string.
pub(crate) fn os_info() -> String {
    format!(
        "{} {} ({})",
        sysinfo::System::name().unwrap_or_default(),
        sysinfo::System::os_version().unwrap_or_default(),
        std::env::consts::ARCH
    )
}

/// Find the nearest valid UTF-8 char boundary at or before `i`.
/// Prevents panics from slicing multi-byte characters.
fn floor_char_boundary(s: &str, i: usize) -> usize {
    if i >= s.len() {
        s.len()
    } else {
        let mut j = i;
        while !s.is_char_boundary(j) {
            j -= 1;
        }
        j
    }
}

/// Get current working directory.
fn cwd() -> String {
    std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "/".to_string())
}

/// Get uname info (kernel version, architecture, etc.).
/// Result is cached for the process lifetime since it doesn't change.
pub(crate) fn uname_info() -> String {
    static CACHE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| {
            std::process::Command::new("uname")
                .arg("-a")
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_else(|| "unknown".to_string())
        })
        .clone()
}

/// Get basic environment info (package managers, etc.).
/// Result is cached for the process lifetime since it doesn't change.
pub(crate) fn basic_env_info() -> String {
    static CACHE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| {
            let mut parts = Vec::new();
            for (cmd, label) in [
                (["apt", "--version"].as_slice(), "APT"),
                (["dnf", "--version"].as_slice(), "DNF"),
                (["pacman", "--version"].as_slice(), "Pacman"),
                (["zypper", "--version"].as_slice(), "Zypper"),
            ] {
                if let Ok(output) = std::process::Command::new(cmd[0]).args(&cmd[1..]).output() {
                    if output.status.success() {
                        let ver = String::from_utf8_lossy(&output.stdout);
                        if let Some(line) = ver.lines().next() {
                            parts.push(format!("{}: {}", label, line));
                        }
                    }
                }
            }
            parts.join("\n")
        })
        .clone()
}

/// Process-wide override for the AI output language, sourced from
/// `config.output_language`. When `Some(non-empty)`, it takes precedence
/// over the LANG-derived default. Set once at startup.
static OUTPUT_LANG_OVERRIDE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();

/// Install a config-driven override for the AI response language.
/// Pass `None` to fall back to the locale-derived default.
pub(crate) fn set_output_language_override(lang: Option<String>) {
    let _ = OUTPUT_LANG_OVERRIDE.set(lang);
}

/// Map a locale/language code to a friendly name the model understands.
fn friendly_language_name(code: &str) -> String {
    let lower = code.to_lowercase();
    let primary = lower.split(['_', '-']).next().unwrap_or("");
    match primary {
        "zh" => "Chinese".to_string(),
        "ja" => "Japanese".to_string(),
        "ko" => "Korean".to_string(),
        "fr" => "French".to_string(),
        "de" => "German".to_string(),
        "es" => "Spanish".to_string(),
        "ru" => "Russian".to_string(),
        // English or anything unrecognized: return as-is (models handle
        // codes like "en-US", "pt-BR" fine).
        _ => code.to_string(),
    }
}

/// Get output language for AI responses.
/// Honors `config.output_language` when set; otherwise derives from `LANG`.
/// Result is cached for the process lifetime since it doesn't change.
pub(crate) fn output_language() -> String {
    static CACHE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| {
            if let Some(Some(lang)) = OUTPUT_LANG_OVERRIDE.get() {
                if !lang.is_empty() {
                    return friendly_language_name(lang);
                }
            }
            let locale = std::env::var("LANG").unwrap_or_else(|_| "en_US.UTF-8".to_string());
            let lang = locale.split('.').next().unwrap_or("en");
            friendly_language_name(lang)
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_json_code_block() {
        let response = r#"```json
{"type": "corrected_command", "command": "ls -la", "description": "Added -la flag"}
```"#;
        let result = parse_error_correction_response(response);
        assert_eq!(result.command, Some("ls -la".to_string()));
        assert_eq!(result.description, Some("Added -la flag".to_string()));
    }

    #[test]
    fn test_parse_multiline_json_code_block() {
        let response = r#"```json
{
  "type": "corrected_command",
  "command": "ls -la",
  "description": "Added -la flag for detailed listing"
}
```"#;
        let result = parse_error_correction_response(response);
        assert_eq!(result.command, Some("ls -la".to_string()));
        assert_eq!(
            result.description,
            Some("Added -la flag for detailed listing".to_string())
        );
    }

    #[test]
    fn test_parse_json_with_braces_in_value() {
        let response = r#"```json
{
  "type": "corrected_command",
  "command": "echo ${HOME}",
  "description": "Variable expansion fix"
}
```"#;
        let result = parse_error_correction_response(response);
        assert_eq!(result.command, Some("echo ${HOME}".to_string()));
        assert_eq!(
            result.description,
            Some("Variable expansion fix".to_string())
        );
    }

    #[test]
    fn test_parse_json_bare() {
        let response =
            r#"{"type": "corrected_command", "command": "ls -la", "description": "fix"}"#;
        let result = parse_error_correction_response(response);
        assert_eq!(result.command, Some("ls -la".to_string()));
    }

    #[test]
    fn test_parse_json_empty_command() {
        let response = r#"```json
{"type": "corrected_command", "command": "", "description": "No fix available"}
```"#;
        let result = parse_error_correction_response(response);
        assert_eq!(result.command, None);
        assert_eq!(result.description, Some("No fix available".to_string()));
    }

    #[test]
    fn test_parse_fallback_bash_block() {
        let response = "Here is the fix:\n```bash\nls -la\n```\nTry that.";
        let result = parse_error_correction_response(response);
        assert_eq!(result.command, Some("ls -la".to_string()));
        assert_eq!(result.description, None);
    }

    #[test]
    fn test_parse_fallback_zsh_block() {
        let response = "```zsh\necho hello\n```";
        let result = parse_error_correction_response(response);
        assert_eq!(result.command, Some("echo hello".to_string()));
    }

    #[test]
    fn test_parse_none() {
        let response = "I don't see a command to fix here.";
        let result = parse_error_correction_response(response);
        assert_eq!(result.command, None);
        assert_eq!(result.description, None);
    }

    #[test]
    fn test_categorize_preference() {
        assert!(matches!(
            categorize_fact("user prefers dark theme"),
            MemoryCategory::Preference
        ));
        assert!(matches!(
            categorize_fact("I always use vim"),
            MemoryCategory::Preference
        ));
    }

    #[test]
    fn test_categorize_environment() {
        assert!(matches!(
            categorize_fact("database port is 5432"),
            MemoryCategory::Environment
        ));
        assert!(matches!(
            categorize_fact("API endpoint at /v1/chat"),
            MemoryCategory::Environment
        ));
    }

    #[test]
    fn test_categorize_solution() {
        assert!(matches!(
            categorize_fact("fixed the connection error by restarting"),
            MemoryCategory::Solution
        ));
    }

    #[test]
    fn test_categorize_pattern() {
        assert!(matches!(
            categorize_fact("follow the convention of using snake_case"),
            MemoryCategory::Pattern
        ));
    }

    #[test]
    fn test_categorize_other() {
        assert!(matches!(
            categorize_fact("the weather is nice today"),
            MemoryCategory::Other
        ));
    }

    #[test]
    fn test_extract_keywords_basic() {
        let keywords = extract_keywords("How do I configure the database connection?");
        assert!(!keywords.is_empty());
        assert!(keywords.contains(&"configure".to_string()));
        assert!(keywords.contains(&"database".to_string()));
        assert!(keywords.contains(&"connection".to_string()));
        assert!(!keywords.contains(&"how".to_string()));
        assert!(!keywords.contains(&"do".to_string()));
    }

    #[test]
    fn test_extract_keywords_short() {
        let keywords = extract_keywords("ls");
        // Single char words (len < 2) are filtered out
        assert!(keywords.is_empty() || keywords.contains(&"ls".to_string()));
    }

    #[test]
    fn test_extract_keywords_dedup() {
        let keywords = extract_keywords("test test test");
        // Should deduplicate
        let count = keywords.iter().filter(|k| *k == "test").count();
        assert!(count <= 1);
    }

    #[test]
    fn test_extract_retainable_facts_preference() {
        let facts = extract_retainable_facts("I prefer dark mode for all editors");
        assert!(!facts.is_empty());
        assert!(facts[0].contains("dark mode"));
    }

    #[test]
    fn test_extract_retainable_facts_remember() {
        let facts = extract_retainable_facts("Please remember that the API key expires in June");
        assert!(!facts.is_empty());
        assert!(facts[0].contains("API key"));
    }

    #[test]
    fn test_extract_retainable_facts_none() {
        let facts = extract_retainable_facts("What is the weather today?");
        assert!(facts.is_empty());
    }

    #[test]
    fn test_extract_retainable_facts_environment() {
        let facts = extract_retainable_facts("the database port is 5432");
        assert!(!facts.is_empty());
    }

    #[test]
    fn test_parse_diagnose_report_json() {
        let response = r#"```json
{
  "type": "diagnose_report",
  "root_cause": "command not found",
  "evidence": ["which foo returned empty"],
  "suggested_fix": "sudo apt install foo",
  "verify_commands": ["which foo"],
  "confidence": "high"
}
```"#;
        let parsed = parse_diagnose_report_response(response);
        assert_eq!(parsed.outcome, DiagnoseParseOutcome::Structured);
        assert_eq!(parsed.report.root_cause, "command not found");
        assert_eq!(parsed.report.evidence.len(), 1);
        assert_eq!(
            parsed.report.suggested_fix.as_deref(),
            Some("sudo apt install foo")
        );
        assert_eq!(parsed.report.confidence, DiagnoseConfidence::High);
    }

    #[test]
    fn test_parse_diagnose_report_empty_evidence_accepted() {
        let response = r#"{"type":"diagnose_report","root_cause":"x","evidence":[]}"#;
        let parsed = parse_diagnose_report_response(response);
        assert_eq!(parsed.outcome, DiagnoseParseOutcome::Structured);
        assert_eq!(parsed.report.root_cause, "x");
        assert!(parsed.report.evidence.is_empty());
    }

    #[test]
    fn test_parse_diagnose_report_without_type_field() {
        let response = r#"{"root_cause":"service inactive","evidence":["systemctl status"]}"#;
        let parsed = parse_diagnose_report_response(response);
        assert_eq!(parsed.outcome, DiagnoseParseOutcome::Structured);
        assert_eq!(parsed.report.root_cause, "service inactive");
    }

    #[test]
    fn test_parse_diagnose_report_from_answer_wrapper() {
        let response =
            r#"{"answer":"{\"root_cause\":\"missing binary\",\"evidence\":[\"which missing\"]}"}"#;
        let parsed = parse_diagnose_report_response(response);
        assert_eq!(parsed.outcome, DiagnoseParseOutcome::Structured);
        assert_eq!(parsed.report.root_cause, "missing binary");
    }

    #[test]
    fn test_parse_diagnose_report_malformed_json_is_format_error() {
        let response = r#"{"type":"diagnose_report","root_cause":"","evidence":["x"]}"#;
        let parsed = parse_diagnose_report_response(response);
        assert_eq!(parsed.outcome, DiagnoseParseOutcome::FormatError);
    }

    #[test]
    fn test_parse_diagnose_report_prose_fallback() {
        let response = "The command failed because foo is not installed.";
        let parsed = parse_diagnose_report_response(response);
        assert_eq!(parsed.outcome, DiagnoseParseOutcome::ProseFallback);
        assert_eq!(parsed.report.root_cause, response);
    }

    #[test]
    fn test_is_auto_executable_suggested_fix_accepts_single_command() {
        assert!(is_auto_executable_suggested_fix("sudo apt install foo"));
        assert!(is_auto_executable_suggested_fix(
            "sed -i \"s/#alias ll=/alias ll=/\" ~/.bashrc && source ~/.bashrc"
        ));
        assert!(is_auto_executable_suggested_fix("alias ll='ls -l'"));
    }

    #[test]
    fn test_is_auto_executable_suggested_fix_rejects_menus_and_prose() {
        assert!(!is_auto_executable_suggested_fix(""));
        assert!(!is_auto_executable_suggested_fix("line1\nline2"));
        assert!(!is_auto_executable_suggested_fix(
            "执行以下命令之一：1. 临时启用 alias ll='ls -l' 2. 永久取消注释 ~/.bashrc"
        ));
        assert!(!is_auto_executable_suggested_fix(
            "1. 临时: alias ll='ls -l'；2. 永久: edit bashrc"
        ));
        assert!(!is_auto_executable_suggested_fix(
            "Choose one of: apt install foo or yum install foo"
        ));
        assert!(!is_auto_executable_suggested_fix(
            "Temporary: alias ll='ls -l'; permanent: uncomment in bashrc"
        ));
    }

    #[test]
    fn test_should_offer_confirm_execute_skips_when_has_alternatives() {
        let fix = "sed -i \"s/#alias ll='ls -l'/alias ll='ls -l'/\" ~/.bashrc && source ~/.bashrc";
        assert!(is_auto_executable_suggested_fix(fix));
        assert!(!should_offer_confirm_execute(Some(fix), true));
        assert!(should_offer_confirm_execute(
            Some("sudo apt install foo"),
            false
        ));
        assert!(!should_offer_confirm_execute(None, false));
    }

    #[test]
    fn test_parse_diagnose_report_has_alternatives() {
        let response = r#"{"root_cause":"x","evidence":["y"],"has_alternatives":true}"#;
        let parsed = parse_diagnose_report_response(response);
        assert!(parsed.report.has_alternatives);
    }

    #[test]
    fn test_readonly_command_guard() {
        use aish_tools::bash::{BashTool, ReadOnlyVerdict};

        assert!(BashTool::is_read_only("systemctl status nginx"));
        assert_eq!(
            BashTool::classify_read_only("systemctl status nginx"),
            ReadOnlyVerdict::ReadOnly
        );
        assert!(BashTool::is_read_only("which foo"));
        assert!(matches!(
            BashTool::classify_read_only("systemctl restart nginx"),
            ReadOnlyVerdict::NotReadOnly { .. }
        ));
        assert!(matches!(
            BashTool::classify_read_only("rm -rf /"),
            ReadOnlyVerdict::NotReadOnly { .. }
        ));
    }

    #[test]
    fn test_verify_outcome_infers_failure_from_output() {
        let out = "Failed to restart foo.service: Unit foo.service not found.\n";
        assert_eq!(verify_outcome_from_execution(0, out), VerifyOutcome::Failed);
        assert_eq!(effective_verify_exit_code(0, out), 1);
        assert_eq!(
            verify_outcome_from_execution(0, "active (running)\n"),
            VerifyOutcome::Passed
        );
    }

    #[test]
    fn test_summarize_verification_conclusion() {
        use VerifyOutcome::*;
        let fixed = summarize_verification_conclusion(&[VerifyStepResult {
            command: "true".into(),
            output: String::new(),
            exit_code: 0,
            outcome: Passed,
            block_reason: None,
        }]);
        assert_eq!(fixed, FailureDiagnoseConclusion::Fixed);

        let partial = summarize_verification_conclusion(&[VerifyStepResult {
            command: "false".into(),
            output: String::new(),
            exit_code: 1,
            outcome: Failed,
            block_reason: None,
        }]);
        assert_eq!(partial, FailureDiagnoseConclusion::PartialFailure);

        let unknown = summarize_verification_conclusion(&[VerifyStepResult {
            command: "rm x".into(),
            output: String::new(),
            exit_code: -1,
            outcome: Blocked,
            block_reason: Some("blocked".into()),
        }]);
        assert_eq!(unknown, FailureDiagnoseConclusion::CannotDetermine);
    }
}
