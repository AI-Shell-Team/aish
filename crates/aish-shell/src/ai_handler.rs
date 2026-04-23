use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use aish_config::MemoryConfig;
use aish_context::ContextManager;
use aish_core::{LlmEvent, MemoryCategory, MemoryType, PlanModeState, PlanPhase};
use aish_llm::{ChatMessage, LlmCallbackResult, LlmSession};
use aish_memory::MemoryManager;
use aish_prompts::PromptManager;
use aish_skills::SkillManager;

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
    skill_manager: SkillManager,
    memory_config: MemoryConfig,
    prompt_manager: PromptManager,
    token_store: crate::token_store::TokenUsageStore,
}

impl AiHandler {
    pub fn new(
        llm_session: LlmSession,
        memory_manager: SharedMemoryManager,
        skill_manager: SkillManager,
        memory_config: MemoryConfig,
        max_llm_messages: usize,
        max_shell_messages: usize,
        token_budget: Option<usize>,
    ) -> Self {
        let mut context_manager = ContextManager::with_limits(
            max_llm_messages,
            max_shell_messages,
            10, // max_knowledge_messages
        );
        context_manager.set_token_budget(token_budget);
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

    /// Add a shell command result to the LLM context so the AI can reference
    /// previous command output in follow-up questions.
    pub fn add_shell_context(&mut self, entry: &str) {
        self.context_manager
            .add_message("user", entry, MemoryType::Shell);
        self.context_manager.trim();
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

    /// Return a snapshot of token usage statistics for the last 7 days.
    pub fn token_stats(&self) -> aish_llm::TokenStats {
        self.token_store.stats()
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

    /// Run a diagnostic agent to investigate a command failure.
    /// Returns a diagnostic summary string if successful.
    pub async fn diagnose_failure(
        &self,
        command: &str,
        exit_code: i32,
        output: &str,
    ) -> aish_core::Result<String> {
        use aish_llm::diagnose_agent::DiagnoseAgent;

        let query = format!(
            "Command failed: '{}'\nExit code: {}\nOutput:\n{}\n\n\
             Please investigate and diagnose why this command failed.",
            command,
            exit_code,
            if output.len() > 4096 {
                &output[..output.floor_char_boundary(4096)]
            } else {
                output
            }
        );

        let agent = DiagnoseAgent::new();
        // Create minimal tool set for diagnosis
        let tools: Vec<Box<dyn aish_llm::Tool>> = vec![
            Box::new(aish_tools::SecureBashTool::new()),
            Box::new(aish_tools::fs::ReadFileTool::new()),
        ];

        agent.diagnose(&self.llm_session, &query, tools).await
    }

    /// Handle an AI question: send to LLM and return the response text.
    pub async fn handle_question(&mut self, question: &str) -> aish_core::Result<String> {
        // Step 1: Process @skill references in the question
        let question_processed = self.inject_skill_prefix(question);

        // Step 2: Auto-recall relevant memories into context
        self.recall_memories(&question_processed);

        // Step 3: Inject loaded skills into context as knowledge
        self.inject_skills();

        // Step 4: Build context and system messages
        let context_messages = self.build_context_messages();
        let system_message = self.system_message();

        // Step 5: Send to LLM
        let response = self
            .llm_session
            .process_input(
                &question_processed,
                &context_messages,
                system_message.as_deref(),
                true,
            )
            .await?;

        // Step 6: Store the exchange in context
        self.context_manager
            .add_message("user", &question_processed, MemoryType::Llm);
        self.context_manager
            .add_message("assistant", &response, MemoryType::Llm);
        self.context_manager.trim();

        // Step 7: Auto-retain user preferences/facts
        self.auto_retain_memory(&question_processed, &response);

        // Step 8: Persist token usage delta to disk
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

        let response = self
            .llm_session
            .process_input(&prompt, &context_messages, system_message.as_deref(), true)
            .await?;

        // Persist token usage delta to disk
        self.persist_token_usage();

        Ok(parse_error_correction_response(&response))
    }

    /// Extract @skill_name references and inject skill prefix.
    /// Example: "@grep do this" → "use grep skill to do this.\n\ndo this"
    fn inject_skill_prefix(&self, text: &str) -> String {
        let available: Vec<String> = self
            .skill_manager
            .list_skills()
            .iter()
            .map(|s| s.metadata.name.to_lowercase())
            .collect();

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
            // Clear previous recall
            self.context_manager.clear_knowledge("memory_recall");

            let keywords = extract_keywords(query);
            let search_query = if keywords.is_empty() {
                query.to_string()
            } else {
                keywords.join(" ")
            };
            let results = mm.recall(&search_query, self.memory_config.recall_limit);
            if results.is_empty() {
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
                // Find the nearest char boundary at or before budget
                let safe_end = floor_char_boundary(&text, budget);
                let truncated = &text[..safe_end];
                format!(
                    "{}\n</long-term-memory>",
                    truncated
                        .rfind('\n')
                        .map(|i| &text[..i])
                        .unwrap_or(truncated)
                )
            } else {
                text
            };

            self.context_manager.add_message(
                "system",
                &format!(
                    "<long-term-memory source=\"recall\">\n{}\n</long-term-memory>",
                    text
                ),
                MemoryType::Knowledge,
            );
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
    fn inject_skills(&mut self) {
        self.context_manager.clear_knowledge("skills");

        let skills = self.skill_manager.list_skills();
        if skills.is_empty() {
            return;
        }

        let descriptions: Vec<String> = skills
            .iter()
            .map(|s| {
                format!(
                    "## {}\n{}\nPath: {}",
                    s.metadata.name, s.metadata.description, s.file_path
                )
            })
            .collect();
        let text = descriptions.join("\n\n");

        self.context_manager.add_message(
            "system",
            &format!("<available-skills>\n{}\n</available-skills>", text),
            MemoryType::Knowledge,
        );
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
                    .map(|s| s.to_string());
                ChatMessage {
                    role,
                    content,
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                }
            })
            .collect()
    }

    /// Return the system message for normal AI interactions.
    fn system_message(&mut self) -> Option<String> {
        let role_prompt = self.prompt_manager.get("role").to_string();
        let mut vars = HashMap::new();
        vars.insert("role_prompt".to_string(), role_prompt);
        vars.insert("username".to_string(), whoami());
        vars.insert("hostname".to_string(), hostname());
        vars.insert("os_info".to_string(), os_info());
        vars.insert("cwd".to_string(), cwd());
        vars.insert("system_info".to_string(), String::new());
        vars.insert("memory_context".to_string(), String::new());
        vars.insert("skill_list".to_string(), String::new());
        Some(self.prompt_manager.render("oracle", &vars))
    }

    /// Return the system message for error correction mode.
    fn error_correction_system_message(
        &mut self,
        command: &str,
        exit_code: i32,
        stderr: &str,
    ) -> Option<String> {
        let stderr_section = if stderr.is_empty() {
            String::new()
        } else {
            let preview = if stderr.len() > 2048 {
                &stderr[..floor_char_boundary(stderr, 2048)]
            } else {
                stderr
            };
            format!("\n**Command Output:**\n```\n{}\n```", preview)
        };
        let mut vars = HashMap::new();
        vars.insert("username".to_string(), whoami());
        vars.insert("os_info".to_string(), os_info());
        vars.insert("command".to_string(), command.to_string());
        vars.insert("exit_code".to_string(), exit_code.to_string());
        vars.insert("stderr_section".to_string(), stderr_section);
        Some(self.prompt_manager.render("cmd_error", &vars))
    }
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
fn parse_error_correction_response(response: &str) -> ErrorCorrectionResult {
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
fn whoami() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "user".to_string())
}

/// Get the hostname.
fn hostname() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| {
            std::process::Command::new("hostname")
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .map_err(|_| "unknown".to_string())
        })
        .unwrap_or_else(|_| "unknown".to_string())
}

/// Get OS information string.
fn os_info() -> String {
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
}
