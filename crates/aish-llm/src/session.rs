use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use aish_context::{ContextBudgetPolicy, ContextMessage, ContextPressureLevel, MicrocompactReport};
use aish_core::{
    AishError, AuditEvent, AuditSink, LlmEvent, LlmEventType, MemoryType, PlanModeState, PlanPhase,
};

use crate::api::{resolve_api_dialect, stream_simple, ApiDialect, StreamContext};
use crate::client::LlmResponse;
use crate::langfuse::LangfuseClient;
use crate::streaming::{extract_message_text, SseEvent, StreamParser};
use crate::types::*;

fn is_short_circuit_result(result: &ToolResult) -> bool {
    result
        .meta
        .as_ref()
        .and_then(|meta| meta.get("dispatch_status"))
        .and_then(|value| value.as_str())
        .is_some_and(|status| status.eq_ignore_ascii_case("short_circuit"))
}

fn audit_security_info(security: &PreflightSecurityContext) -> (Option<String>, Option<String>) {
    let Some(ref decision) = security.decision else {
        return (None, None);
    };
    let rule = decision
        .analysis
        .matched_rule
        .as_ref()
        .and_then(|r| r.id.clone().or(r.name.clone()));
    let level = match decision.level {
        aish_security::RiskLevel::Low => "LOW",
        aish_security::RiskLevel::Medium => "MEDIUM",
        aish_security::RiskLevel::High => "HIGH",
    };
    (rule, Some(level.to_string()))
}

/// Maximum consecutive tool failures before pausing for user confirmation.
const MAX_CONSECUTIVE_FAILURES: usize = 3;

const COMPACT_SUMMARY_SYSTEM_PROMPT: &str = "You summarize old AI Shell context for a shell operations assistant. Respond with TEXT ONLY. Do not call tools. Preserve operational facts, commands, failures, important stderr, environment constraints, plan state, and pending user intent. Output a concise <conversation-summary> block with stable section headings.";

/// Main LLM session that orchestrates the chat loop with tool calling.
pub struct LlmSession {
    stream_ctx: StreamContext,
    api_dialect: ApiDialect,
    tools: HashMap<String, Arc<dyn Tool>>,
    cancellation_token: Arc<CancellationToken>,
    event_callback: Option<Arc<dyn Fn(LlmEvent) -> Option<LlmCallbackResult> + Send + Sync>>,
    confirmation_callback: Option<Arc<dyn Fn(&PreflightSecurityContext) -> bool + Send + Sync>>,
    security_notice_callback: Option<Arc<dyn Fn(&PreflightSecurityContext) + Send + Sync>>,
    /// Callback invoked when the tool-call iteration limit is reached.
    /// Receives the current iteration count and returns true to reset and continue.
    iteration_limit_callback: Option<Arc<dyn Fn(u32) -> bool + Send + Sync>>,
    audit_sink: Option<Arc<dyn AuditSink>>,
    audit_redactor: Option<Arc<dyn Fn(&str) -> String + Send + Sync>>,
    audit_session_uuid: Option<String>,
    audit_user: Option<String>,
    /// Dynamic host reference — updated by the PTY forwarding loop when the
    /// user enters or exits nested SSH sessions.  Stored as a shared mutex so
    /// that audit events always reflect the *current* remote host, not a
    /// snapshot taken at session creation time.
    audit_host: Option<Arc<Mutex<Option<String>>>>,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    langfuse: Option<LangfuseClient>,
    /// Stable Langfuse trace ID for the entire session (created once, reused across turns).
    langfuse_session_id: std::sync::Mutex<Option<String>>,
    /// Monotonic turn counter for naming spans within the session trace.
    langfuse_turn_counter: std::sync::atomic::AtomicU32,
    /// Maximum context token budget. Messages are trimmed when exceeded.
    max_context_tokens: usize,
    context_budget_policy: ContextBudgetPolicy,
    compact_consecutive_failures: std::sync::Mutex<usize>,
    /// Plan mode state for dynamic tool filtering.
    plan_state: Arc<Mutex<PlanModeState>>,
    /// Cumulative token usage statistics for this session.
    token_stats: std::sync::Mutex<crate::usage::TokenStats>,
    /// Locally estimated prompt tokens from the last API call's message list.
    /// Updated every iteration — reflects actual context window consumption
    /// regardless of whether the API reports prompt_tokens in streaming mode.
    last_prompt_estimate: std::sync::atomic::AtomicU64,
    /// Per-session tool execution policy (read-only bash enforcement, etc.).
    tool_execution_policy: crate::tool_context::ToolExecutionPolicy,
    /// True for sessions created via [`Self::create_subsession`].
    is_sub_agent: bool,
    /// Scripted chat completion responses for unit/integration tests (pop in order).
    #[cfg(test)]
    test_chat_responses: Option<Arc<std::sync::Mutex<Vec<Result<LlmResponse, AishError>>>>>,
}

impl LlmSession {
    pub fn new(
        api_base: &str,
        api_key: &str,
        model: &str,
        temperature: Option<f32>,
        max_tokens: Option<u32>,
    ) -> Self {
        Self::with_context(
            StreamContext::new(api_base, api_key, model, None),
            temperature,
            max_tokens,
        )
    }

    pub fn with_context(
        stream_ctx: StreamContext,
        temperature: Option<f32>,
        max_tokens: Option<u32>,
    ) -> Self {
        let api_dialect = resolve_api_dialect(
            &stream_ctx.config_model,
            &stream_ctx.api_base,
            &stream_ctx.api_key,
        );
        Self {
            stream_ctx,
            api_dialect,
            tools: HashMap::with_capacity(16),
            cancellation_token: Arc::new(CancellationToken::new()),
            event_callback: None,
            confirmation_callback: None,
            security_notice_callback: None,
            iteration_limit_callback: None,
            audit_sink: None,
            audit_redactor: None,
            audit_session_uuid: None,
            audit_user: None,
            audit_host: None,
            temperature,
            max_tokens,
            langfuse: None,
            langfuse_session_id: std::sync::Mutex::new(None),
            langfuse_turn_counter: std::sync::atomic::AtomicU32::new(0),
            max_context_tokens: 100_000,
            context_budget_policy: ContextBudgetPolicy::default(),
            compact_consecutive_failures: std::sync::Mutex::new(0),
            plan_state: Arc::new(Mutex::new(PlanModeState::default())),
            token_stats: std::sync::Mutex::new(crate::usage::TokenStats::default()),
            last_prompt_estimate: std::sync::atomic::AtomicU64::new(0),
            tool_execution_policy: crate::tool_context::ToolExecutionPolicy::default(),
            is_sub_agent: false,
            #[cfg(test)]
            test_chat_responses: None,
        }
    }

    pub fn tool_execution_policy(&self) -> crate::tool_context::ToolExecutionPolicy {
        self.tool_execution_policy
    }

    pub fn set_tool_execution_policy(&mut self, policy: crate::tool_context::ToolExecutionPolicy) {
        self.tool_execution_policy = policy;
    }

    /// Whether this session is an isolated sub-agent loop (not the main shell chat).
    pub fn is_sub_agent(&self) -> bool {
        self.is_sub_agent
    }

    pub fn register_tool(&mut self, tool: Box<dyn Tool>) {
        self.register_shared_tool(Arc::from(tool));
    }

    /// Register a shared tool handle (used when inheriting parent tools into a sub-session).
    pub fn register_shared_tool(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    /// Return shared handles for tools whose names appear in `names` (order preserved).
    pub fn shared_tools_by_names<'a, I>(&self, names: I) -> Vec<Arc<dyn Tool>>
    where
        I: IntoIterator<Item = &'a str>,
    {
        names
            .into_iter()
            .filter_map(|name| self.tools.get(name).cloned())
            .collect()
    }

    pub fn set_event_callback(
        &mut self,
        cb: Arc<dyn Fn(LlmEvent) -> Option<LlmCallbackResult> + Send + Sync>,
    ) {
        self.event_callback = Some(cb);
    }

    /// Clone the current event callback, if any.
    pub fn event_callback_arc(
        &self,
    ) -> Option<Arc<dyn Fn(LlmEvent) -> Option<LlmCallbackResult> + Send + Sync>> {
        self.event_callback.clone()
    }

    /// Set the confirmation callback invoked when a tool's preflight returns Confirm.
    /// The callback receives raw security context and returns true to approve.
    pub fn set_confirmation_callback(
        &mut self,
        cb: Arc<dyn Fn(&PreflightSecurityContext) -> bool + Send + Sync>,
    ) {
        self.confirmation_callback = Some(cb);
    }

    /// Set the callback invoked when a tool's preflight returns a display-only security notice.
    pub fn set_security_notice_callback(
        &mut self,
        cb: Arc<dyn Fn(&PreflightSecurityContext) + Send + Sync>,
    ) {
        self.security_notice_callback = Some(cb);
    }

    /// Set the callback invoked when the tool-call iteration limit is reached.
    /// The callback receives the current iteration count and returns true to
    /// reset the counter and continue, or false to stop.
    pub fn set_iteration_limit_callback(&mut self, cb: Arc<dyn Fn(u32) -> bool + Send + Sync>) {
        self.iteration_limit_callback = Some(cb);
    }

    /// Wire up the audit sink and identity context for tool-call / security
    /// audit instrumentation.  When `sink` is set, [`execute_tool`] and
    /// [`run_tool_preflight`] emit audit events.
    ///
    /// `host` is a shared mutex so nested SSH host changes (detected by the
    /// PTY forwarding loop) are reflected in every audit event without
    /// requiring a new `set_audit_context` call.
    pub fn set_audit_context(
        &mut self,
        sink: Arc<dyn AuditSink>,
        redactor: Option<Arc<dyn Fn(&str) -> String + Send + Sync>>,
        session_uuid: String,
        user: Option<String>,
        host: Option<Arc<Mutex<Option<String>>>>,
    ) {
        self.audit_sink = Some(sink);
        self.audit_redactor = redactor;
        self.audit_session_uuid = Some(session_uuid);
        self.audit_user = user;
        self.audit_host = host;
    }

    fn redact(&self, text: &str) -> String {
        match self.audit_redactor {
            Some(ref r) => r(text),
            None => text.to_string(),
        }
    }

    /// Resolve the current (user, host) pair from the shared host mutex.
    /// The mutex value uses `user@host` format (from SSH command parsing);
    /// we split it into separate fields.  Falls back to `audit_user` when
    /// the mutex is absent or has no user@ prefix.
    fn current_audit_identity(&self) -> (Option<String>, Option<String>) {
        let raw = self
            .audit_host
            .as_ref()
            .and_then(|h| h.lock().unwrap_or_else(|e| e.into_inner()).clone())
            .filter(|s| !s.is_empty());

        match &raw {
            Some(val) => {
                let (user, host) = val
                    .split_once('@')
                    .map(|(u, h)| (Some(u.to_string()), h.to_string()))
                    .unwrap_or((None, val.clone()));
                (user.or_else(|| self.audit_user.clone()), Some(host))
            }
            None => (self.audit_user.clone(), None),
        }
    }

    fn emit_audit(&self, mut event: AuditEvent) {
        if let Some(ref sink) = self.audit_sink {
            let (user, host) = self.current_audit_identity();
            if user.is_some() {
                event.user = user;
            }
            if host.is_some() {
                event.host = host;
            }
            sink.record(event);
        }
    }

    pub fn cancellation_token(&self) -> &CancellationToken {
        &self.cancellation_token
    }

    /// Return a shared reference to the cancellation token, allowing tools
    /// and other components to monitor cancellation without borrowing self.
    pub fn cancellation_token_arc(&self) -> Arc<CancellationToken> {
        Arc::clone(&self.cancellation_token)
    }

    /// Install scripted LLM responses for tests (consumed in FIFO order).
    #[cfg(test)]
    pub fn set_test_chat_responses(&mut self, responses: Vec<Result<LlmResponse, AishError>>) {
        self.test_chat_responses = Some(Arc::new(std::sync::Mutex::new(responses)));
    }

    /// Set the maximum context token budget for message trimming.
    pub fn set_max_context_tokens(&mut self, max: usize) {
        self.max_context_tokens = max;
    }

    pub fn set_context_budget_policy(&mut self, policy: ContextBudgetPolicy) {
        self.max_context_tokens = policy.effective_context_window();
        self.context_budget_policy = policy;
    }

    /// Set an optional Langfuse client for observability tracing.
    pub fn set_langfuse(&mut self, client: LangfuseClient) {
        self.langfuse = Some(client);
    }

    /// Return tool specs for all registered tools.
    pub fn tool_specs(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|t| t.to_spec()).collect()
    }

    /// Iterate registered tools (for prompt assembly appendix).
    pub(crate) fn registered_tools(&self) -> impl Iterator<Item = &dyn Tool> {
        self.tools.values().map(|tool| tool.as_ref())
    }

    /// Get a reference to the plan state (for external coordination).
    pub fn plan_state(&self) -> Arc<Mutex<PlanModeState>> {
        Arc::clone(&self.plan_state)
    }

    /// Return the locally estimated prompt token count from the last API call.
    pub fn last_prompt_estimate(&self) -> u64 {
        self.last_prompt_estimate
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Return a snapshot of cumulative token usage statistics.
    pub fn token_stats(&self) -> crate::usage::TokenStats {
        self.token_stats.lock().unwrap().clone()
    }

    /// Record token usage from an API response.
    pub(crate) fn record_usage_public(&self, usage: crate::usage::TokenUsage) {
        self.record_usage(usage);
    }

    pub(crate) fn loop_temperature(&self) -> Option<f32> {
        self.temperature
    }

    pub(crate) fn loop_max_tokens(&self) -> Option<u32> {
        self.max_tokens
    }

    fn record_usage(&self, usage: crate::usage::TokenUsage) {
        self.token_stats.lock().unwrap().record(usage);
    }

    /// Update the model, optionally also updating API base and key.
    pub fn update_model(&mut self, model: &str, api_base: Option<&str>, api_key: Option<&str>) {
        self.stream_ctx.refresh_dialect(model, api_base, api_key);
        self.api_dialect = resolve_api_dialect(
            &self.stream_ctx.config_model,
            &self.stream_ctx.api_base,
            &self.stream_ctx.api_key,
        );
    }

    /// Return the current model name.
    pub fn model_name(&self) -> &str {
        &self.stream_ctx.model
    }

    /// Resolved model name sent to the API (prefixes stripped when applicable).
    pub fn resolved_model_name(&self) -> String {
        self.stream_ctx.resolved_model()
    }

    pub fn api_dialect(&self) -> ApiDialect {
        self.api_dialect
    }

    async fn chat_completion(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[ToolSpec]>,
        stream: bool,
        temperature: Option<f32>,
        max_tokens: Option<u32>,
    ) -> Result<LlmResponse, AishError> {
        #[cfg(test)]
        if let Some(ref queue) = self.test_chat_responses {
            let mut q = queue.lock().expect("test chat response queue poisoned");
            if !q.is_empty() {
                return q.remove(0);
            }
        }

        stream_simple(
            self.api_dialect,
            &self.stream_ctx,
            messages,
            tools,
            stream,
            temperature,
            max_tokens,
        )
        .await
    }

    /// Low-level chat completion returning the raw API response.
    pub async fn chat_completion_raw(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[ToolSpec]>,
        stream: bool,
        temperature: Option<f32>,
        max_tokens: Option<u32>,
    ) -> Result<LlmResponse, AishError> {
        self.chat_completion(messages, tools, stream, temperature, max_tokens)
            .await
    }

    pub async fn summarize_context_messages(
        &self,
        messages: &[ContextMessage],
        plan_state: Option<&PlanModeState>,
        summary_max_tokens: usize,
    ) -> Result<String, AishError> {
        let prompt = build_context_summary_prompt(messages, plan_state, summary_max_tokens);
        self.generate_compact_summary(prompt, summary_max_tokens)
            .await
    }

    /// Execute a tool call by its [`ToolCall`] descriptor (public wrapper).
    pub async fn execute_tool_external(&self, tool_call: &ToolCall) -> ToolResult {
        self.execute_tool(tool_call).await
    }

    /// Execute a tool by name with given arguments (async path).
    pub async fn execute_tool_by_name(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> Result<ToolResult, String> {
        let tool = self
            .tools
            .get(name)
            .ok_or_else(|| format!("Unknown tool: {}", name))?;
        if let Some(result) = self.run_tool_preflight(name, tool.as_ref(), &args) {
            return Ok(result);
        }
        Ok(tool.as_ref().execute_async_in_session(args, self).await)
    }

    /// Emit an event through the callback (public for agent use).
    pub fn emit_event(&self, event: LlmEvent) {
        if let Some(cb) = &self.event_callback {
            let _ = cb(event);
        }
    }

    /// Process user input: send to LLM, handle tool calls in a loop, return final response.
    pub async fn process_input(
        &self,
        user_msg: &ChatMessage,
        context_messages: &[ChatMessage],
        system_message: Option<&str>,
        stream: bool,
    ) -> Result<crate::types::ProcessResult, AishError> {
        self.cancellation_token.reset();

        // Emit operation start event
        self.emit_event(LlmEvent {
            event_type: LlmEventType::OpStart,
            data: serde_json::json!({"prompt_length": user_msg.text_byte_len()}),
            timestamp: now_timestamp(),
            metadata: None,
        });

        // Start or reuse Langfuse session trace.
        // The first call creates a session-level trace; subsequent turns
        // reuse the same trace so all generations/spans are grouped together.
        let trace_id = if let Some(ref langfuse) = self.langfuse {
            let existing_trace_id = self.langfuse_session_id.lock().unwrap().clone();
            let trace_id = if let Some(id) = existing_trace_id {
                Some(id)
            } else {
                let id = langfuse
                    .trace_session("session", &serde_json::json!({"session_start": true}))
                    .await;
                let mut session_id_guard = self.langfuse_session_id.lock().unwrap();
                Some(session_id_guard.get_or_insert_with(|| id.clone()).clone())
            };
            self.langfuse_turn_counter
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            trace_id
        } else {
            None
        };

        // Build initial message list
        let mut messages: Vec<ChatMessage> = Vec::new();
        let prompt_bundle = crate::prompt::PromptAssembly::build(
            self,
            crate::prompt::PromptContext::MainChat,
            system_message.unwrap_or(""),
        );
        if system_message.is_some() {
            messages.push(ChatMessage::system(prompt_bundle.system_message));
        }
        messages.extend_from_slice(context_messages);
        messages.push(user_msg.clone());

        messages = self.prepare_messages_for_send(messages).await;

        // OpenAI-compat prompt cache hint for Claude models on proxy endpoints only.
        let is_openai_compat_claude = self.api_dialect == ApiDialect::OpenAiCompletions
            && self.stream_ctx.model.to_lowercase().contains("claude");
        if is_openai_compat_claude {
            if let Some(msg) = messages.first_mut() {
                if msg.role == "system" {
                    msg.cache_control = Some(CacheControl::ephemeral());
                }
            }
        }

        let initial_len = messages.len();

        let tool_specs = prompt_bundle.tool_specs;
        let has_tools = !tool_specs.is_empty();

        // Tool calling loop (max iterations to prevent infinite loops)
        let mut iterations = 0u32;
        let max_iterations = 20u32;
        let mut consecutive_failures = 0usize;

        loop {
            if self.cancellation_token.is_cancelled() {
                self.emit_event(LlmEvent {
                    event_type: LlmEventType::Cancelled,
                    data: serde_json::json!({}),
                    timestamp: now_timestamp(),
                    metadata: None,
                });
                self.emit_event(LlmEvent {
                    event_type: LlmEventType::OpEnd,
                    data: serde_json::json!({"reason": "cancelled"}),
                    timestamp: now_timestamp(),
                    metadata: None,
                });
                return Err(AishError::Cancelled);
            }
            if iterations >= max_iterations {
                // Ask user whether to continue instead of hard-erroring
                let should_continue = if let Some(ref cb) = self.iteration_limit_callback {
                    cb(iterations)
                } else {
                    false
                };
                if should_continue {
                    tracing::info!(
                        iterations,
                        "User approved continuing past iteration limit, resetting counter"
                    );
                    iterations = 0;
                } else {
                    self.emit_event(LlmEvent {
                        event_type: LlmEventType::Error,
                        data: serde_json::json!({"error": "Max tool call iterations reached"}),
                        timestamp: now_timestamp(),
                        metadata: None,
                    });
                    self.emit_event(LlmEvent {
                        event_type: LlmEventType::OpEnd,
                        data: serde_json::json!({"reason": "max_iterations"}),
                        timestamp: now_timestamp(),
                        metadata: None,
                    });
                    return Err(AishError::Llm("Max tool call iterations reached".into()));
                }
            }
            iterations += 1;

            messages = self.prepare_messages_for_send(messages).await;

            // Estimate prompt tokens locally for the context bar.
            // This works even when the streaming API doesn't report prompt_tokens.
            let estimate = estimate_chat_tokens(&messages, &self.context_budget_policy);
            self.last_prompt_estimate
                .store(estimate as u64, std::sync::atomic::Ordering::Relaxed);

            // Emit generation start BEFORE the API call so the display layer
            // can show a thinking animation while the request is in flight.
            // This matches the Python implementation where generation_start
            // fires before _create_completion_response.
            self.emit_event(LlmEvent {
                event_type: LlmEventType::GenerationStart,
                data: serde_json::json!({
                    "iteration": iterations,
                    "has_tools": has_tools,
                }),
                timestamp: now_timestamp(),
                metadata: None,
            });

            let response = match self
                .chat_completion(
                    &messages,
                    if has_tools { Some(&tool_specs) } else { None },
                    stream,
                    self.temperature,
                    self.max_tokens,
                )
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    self.emit_event(LlmEvent {
                        event_type: LlmEventType::Error,
                        data: serde_json::json!({"error": e.to_string()}),
                        timestamp: now_timestamp(),
                        metadata: None,
                    });
                    self.emit_event(LlmEvent {
                        event_type: LlmEventType::OpEnd,
                        data: serde_json::json!({"reason": "api_error"}),
                        timestamp: now_timestamp(),
                        metadata: None,
                    });
                    return Err(e);
                }
            };

            match response {
                LlmResponse::Json(json) => {
                    let (content, reasoning_content, tool_calls, usage) =
                        StreamParser::parse_response(&json);
                    let (pt, ct) = usage
                        .as_ref()
                        .map(|u| (u.prompt_tokens, u.completion_tokens))
                        .unwrap_or((0, 0));
                    if let Some(u) = usage {
                        self.record_usage(u);
                    }

                    // Log generation span to Langfuse for every LLM call
                    if let (Some(ref langfuse), Some(ref tid)) = (&self.langfuse, &trace_id) {
                        let span_name = format!(
                            "turn-{}-iter-{}",
                            self.langfuse_turn_counter
                                .load(std::sync::atomic::Ordering::Relaxed),
                            iterations
                        );
                        let resolved_model = self.stream_ctx.resolved_model();
                        langfuse
                            .span_generation(
                                tid,
                                &span_name,
                                &resolved_model,
                                serde_json::json!(messages),
                                content.as_deref().unwrap_or(""),
                                pt,
                                ct,
                            )
                            .await;
                    }

                    if tool_calls.is_empty() {
                        // Flush Langfuse buffer
                        if let Some(ref langfuse) = self.langfuse {
                            langfuse.flush().await;
                        }
                        // Emit generation end for final content response
                        self.emit_event(LlmEvent {
                            event_type: LlmEventType::GenerationEnd,
                            data: serde_json::json!({}),
                            timestamp: now_timestamp(),
                            metadata: None,
                        });
                        self.emit_event(LlmEvent {
                            event_type: LlmEventType::OpEnd,
                            data: serde_json::json!({"reason": "complete"}),
                            timestamp: now_timestamp(),
                            metadata: None,
                        });
                        let new_messages = messages[initial_len..].to_vec();
                        return Ok(crate::types::ProcessResult {
                            text: content.unwrap_or_default(),
                            new_messages,
                        });
                    }

                    // Add assistant message with tool calls
                    let assistant_msg = json
                        .get("choices")
                        .and_then(|c| c.as_array())
                        .and_then(|a| a.first())
                        .and_then(|c| c.get("message"));

                    if let Some(msg) = assistant_msg {
                        let mut chat_msg = ChatMessage::assistant("");
                        chat_msg.content =
                            extract_message_text(msg.get("content")).map(MessageContent::Text);
                        chat_msg.tool_calls = Some(tool_calls.clone());
                        chat_msg.reasoning_content = reasoning_content;
                        messages.push(chat_msg);
                    }

                    // Execute each tool call and append results
                    for tc in &tool_calls {
                        let result = self.execute_tool(tc).await;
                        let short_circuit = is_short_circuit_result(&result);
                        let output = result.output.clone();

                        // Log tool call span to Langfuse
                        if let (Some(ref langfuse), Some(ref tid)) = (&self.langfuse, &trace_id) {
                            langfuse
                                .span_tool_call(tid, &tc.name, &tc.arguments, &output, 0)
                                .await;
                        }
                        if short_circuit {
                            self.emit_event(LlmEvent {
                                event_type: LlmEventType::GenerationEnd,
                                data: serde_json::json!({}),
                                timestamp: now_timestamp(),
                                metadata: None,
                            });
                            self.emit_event(LlmEvent {
                                event_type: LlmEventType::OpEnd,
                                data: serde_json::json!({"reason": "short_circuit"}),
                                timestamp: now_timestamp(),
                                metadata: None,
                            });
                            // Sub-agent cancel is shown by the shell as `shell.interrupted`
                            // (same as Ctrl+C); do not return the tool string as AI body.
                            let suppress_body = result.meta.as_ref().is_some_and(|meta| {
                                matches!(
                                    meta.get("reason").and_then(|v| v.as_str()),
                                    Some("sub_agent_cancelled") | Some("user_cancelled")
                                )
                            });
                            let text = if self.security_notice_callback.is_some() || suppress_body {
                                String::new()
                            } else {
                                output
                            };
                            let new_messages = messages[initial_len..].to_vec();
                            return Ok(crate::types::ProcessResult { text, new_messages });
                        }

                        // Track consecutive failures for early termination.
                        // short_circuit results (security blocked) are excluded.
                        if result.ok {
                            consecutive_failures = 0;
                        } else {
                            consecutive_failures += 1;
                        }
                        if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                            tracing::warn!(
                                consecutive_failures,
                                "Too many consecutive tool failures, stopping loop"
                            );
                            self.emit_event(LlmEvent {
                                event_type: LlmEventType::Error,
                                data: serde_json::json!({
                                    "error": format!(
                                        "Stopped: {} consecutive tool failures",
                                        consecutive_failures
                                    ),
                                    "consecutive_failures": consecutive_failures,
                                }),
                                timestamp: now_timestamp(),
                                metadata: None,
                            });
                            self.emit_event(LlmEvent {
                                event_type: LlmEventType::OpEnd,
                                data: serde_json::json!({"reason": "consecutive_failures"}),
                                timestamp: now_timestamp(),
                                metadata: None,
                            });
                            messages.push(ChatMessage::tool_result(&tc.id, output));
                            let text = format!(
                                "Stopped after {} consecutive tool execution failures. \
                                 Please check your connection and retry.",
                                consecutive_failures
                            );
                            let new_messages = messages[initial_len..].to_vec();
                            return Ok(crate::types::ProcessResult { text, new_messages });
                        }

                        messages.push(ChatMessage::tool_result(&tc.id, output));
                    }

                    // Trim old tool-call rounds to prevent unbounded growth
                    smart_trim_tool_loop(&mut messages, initial_len);
                }

                LlmResponse::Stream(resp) => {
                    let mut accumulated = String::with_capacity(4096);
                    let mut reasoning_accumulated = String::with_capacity(1024);
                    let mut tool_calls_accum: HashMap<usize, (String, String, String)> =
                        HashMap::with_capacity(8); // index -> (id, name, args)

                    let mut stream_done = false;
                    let mut text_buffer = String::with_capacity(4096);
                    let mut stream = resp;
                    let mut reasoning_started = false;
                    // Track whether tool calls have been seen in this stream,
                    // used to emit content preview (matching Python's
                    // content_preview_started / tool_calls_seen logic).
                    let mut tool_calls_seen = false;
                    let mut content_preview_started = false;
                    // Accumulate token usage from SSE chunks
                    let mut stream_prompt_tokens: u64 = 0;
                    let mut stream_completion_tokens: u64 = 0;

                    while !stream_done {
                        if self.cancellation_token.is_cancelled() {
                            return Err(AishError::Cancelled);
                        }

                        match stream.chunk().await {
                            Ok(Some(chunk)) => {
                                text_buffer.push_str(&String::from_utf8_lossy(&chunk));

                                // Process complete SSE blocks (delimited by double newline)
                                while let Some(pos) = text_buffer.find("\n\n") {
                                    let block = text_buffer[..pos].to_string();
                                    text_buffer = text_buffer[pos + 2..].to_string();

                                    for line in block.lines() {
                                        let (events, chunk_usage) =
                                            StreamParser::parse_sse_chunk(line);
                                        if let Some(u) = chunk_usage {
                                            stream_prompt_tokens += u.prompt_tokens;
                                            stream_completion_tokens += u.completion_tokens;
                                            self.record_usage(u);
                                        }
                                        for event in events {
                                            match event {
                                                SseEvent::ContentDelta(delta) => {
                                                    accumulated.push_str(&delta);
                                                    // Python pattern: only emit content
                                                    // delta during streaming when tool
                                                    // calls are present. For plain
                                                    // conversations the response is
                                                    // rendered by the caller after the
                                                    // operation completes.
                                                    if tool_calls_seen {
                                                        if !content_preview_started {
                                                            content_preview_started = true;
                                                            self.emit_content_delta(
                                                                &accumulated,
                                                                &accumulated,
                                                            );
                                                        } else {
                                                            self.emit_content_delta(
                                                                &delta,
                                                                &accumulated,
                                                            );
                                                        }
                                                    }
                                                }
                                                SseEvent::ReasoningDelta(delta) => {
                                                    reasoning_accumulated.push_str(&delta);
                                                    if !reasoning_started {
                                                        reasoning_started = true;
                                                        self.emit_event(LlmEvent {
                                                            event_type:
                                                                LlmEventType::ReasoningStart,
                                                            data: serde_json::json!({}),
                                                            timestamp: now_timestamp(),
                                                            metadata: None,
                                                        });
                                                    }
                                                    self.emit_event(LlmEvent {
                                                        event_type: LlmEventType::ReasoningDelta,
                                                        data: serde_json::json!({
                                                            "delta": delta
                                                        }),
                                                        timestamp: now_timestamp(),
                                                        metadata: None,
                                                    });
                                                }
                                                SseEvent::ToolCallDelta {
                                                    index,
                                                    id,
                                                    name,
                                                    arguments,
                                                } => {
                                                    tool_calls_seen = true;
                                                    let entry = tool_calls_accum
                                                        .entry(index)
                                                        .or_insert_with(|| {
                                                            (
                                                                String::new(),
                                                                String::new(),
                                                                String::new(),
                                                            )
                                                        });
                                                    if let Some(i) = id {
                                                        if !i.is_empty() {
                                                            entry.0 = i;
                                                        }
                                                    }
                                                    if let Some(n) = name {
                                                        if !n.is_empty() {
                                                            entry.1 = n;
                                                        }
                                                    }
                                                    if let Some(a) = arguments {
                                                        entry.2.push_str(&a);
                                                    }
                                                }
                                                SseEvent::Finish(_) => {}
                                                SseEvent::Done => {
                                                    stream_done = true;
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            Ok(None) => {
                                stream_done = true;
                            }
                            Err(e) => {
                                // Emit error event (matching Python's streaming error
                                // pattern: emit error and break, don't panic).
                                self.emit_event(LlmEvent {
                                    event_type: LlmEventType::Error,
                                    data: serde_json::json!({
                                        "error_type": "streaming_error",
                                        "error_message": format!("Stream error: {}", e),
                                    }),
                                    timestamp: now_timestamp(),
                                    metadata: None,
                                });
                                // End reasoning if active before returning error
                                if reasoning_started {
                                    self.emit_event(LlmEvent {
                                        event_type: LlmEventType::ReasoningEnd,
                                        data: serde_json::json!({}),
                                        timestamp: now_timestamp(),
                                        metadata: None,
                                    });
                                }
                                return Err(AishError::Llm(format!("Stream error: {}", e)));
                            }
                        }
                    }

                    // End reasoning if it was started
                    if reasoning_started {
                        self.emit_event(LlmEvent {
                            event_type: LlmEventType::ReasoningEnd,
                            data: serde_json::json!({}),
                            timestamp: now_timestamp(),
                            metadata: None,
                        });
                    }

                    // Log generation span to Langfuse for every LLM call
                    if let (Some(ref langfuse), Some(ref tid)) = (&self.langfuse, &trace_id) {
                        let span_name = format!(
                            "turn-{}-iter-{}",
                            self.langfuse_turn_counter
                                .load(std::sync::atomic::Ordering::Relaxed),
                            iterations
                        );
                        let resolved_model = self.stream_ctx.resolved_model();
                        langfuse
                            .span_generation(
                                tid,
                                &span_name,
                                &resolved_model,
                                serde_json::json!(messages),
                                &accumulated,
                                stream_prompt_tokens,
                                stream_completion_tokens,
                            )
                            .await;
                    }

                    // No tool calls — return accumulated content
                    if tool_calls_accum.is_empty() {
                        // Flush Langfuse buffer
                        if let Some(ref langfuse) = self.langfuse {
                            langfuse.flush().await;
                        }
                        // Emit generation end for streamed content response
                        self.emit_event(LlmEvent {
                            event_type: LlmEventType::GenerationEnd,
                            data: serde_json::json!({}),
                            timestamp: now_timestamp(),
                            metadata: None,
                        });
                        self.emit_event(LlmEvent {
                            event_type: LlmEventType::OpEnd,
                            data: serde_json::json!({"reason": "stream_complete"}),
                            timestamp: now_timestamp(),
                            metadata: None,
                        });
                        let new_messages = messages[initial_len..].to_vec();
                        return Ok(crate::types::ProcessResult {
                            text: accumulated,
                            new_messages,
                        });
                    }

                    // Build sorted tool calls from accumulated deltas.
                    // Validate that all accumulated tool calls have non-empty
                    // id and name (matching Python's missing_ids check).
                    let mut sorted_calls: Vec<(usize, (String, String, String))> =
                        tool_calls_accum.into_iter().collect();
                    sorted_calls.sort_by_key(|(i, _)| *i);

                    let mut missing_ids = Vec::new();
                    let tool_calls: Vec<ToolCall> = sorted_calls
                        .into_iter()
                        .enumerate()
                        .filter_map(|(seq_idx, (orig_idx, (id, name, args)))| {
                            // Some providers omit the id in streaming deltas.
                            // Fall back to a synthetic id so the tool call can still execute.
                            let id = if id.is_empty() {
                                format!("tc_{orig_idx}")
                            } else {
                                id
                            };
                            if name.is_empty() {
                                missing_ids.push(seq_idx);
                                None
                            } else {
                                Some(ToolCall {
                                    id,
                                    name,
                                    arguments: args,
                                })
                            }
                        })
                        .collect();

                    if !missing_ids.is_empty() {
                        tracing::warn!(
                            "Dropping tool calls with missing name at indexes: {:?}",
                            missing_ids
                        );
                        // Emit error event for malformed tool calls
                        self.emit_event(LlmEvent {
                            event_type: LlmEventType::Error,
                            data: serde_json::json!({
                                "error_type": "stream_chunk_builder_error",
                                "error_message": format!(
                                    "tool_calls missing name at indexes: {:?}",
                                    missing_ids
                                ),
                            }),
                            timestamp: now_timestamp(),
                            metadata: None,
                        });
                    }

                    // If all tool calls were malformed, return accumulated content
                    if tool_calls.is_empty() {
                        self.emit_event(LlmEvent {
                            event_type: LlmEventType::GenerationEnd,
                            data: serde_json::json!({}),
                            timestamp: now_timestamp(),
                            metadata: None,
                        });
                        self.emit_event(LlmEvent {
                            event_type: LlmEventType::OpEnd,
                            data: serde_json::json!({"reason": "malformed_tool_calls"}),
                            timestamp: now_timestamp(),
                            metadata: None,
                        });
                        let new_messages = messages[initial_len..].to_vec();
                        return Ok(crate::types::ProcessResult {
                            text: accumulated,
                            new_messages,
                        });
                    }

                    // Add assistant message
                    let mut assistant_msg = ChatMessage::assistant("");
                    assistant_msg.content = if accumulated.is_empty() {
                        None
                    } else {
                        Some(MessageContent::Text(accumulated))
                    };
                    assistant_msg.tool_calls = Some(tool_calls.clone());
                    if !reasoning_accumulated.is_empty() {
                        assistant_msg.reasoning_content = Some(reasoning_accumulated);
                    }
                    messages.push(assistant_msg);

                    // Execute tools
                    for tc in &tool_calls {
                        let result = self.execute_tool(tc).await;
                        let short_circuit = is_short_circuit_result(&result);
                        let output = result.output.clone();

                        // Log tool call span to Langfuse
                        if let (Some(ref langfuse), Some(ref tid)) = (&self.langfuse, &trace_id) {
                            langfuse
                                .span_tool_call(tid, &tc.name, &tc.arguments, &output, 0)
                                .await;
                        }
                        if short_circuit {
                            self.emit_event(LlmEvent {
                                event_type: LlmEventType::GenerationEnd,
                                data: serde_json::json!({}),
                                timestamp: now_timestamp(),
                                metadata: None,
                            });
                            self.emit_event(LlmEvent {
                                event_type: LlmEventType::OpEnd,
                                data: serde_json::json!({"reason": "short_circuit"}),
                                timestamp: now_timestamp(),
                                metadata: None,
                            });
                            // Sub-agent cancel is shown by the shell as `shell.interrupted`
                            // (same as Ctrl+C); do not return the tool string as AI body.
                            let suppress_body = result.meta.as_ref().is_some_and(|meta| {
                                matches!(
                                    meta.get("reason").and_then(|v| v.as_str()),
                                    Some("sub_agent_cancelled") | Some("user_cancelled")
                                )
                            });
                            let text = if self.security_notice_callback.is_some() || suppress_body {
                                String::new()
                            } else {
                                output
                            };
                            let new_messages = messages[initial_len..].to_vec();
                            return Ok(crate::types::ProcessResult { text, new_messages });
                        }

                        // Track consecutive failures for early termination.
                        // short_circuit results (security blocked) are excluded.
                        if result.ok {
                            consecutive_failures = 0;
                        } else {
                            consecutive_failures += 1;
                        }
                        if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                            tracing::warn!(
                                consecutive_failures,
                                "Too many consecutive tool failures, stopping loop"
                            );
                            self.emit_event(LlmEvent {
                                event_type: LlmEventType::Error,
                                data: serde_json::json!({
                                    "error": format!(
                                        "Stopped: {} consecutive tool failures",
                                        consecutive_failures
                                    ),
                                    "consecutive_failures": consecutive_failures,
                                }),
                                timestamp: now_timestamp(),
                                metadata: None,
                            });
                            self.emit_event(LlmEvent {
                                event_type: LlmEventType::OpEnd,
                                data: serde_json::json!({"reason": "consecutive_failures"}),
                                timestamp: now_timestamp(),
                                metadata: None,
                            });
                            messages.push(ChatMessage::tool_result(&tc.id, output));
                            let text = format!(
                                "Stopped after {} consecutive tool execution failures. \
                                 Please check your connection and retry.",
                                consecutive_failures
                            );
                            let new_messages = messages[initial_len..].to_vec();
                            return Ok(crate::types::ProcessResult { text, new_messages });
                        }

                        messages.push(ChatMessage::tool_result(&tc.id, output));
                    }

                    // Smart-trim old tool outputs to prevent unbounded growth
                    smart_trim_tool_loop(&mut messages, initial_len);
                }
            }
        }
    }

    /// Simple completion without tool calling or context.
    pub async fn completion(
        &self,
        prompt: &str,
        system_message: Option<&str>,
        stream: bool,
    ) -> Result<String, AishError> {
        let mut messages = Vec::new();
        if let Some(sys) = system_message {
            messages.push(ChatMessage::system(sys));
        }
        messages.push(ChatMessage::user(prompt));

        let response = self
            .chat_completion(&messages, None, stream, self.temperature, self.max_tokens)
            .await?;

        match response {
            LlmResponse::Json(json) => {
                let (content, _reasoning, _tool_calls, usage) = StreamParser::parse_response(&json);
                if let Some(u) = usage {
                    self.record_usage(u);
                }
                Ok(content.unwrap_or_default())
            }
            LlmResponse::Stream(_) => {
                // Delegate to process_input for streaming handling
                let user_msg = ChatMessage::user(prompt);
                let result = self
                    .process_input(&user_msg, &[], system_message, true)
                    .await?;
                Ok(result.text)
            }
        }
    }

    /// Execute a single tool call, emitting start/end events.
    ///
    /// Follows the Python `execute_tool` pattern:
    /// - Normalizes tool results (wraps panics/errors gracefully).
    /// - Retries once on execution failure (matching Python's robustness).
    /// - Emits structured TOOL_EXECUTION_START / TOOL_EXECUTION_END events.
    async fn execute_tool(&self, tool_call: &ToolCall) -> ToolResult {
        let args: serde_json::Value =
            serde_json::from_str(&tool_call.arguments).unwrap_or(serde_json::Value::Null);

        if let Some(tool) = self.tools.get(&tool_call.name) {
            if let Some(result) = self.run_tool_preflight(&tool_call.name, tool.as_ref(), &args) {
                return result;
            }

            self.emit_event(LlmEvent {
                event_type: LlmEventType::ToolExecutionStart,
                data: serde_json::json!({
                    "tool_name": tool_call.name,
                    "tool_call_id": tool_call.id,
                    "tool_args": args
                }),
                timestamp: now_timestamp(),
                metadata: None,
            });

            // Execute with retry: try once, retry once on failure.
            // Mirrors Python's normalize_tool_result + error recovery pattern.
            let result = {
                let first = tool
                    .as_ref()
                    .execute_async_in_session(args.clone(), self)
                    .await;
                if first.ok || is_short_circuit_result(&first) {
                    first
                } else {
                    // Retry once — log the retry attempt
                    tracing::warn!(
                        "Tool '{}' failed, retrying once: {}",
                        tool_call.name,
                        first.output
                    );
                    let second = tool
                        .as_ref()
                        .execute_async_in_session(args.clone(), self)
                        .await;
                    if second.ok {
                        second
                    } else {
                        // Combine both error messages for diagnostic clarity
                        ToolResult {
                            ok: false,
                            output: format!(
                                "{}\n(retry also failed: {})",
                                first.output, second.output
                            ),
                            meta: second.meta,
                        }
                    }
                }
            };

            // Update plan state based on tool result metadata
            if let Some(ref meta) = result.meta {
                if let Some(action) = meta.get("action").and_then(|a| a.as_str()) {
                    match action {
                        "enter_plan_mode" => {
                            let mut state = self.plan_state.lock().unwrap();
                            state.phase = PlanPhase::Planning;
                            state.summary = meta
                                .get("summary")
                                .and_then(|s| s.as_str())
                                .map(|s| s.to_string());
                        }
                        "exit_plan_mode" => {
                            let mut state = self.plan_state.lock().unwrap();
                            state.phase = PlanPhase::Normal;
                        }
                        _ => {}
                    }
                }
            }

            // Prepare output preview only for bash tool (used for terminal display).
            let (output_preview, total_lines) = if tool_call.name == "bash" {
                let s = &result.output;
                // Use raw line count from PTY output (stored in meta by
                // the bash tool) instead of counting lines in the tagged
                // tool output which may be truncated by offload.
                let total = result
                    .meta
                    .as_ref()
                    .and_then(|m| m.get("raw_line_count"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or_else(|| s.lines().count() as u64)
                    as usize;
                let limit = 512.min(s.len());
                // Find safe UTF-8 boundary
                let mut end = limit;
                while end > 0 && end < s.len() && !s.is_char_boundary(end) {
                    end -= 1;
                }
                (Some(s[..end].to_string()), total)
            } else {
                (None, 0)
            };

            let mut event_data = serde_json::json!({
                "tool_name": tool_call.name,
                "tool_call_id": tool_call.id,
                "ok": result.ok,
                "tool_args": args,
            });
            if let Some(preview) = output_preview {
                event_data["output_preview"] = serde_json::Value::String(preview);
            }
            if tool_call.name == "bash" && total_lines > 0 {
                event_data["output_total_lines"] =
                    serde_json::Value::Number(serde_json::Number::from(total_lines));
                // Pass full output for Ctrl+O expand panel display.
                // When output was offloaded, read full content from disk
                // (result.output only contains a 1 KB preview in that case).
                // Cap at 256 KB to avoid passing huge data through events.
                const MAX_FULL_OUTPUT: usize = 256 * 1024;
                let full = result
                    .meta
                    .as_ref()
                    .and_then(|m| m.get("offload"))
                    .and_then(|o| o.get("stdout_path"))
                    .and_then(|p| p.as_str())
                    .and_then(|path| std::fs::read_to_string(path).ok())
                    .unwrap_or_else(|| result.output.clone());
                let full = if full.len() > MAX_FULL_OUTPUT {
                    let mut end = MAX_FULL_OUTPUT;
                    while end > 0 && !full.is_char_boundary(end) {
                        end -= 1;
                    }
                    format!("{}...(truncated)", &full[..end])
                } else {
                    full
                };
                event_data["output_full"] = serde_json::Value::String(full);
            }

            self.emit_event(LlmEvent {
                event_type: LlmEventType::ToolExecutionEnd,
                data: event_data,
                timestamp: now_timestamp(),
                metadata: None,
            });

            // Audit: record the AI tool invocation.
            let audited_args = self.redact(&tool_call.arguments);
            let result_summary = {
                let full = self.redact(&result.output);
                const MAX_AUDIT_RESULT: usize = 1024;
                if full.len() > MAX_AUDIT_RESULT {
                    let mut end = MAX_AUDIT_RESULT;
                    while end > 0 && !full.is_char_boundary(end) {
                        end -= 1;
                    }
                    format!("{}...(truncated)", &full[..end])
                } else {
                    full
                }
            };
            self.emit_audit(AuditEvent::ai_tool(
                chrono::Utc::now(),
                self.audit_session_uuid.clone(),
                None,
                None,
                tool_call.name.clone(),
                audited_args,
                result_summary,
            ));

            result
        } else {
            ToolResult::error(format!("Unknown tool: {}", tool_call.name))
        }
    }

    /// Create an isolated subsession that shares the LLM client credentials
    /// and confirmation callback but has independent event handling, cancellation,
    /// and an empty tool registry.
    ///
    /// The caller is responsible for registering tools in the subsession.
    pub fn create_subsession(&self) -> Self {
        Self {
            stream_ctx: self.stream_ctx.clone(),
            api_dialect: self.api_dialect,
            tools: HashMap::with_capacity(16),
            cancellation_token: Arc::new(CancellationToken::new()),
            event_callback: self.event_callback.clone(),
            confirmation_callback: self.confirmation_callback.clone(),
            security_notice_callback: self.security_notice_callback.clone(),
            iteration_limit_callback: self.iteration_limit_callback.clone(),
            audit_sink: self.audit_sink.clone(),
            audit_redactor: self.audit_redactor.clone(),
            audit_session_uuid: self.audit_session_uuid.clone(),
            audit_user: self.audit_user.clone(),
            audit_host: self.audit_host.clone(),
            temperature: self.temperature,
            max_tokens: self.max_tokens,
            langfuse: self.langfuse.clone(),
            langfuse_session_id: std::sync::Mutex::new(
                self.langfuse_session_id.lock().unwrap().clone(),
            ),
            langfuse_turn_counter: std::sync::atomic::AtomicU32::new(0),
            max_context_tokens: self.max_context_tokens,
            context_budget_policy: self.context_budget_policy.clone(),
            compact_consecutive_failures: std::sync::Mutex::new(0),
            plan_state: Arc::new(Mutex::new(PlanModeState::default())),
            token_stats: std::sync::Mutex::new(crate::usage::TokenStats::default()),
            last_prompt_estimate: std::sync::atomic::AtomicU64::new(0),
            tool_execution_policy: self.tool_execution_policy,
            is_sub_agent: true,
            #[cfg(test)]
            test_chat_responses: None,
        }
    }

    fn run_tool_preflight(
        &self,
        tool_name: &str,
        tool: &dyn Tool,
        args: &serde_json::Value,
    ) -> Option<ToolResult> {
        if self.is_sub_agent && crate::prompt::SUBAGENT_GLOBAL_DENY.contains(&tool_name) {
            return Some(ToolResult::error(format!(
                "Tool '{tool_name}' is not available in sub-agent sessions"
            )));
        }

        let ctx = crate::tool_context::ToolContext::for_session(self);
        match tool.preflight_with_context(args, &ctx) {
            PreflightResult::Allow => {
                self.emit_audit(AuditEvent::security_decision(
                    chrono::Utc::now(),
                    self.audit_session_uuid.clone(),
                    None,
                    None,
                    None,
                    "allow".to_string(),
                    None,
                    None,
                    Some("LOW".to_string()),
                ));
                None
            }
            PreflightResult::Confirm { message, security } => {
                let security = security.unwrap_or_else(|| {
                    PreflightSecurityContext::fallback(
                        tool_name.to_string(),
                        None,
                        message.clone(),
                        SecurityPanelMode::Confirm,
                    )
                });
                let approved = if let Some(ref cb) = self.confirmation_callback {
                    cb(&security)
                } else {
                    true
                };
                let (matched_rule, risk_level) = audit_security_info(&security);
                self.emit_audit(AuditEvent::security_decision(
                    chrono::Utc::now(),
                    self.audit_session_uuid.clone(),
                    None,
                    None,
                    security.target.as_ref().map(|t| self.redact(t)),
                    "confirm".to_string(),
                    Some(if approved {
                        "yes".to_string()
                    } else {
                        "no".to_string()
                    }),
                    matched_rule,
                    risk_level,
                ));
                if approved {
                    None
                } else {
                    Some(ToolResult::error(format!(
                        "Tool execution denied: {}",
                        message
                    )))
                }
            }
            PreflightResult::Block { message, security } => {
                let security = security.unwrap_or_else(|| {
                    PreflightSecurityContext::fallback(
                        tool_name.to_string(),
                        None,
                        message.clone(),
                        SecurityPanelMode::Blocked,
                    )
                });
                let (matched_rule, risk_level) = audit_security_info(&security);
                self.emit_audit(AuditEvent::security_decision(
                    chrono::Utc::now(),
                    self.audit_session_uuid.clone(),
                    None,
                    None,
                    security.target.as_ref().map(|t| self.redact(t)),
                    "block".to_string(),
                    None,
                    matched_rule,
                    risk_level,
                ));
                if let Some(ref cb) = self.security_notice_callback {
                    cb(&security);
                }
                Some(ToolResult {
                    ok: false,
                    output: format!("Blocked by security policy: {}", message),
                    meta: Some(serde_json::json!({
                        "dispatch_status": "short_circuit",
                        "reason": "security_blocked"
                    })),
                })
            }
        }
    }

    fn emit_content_delta(&self, delta: &str, accumulated: &str) {
        self.emit_event(LlmEvent {
            event_type: LlmEventType::ContentDelta,
            data: serde_json::json!({
                "delta": delta,
                "accumulated": accumulated
            }),
            timestamp: now_timestamp(),
            metadata: None,
        });
    }

    pub fn emit_context_compaction_start(&self, scope: &str, mode: &str) {
        self.emit_event(LlmEvent {
            event_type: LlmEventType::ContextCompactionStart,
            data: serde_json::json!({
                "scope": scope,
                "mode": mode,
            }),
            timestamp: now_timestamp(),
            metadata: None,
        });
    }

    pub fn emit_context_compaction_end(
        &self,
        scope: &str,
        mode: &str,
        before_tokens: usize,
        after_tokens: usize,
        changed: bool,
    ) {
        self.emit_event(LlmEvent {
            event_type: LlmEventType::ContextCompactionEnd,
            data: serde_json::json!({
                "scope": scope,
                "mode": mode,
                "changed": changed,
                "before_tokens": before_tokens,
                "after_tokens": after_tokens,
                "reclaimed_tokens": before_tokens.saturating_sub(after_tokens),
            }),
            timestamp: now_timestamp(),
            metadata: None,
        });
    }

    pub(crate) async fn prepare_messages_for_send(
        &self,
        mut messages: Vec<ChatMessage>,
    ) -> Vec<ChatMessage> {
        let policy = &self.context_budget_policy;
        if !policy.enabled {
            return trim_messages(messages, self.max_context_tokens, 5);
        }

        let before_tokens = estimate_chat_tokens(&messages, policy);
        let before_state = policy.state_for_tokens(before_tokens);
        let mut compaction_changed = false;
        let mut emitted_compaction_start = false;
        let mut compaction_mode = "microcompact";

        if matches!(
            before_state.pressure,
            ContextPressureLevel::Warning
                | ContextPressureLevel::AutoCompact
                | ContextPressureLevel::Blocking
        ) {
            let report = microcompact_chat_messages(&mut messages, policy);
            if report.changed_messages > 0 {
                compaction_changed = true;
                tracing::info!(
                    changed_messages = report.changed_messages,
                    reclaimed_tokens = report.reclaimed_tokens,
                    "send-path context microcompact completed"
                );
            }
        }

        let after_micro_tokens = estimate_chat_tokens(&messages, policy);
        let after_micro_state = policy.state_for_tokens(after_micro_tokens);
        if after_micro_state.is_above_auto_compact_threshold && policy.full_compact_enabled {
            self.emit_context_compaction_start("send_path", "full_compact");
            emitted_compaction_start = true;
            compaction_mode = "full_compact";
            let failures = *self.compact_consecutive_failures.lock().unwrap();
            if failures >= policy.max_consecutive_failures {
                tracing::warn!(
                    failures,
                    "send-path full compact skipped because failure fuse is open"
                );
            } else {
                match self
                    .full_compact_chat_messages_with_model(messages.clone(), policy)
                    .await
                {
                    Ok(compacted) => {
                        *self.compact_consecutive_failures.lock().unwrap() = 0;
                        compaction_changed = true;
                        messages = compacted;
                    }
                    Err(err) => {
                        let mut guard = self.compact_consecutive_failures.lock().unwrap();
                        *guard = (*guard).saturating_add(1);
                        tracing::warn!(
                            error = %err,
                            failures = *guard,
                            "send-path full compact failed; falling back to final trim"
                        );
                    }
                }
            }
        }

        if emitted_compaction_start || compaction_changed {
            let after_tokens = estimate_chat_tokens(&messages, policy);
            self.emit_context_compaction_end(
                "send_path",
                compaction_mode,
                before_tokens,
                after_tokens,
                compaction_changed,
            );
        }

        trim_messages(
            messages,
            self.max_context_tokens,
            policy.micro_keep_recent_messages.max(5),
        )
    }

    async fn full_compact_chat_messages_with_model(
        &self,
        messages: Vec<ChatMessage>,
        policy: &ContextBudgetPolicy,
    ) -> Result<Vec<ChatMessage>, String> {
        let keep_recent = policy.micro_keep_recent_messages.max(2);
        let recent_start = messages.len().saturating_sub(keep_recent);
        let old_messages: Vec<ChatMessage> = messages
            .iter()
            .take(recent_start)
            .filter(|m| m.role != "system")
            .cloned()
            .collect();
        if old_messages.is_empty() {
            return Err("no old chat messages available for full compact".to_string());
        }

        let prompt = build_chat_summary_prompt(&old_messages, policy.summary_max_tokens);
        let summary = self
            .generate_compact_summary(prompt, policy.summary_max_tokens)
            .await
            .map_err(|err| err.to_string())?;

        let mut compacted = Vec::new();
        for (idx, msg) in messages.iter().enumerate() {
            if idx >= recent_start {
                break;
            }
            if msg.role == "system" {
                compacted.push(msg.clone());
            }
        }
        compacted.push(ChatMessage::system(summary));
        compacted.extend(messages.into_iter().skip(recent_start));
        tracing::info!(
            compacted_messages = old_messages.len(),
            "send-path model full compact completed"
        );
        Ok(compacted)
    }

    async fn generate_compact_summary(
        &self,
        user_prompt: String,
        summary_max_tokens: usize,
    ) -> Result<String, AishError> {
        let messages = vec![
            ChatMessage::system(COMPACT_SUMMARY_SYSTEM_PROMPT),
            ChatMessage::user(user_prompt),
        ];
        let max_tokens = summary_max_tokens.min(u32::MAX as usize) as u32;
        let response = self
            .chat_completion(&messages, None, false, Some(0.1), Some(max_tokens))
            .await?;
        match response {
            LlmResponse::Json(json) => {
                let (content, _reasoning, _tool_calls, usage) = StreamParser::parse_response(&json);
                if let Some(u) = usage {
                    self.record_usage(u);
                }
                let summary = format_compact_summary(content.unwrap_or_default());
                if summary.trim().is_empty() {
                    return Err(AishError::Llm("compact summary is empty".to_string()));
                }
                Ok(summary)
            }
            LlmResponse::Stream(_) => Err(AishError::Llm(
                "compact summary unexpectedly returned a stream".to_string(),
            )),
        }
    }
}

/// Helper: current time as a UNIX timestamp in seconds (f64).
fn now_timestamp() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

fn build_context_summary_prompt(
    messages: &[ContextMessage],
    plan_state: Option<&PlanModeState>,
    summary_max_tokens: usize,
) -> String {
    let mut prompt = String::new();
    prompt.push_str("Summarize the older persistent AI Shell context below.\n");
    prompt.push_str("Focus on shell operations facts: user goal, important commands, exit codes, failures, stderr clues, paths, hosts, cwd, memory/skill hints, and pending next steps.\n");
    prompt.push_str(
        "Do not include low-value full stdout. Keep offload paths and return codes when present.\n",
    );
    prompt.push_str(&format!(
        "Target summary budget: about {} tokens.\n\n",
        summary_max_tokens
    ));
    if let Some(state) = plan_state {
        if state.phase == PlanPhase::Planning {
            prompt.push_str("Current plan mode state:\n");
            prompt.push_str(&format!(
                "- plan_id={:?}, artifact_path={:?}, draft_revision={}, approval_status={:?}\n\n",
                state.plan_id, state.artifact_path, state.draft_revision, state.approval_status
            ));
        }
    }
    prompt.push_str(
        "Return exactly one <conversation-summary> block with these sections when applicable:\n",
    );
    prompt.push_str("Summary, Current Goal, Important Commands, Failures And Evidence, Environment Facts, Pending Next Steps.\n\n");
    prompt.push_str("Older context messages:\n");
    for (idx, msg) in messages.iter().enumerate() {
        prompt.push_str(&format!(
            "\n<message index=\"{}\" role=\"{}\" memory_type=\"{}\">\n{}\n</message>\n",
            idx,
            msg.role,
            memory_type_label(&msg.memory_type),
            truncate_for_summary_prompt(&msg.content, 4_000)
        ));
    }
    prompt
}

fn build_chat_summary_prompt(messages: &[ChatMessage], summary_max_tokens: usize) -> String {
    let mut prompt = String::new();
    prompt.push_str("Summarize the older in-flight chat/tool context below for AI Shell.\n");
    prompt.push_str("Preserve user intent, command/tool results, failures, important stderr, and pending next steps. Drop verbose stdout and repeated low-value details.\n");
    prompt.push_str(&format!(
        "Target summary budget: about {} tokens.\n\n",
        summary_max_tokens
    ));
    prompt.push_str("Return exactly one <conversation-summary> block.\n\n");
    for (idx, msg) in messages.iter().enumerate() {
        let content = msg.text_content().unwrap_or("");
        prompt.push_str(&format!(
            "\n<message index=\"{}\" role=\"{}\">\n{}\n</message>\n",
            idx,
            msg.role,
            truncate_for_summary_prompt(content, 4_000)
        ));
        if let Some(reasoning) = &msg.reasoning_content {
            prompt.push_str(&format!(
                "<reasoning index=\"{}\">\n{}\n</reasoning>\n",
                idx,
                truncate_for_summary_prompt(reasoning, 1_000)
            ));
        }
    }
    prompt
}

fn memory_type_label(memory_type: &MemoryType) -> &'static str {
    match memory_type {
        MemoryType::Llm => "llm",
        MemoryType::Shell => "shell",
        MemoryType::Knowledge => "knowledge",
    }
}

fn truncate_for_summary_prompt(content: &str, max_chars: usize) -> String {
    if content.len() <= max_chars {
        return content.to_string();
    }
    let mut end = max_chars.min(content.len());
    while end > 0 && !content.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[truncated]", &content[..end])
}

fn format_compact_summary(summary: String) -> String {
    let mut text = strip_tag_block(&summary, "analysis");
    if let Some(inner) = extract_tag_inner(&text, "summary") {
        text = inner;
    }
    let trimmed = text.trim();
    if trimmed.starts_with("<conversation-summary") {
        trimmed.to_string()
    } else {
        format!(
            "<conversation-summary source=\"model_auto_compact\">\n{}\n</conversation-summary>",
            trimmed
        )
    }
}

fn strip_tag_block(text: &str, tag: &str) -> String {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);
    let Some(start) = text.find(&open) else {
        return text.to_string();
    };
    let Some(end_rel) = text[start + open.len()..].find(&close) else {
        return text.to_string();
    };
    let end = start + open.len() + end_rel + close.len();
    format!("{}{}", &text[..start], &text[end..])
}

fn extract_tag_inner(text: &str, tag: &str) -> Option<String> {
    let open = format!("<{}>", tag);
    let close = format!("</{}>", tag);
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close).map(|idx| start + idx)?;
    Some(text[start..end].trim().to_string())
}

fn estimate_chat_tokens(messages: &[ChatMessage], policy: &ContextBudgetPolicy) -> usize {
    messages
        .iter()
        .map(|m| estimate_one_chat_message(m, policy))
        .sum()
}

fn estimate_one_chat_message(message: &ChatMessage, _policy: &ContextBudgetPolicy) -> usize {
    let content_len = message.text_byte_len();
    let reasoning_len = message
        .reasoning_content
        .as_ref()
        .map(|c| c.len())
        .unwrap_or(0);
    ((content_len + reasoning_len) / 4).max(1)
}

fn microcompact_chat_messages(
    messages: &mut [ChatMessage],
    policy: &ContextBudgetPolicy,
) -> MicrocompactReport {
    let before_tokens = estimate_chat_tokens(messages, policy);
    let recent_start = messages
        .len()
        .saturating_sub(policy.micro_keep_recent_messages);
    let mut changed_messages = 0usize;

    for (idx, msg) in messages.iter_mut().enumerate() {
        if idx >= recent_start || msg.role == "system" {
            continue;
        }

        let mut changed = false;
        if msg.role == "tool" {
            if let Some(content) = msg.text_content() {
                if content.len() > 256 || is_low_value_chat_output(content) {
                    let mut replacement = String::from(
                        "[old tool output cleared by context microcompact; key metadata retained]",
                    );
                    if let Some(id) = &msg.tool_call_id {
                        replacement.push_str(&format!("\ntool_call_id: {}", id));
                    }
                    if let Some(return_code) = extract_return_code(content) {
                        replacement.push_str(&format!("\nreturn_code: {}", return_code));
                    }
                    msg.content = Some(MessageContent::Text(replacement));
                    changed = true;
                }
            }
        }

        if msg
            .reasoning_content
            .as_ref()
            .is_some_and(|s| s.len() > 512)
        {
            msg.reasoning_content =
                Some("[old reasoning content cleared by context microcompact]".to_string());
            changed = true;
        }

        if changed {
            changed_messages += 1;
        }
    }

    let after_tokens = estimate_chat_tokens(messages, policy);
    MicrocompactReport {
        changed_messages,
        reclaimed_tokens: before_tokens.saturating_sub(after_tokens),
    }
}

#[cfg(test)]
fn full_compact_chat_messages(
    messages: Vec<ChatMessage>,
    policy: &ContextBudgetPolicy,
) -> Result<Vec<ChatMessage>, String> {
    let keep_recent = policy.micro_keep_recent_messages.max(2);
    let recent_start = messages.len().saturating_sub(keep_recent);
    let old_messages: Vec<ChatMessage> = messages
        .iter()
        .take(recent_start)
        .filter(|m| m.role != "system")
        .cloned()
        .collect();
    if old_messages.is_empty() {
        return Err("no old chat messages available for full compact".to_string());
    }

    let summary = build_chat_summary(&old_messages, policy.summary_max_tokens);
    if summary.trim().is_empty() {
        return Err("chat compact summary is empty".to_string());
    }

    let mut compacted = Vec::new();
    for (idx, msg) in messages.iter().enumerate() {
        if idx >= recent_start {
            break;
        }
        if msg.role == "system" {
            compacted.push(msg.clone());
        }
    }
    compacted.push(ChatMessage::system(summary));
    compacted.extend(messages.into_iter().skip(recent_start));
    tracing::info!(
        compacted_messages = old_messages.len(),
        "send-path full compact completed"
    );
    Ok(compacted)
}

#[cfg(test)]
fn build_chat_summary(old_messages: &[ChatMessage], summary_max_tokens: usize) -> String {
    let mut lines = vec![
        "<conversation-summary source=\"send_path_auto_compact\">".to_string(),
        "Summary:".to_string(),
        format!("- Compacted older chat messages: {}.", old_messages.len()),
    ];
    for msg in old_messages
        .iter()
        .rev()
        .take(10)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        let content = msg.text_content().unwrap_or("");
        lines.push(format!(
            "- {}: {}",
            msg.role,
            summarize_chat_line(content, 220)
        ));
    }
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

#[cfg(test)]
fn summarize_chat_line(content: &str, max_chars: usize) -> String {
    let one_line = content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(4)
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

fn is_low_value_chat_output(content: &str) -> bool {
    content.contains("<stdout>")
        || content.contains("<stderr>")
        || content.contains("<offload>")
        || content.len() > 8_000
}

fn extract_return_code(content: &str) -> Option<String> {
    let open = "<return_code>";
    let close = "</return_code>";
    let start = content.find(open)? + open.len();
    let end = content[start..]
        .find(close)
        .map(|idx| start + idx)
        .unwrap_or(content.len());
    let value = content[start..end].trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// Trim messages to fit within a token budget.
///
/// Strategy:
/// - Always preserve the first message (system prompt)
/// - Remove oldest non-system messages when over budget
/// - Always keep the last `preserve_recent` messages
/// - Token estimation: ~4 chars per token (rough but fast)
fn trim_messages(
    messages: Vec<ChatMessage>,
    max_tokens: usize,
    preserve_recent: usize,
) -> Vec<ChatMessage> {
    // Rough token estimation: 4 chars per token
    let estimate_tokens = |msgs: &[ChatMessage]| -> usize {
        msgs.iter()
            .map(|m| {
                let content_len = m.text_byte_len();
                let reasoning_len = m.reasoning_content.as_ref().map(|c| c.len()).unwrap_or(0);
                (content_len + reasoning_len) / 4
            })
            .sum()
    };

    let total = estimate_tokens(&messages);
    if total <= max_tokens || messages.len() <= preserve_recent + 1 {
        return messages;
    }

    tracing::warn!(
        "Context trimming: {} estimated tokens exceeds budget of {}",
        total,
        max_tokens
    );

    // Split: all leading system messages + middle (to trim) + last N (to keep)
    let system_count = messages.iter().take_while(|m| m.role == "system").count();
    let system: Vec<_> = messages[..system_count].to_vec();

    let system_count = system.len();
    let recent_start = messages
        .len()
        .saturating_sub(preserve_recent)
        .max(system_count);
    let recent: Vec<_> = messages[recent_start..].to_vec();

    // Calculate how many middle messages to keep
    let system_tokens = estimate_tokens(&system);
    let recent_tokens = estimate_tokens(&recent);
    let middle_budget = max_tokens.saturating_sub(system_tokens + recent_tokens);

    let middle: Vec<_> = messages[system_count..recent_start].to_vec();
    let mut kept_middle = Vec::new();
    let mut middle_used = 0usize;

    // Keep newest middle messages that fit
    for msg in middle.into_iter().rev() {
        let msg_tokens = {
            let content_len = msg.text_byte_len();
            let reasoning_len = msg.reasoning_content.as_ref().map(|c| c.len()).unwrap_or(0);
            (content_len + reasoning_len) / 4
        };
        if middle_used + msg_tokens > middle_budget {
            break;
        }
        middle_used += msg_tokens;
        kept_middle.push(msg);
    }
    kept_middle.reverse();

    let mut result = system;
    result.extend(kept_middle);
    result.extend(recent);
    result
}

/// Token budget for the agent-loop smart trim. When estimated tokens in the
/// loop portion exceed this, old tool outputs are replaced with informative
/// one-line summaries instead of deleting entire rounds.
const SMART_TRIM_TOKEN_BUDGET: usize = 60_000;

/// Number of most-recent tool-call rounds that are always kept verbatim.
const SMART_TRIM_PROTECT_RECENT_ROUNDS: usize = 6;

/// Generate an informative one-line summary for a tool result, preserving
/// the most operationally useful information (command, file path, exit code).
fn summarize_tool_output(tool_name: &str, tool_args: &str, output: &str) -> String {
    let line_count = output.lines().count();
    let char_count = output.len();

    let args =
        serde_json::from_str::<serde_json::Value>(tool_args).unwrap_or(serde_json::Value::Null);

    match tool_name {
        "bash" => {
            let cmd = args.get("command").and_then(|v| v.as_str()).unwrap_or("?");
            let cmd_display = if cmd.len() > 80 {
                format!("{}...", &cmd[..77])
            } else {
                cmd.to_string()
            };
            let exit_code = extract_return_code(output).unwrap_or_else(|| "?".to_string());
            format!(
                "[bash] `{}` -> exit {}, {} lines",
                cmd_display, exit_code, line_count
            )
        }
        "read_file" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("?");
            let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(1);
            format!(
                "[read_file] {} from line {} ({} chars)",
                path, offset, char_count
            )
        }
        "write_file" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("?");
            let content_lines = args
                .get("content")
                .and_then(|v| v.as_str())
                .map(|s| s.lines().count())
                .unwrap_or(0);
            format!("[write_file] wrote to {} ({} lines)", path, content_lines)
        }
        "edit_file" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("?");
            format!("[edit_file] edited {} ({} chars result)", path, char_count)
        }
        "grep" | "search_files" => {
            let pattern = args.get("pattern").and_then(|v| v.as_str()).unwrap_or("?");
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let matches = if output.contains("No matches found") {
                0
            } else {
                output.lines().count().saturating_sub(1).max(1)
            };
            format!(
                "[{}] '{}' in {} -> {} matches",
                tool_name, pattern, path, matches
            )
        }
        "glob" | "list_files" => {
            let pattern = args.get("pattern").and_then(|v| v.as_str()).unwrap_or("?");
            format!("[{}] '{}' -> {} lines", tool_name, pattern, line_count)
        }
        "ask_user" => "[ask_user] asked user a question".to_string(),
        "memory" => {
            let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("?");
            format!("[memory] {}", action)
        }
        "final_answer" => "[final_answer] completed".to_string(),
        _ => {
            // Generic fallback: include first arg key-value pair
            let first_arg = if let Some(obj) = args.as_object() {
                obj.iter()
                    .take(2)
                    .map(|(k, v)| format!("{}={}", k, truncate_str(&v.to_string(), 40)))
                    .collect::<Vec<_>>()
                    .join(" ")
            } else {
                String::new()
            };
            format!(
                "[{}]{} ({} chars)",
                tool_name,
                if first_arg.is_empty() {
                    String::new()
                } else {
                    format!(" {}", first_arg)
                },
                char_count
            )
        }
    }
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max.saturating_sub(3)])
    }
}

/// Smart-trim tool-call messages in the agent loop.
///
/// Instead of deleting entire rounds (which loses tool-call structure and
/// context), this replaces old tool *outputs* with informative one-line
/// summaries while preserving the assistant's tool_calls metadata. The
/// trigger is a token budget rather than a fixed round count, making it
/// adaptive to different models and tool output sizes.
///
/// Guarantees:
/// - System messages and the stable prefix are never touched.
/// - The most recent `SMART_TRIM_PROTECT_RECENT_ROUNDS` rounds are kept verbatim.
/// - tool_call / tool_result pairing integrity is preserved.
fn smart_trim_tool_loop(messages: &mut [ChatMessage], initial_len: usize) {
    if messages.len() <= initial_len {
        return;
    }

    // Quick token estimate — if under budget, skip entirely.
    let loop_tokens: usize = messages[initial_len..]
        .iter()
        .map(|m| {
            let content_len = m.text_byte_len();
            let reasoning_len = m.reasoning_content.as_ref().map(|c| c.len()).unwrap_or(0);
            (content_len + reasoning_len) / 4
        })
        .sum();
    if loop_tokens <= SMART_TRIM_TOKEN_BUDGET {
        return;
    }

    // Build index: tool_call_id -> (tool_name, arguments_json)
    let mut call_id_to_tool: HashMap<String, (String, String)> = HashMap::new();
    for msg in &messages[initial_len..] {
        if msg.role == "assistant" {
            if let Some(tcs) = &msg.tool_calls {
                for tc in tcs {
                    call_id_to_tool.insert(tc.id.clone(), (tc.name.clone(), tc.arguments.clone()));
                }
            }
        }
    }

    // Find the boundary of protected recent rounds.
    let mut round_count = 0usize;
    let mut protect_from = messages.len();
    for i in (initial_len..messages.len()).rev() {
        if messages[i].role == "assistant" && messages[i].tool_calls.is_some() {
            round_count += 1;
            if round_count == SMART_TRIM_PROTECT_RECENT_ROUNDS {
                protect_from = i;
                break;
            }
        }
    }

    // Replace old tool outputs with info-summaries (within the agent loop
    // portion, but only before the protected tail).
    let mut summarized = 0usize;
    let placeholder = "[old tool output cleared by context microcompact; key metadata retained]";
    for msg in messages.iter_mut().take(protect_from).skip(initial_len) {
        if msg.role != "tool" {
            continue;
        }
        let content = match &msg.content {
            Some(MessageContent::Text(c)) if !c.is_empty() => c.clone(),
            Some(MessageContent::Blocks(blocks)) => {
                let text: String = blocks
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                if text.is_empty() {
                    continue;
                }
                text
            }
            _ => continue,
        };
        // Already summarized (by us or microcompact) — skip.
        if content == placeholder || content.starts_with('[') && content.contains("] `") {
            continue;
        }

        let call_id = msg.tool_call_id.as_deref().unwrap_or("");
        let (tool_name, tool_args) = call_id_to_tool
            .get(call_id)
            .cloned()
            .unwrap_or(("unknown".to_string(), String::new()));

        let summary = summarize_tool_output(&tool_name, &tool_args, &content);
        msg.content = Some(MessageContent::Text(summary));
        summarized += 1;
    }

    if summarized > 0 {
        tracing::info!(
            initial_len,
            summarized,
            total = messages.len(),
            loop_tokens,
            budget = SMART_TRIM_TOKEN_BUDGET,
            "Smart-trimmed old tool outputs in agent loop"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            content: Some(MessageContent::Text(content.to_string())),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            cache_control: None,
        }
    }

    #[test]
    fn test_trim_messages_under_budget() {
        let msgs = vec![
            make_msg("system", "sys"),
            make_msg("user", "hello"),
            make_msg("assistant", "hi"),
        ];
        let result = trim_messages(msgs, 10000, 5);
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn test_trim_messages_preserves_system() {
        let mut msgs = vec![make_msg("system", "system prompt")];
        for i in 0..20 {
            msgs.push(make_msg(
                "user",
                &format!("message {} with some content to make it longer", i),
            ));
        }
        let result = trim_messages(msgs, 50, 2);
        assert_eq!(result[0].role, "system");
        assert!(result.len() < 21);
    }

    #[test]
    fn test_trim_messages_preserves_recent() {
        let mut msgs = vec![make_msg("system", "sys")];
        for i in 0..20 {
            msgs.push(make_msg(
                "user",
                &format!("message number {} with padding content here", i),
            ));
        }
        let result = trim_messages(msgs, 50, 3);
        // Last 3 messages should be preserved
        assert_eq!(
            result.last().unwrap().text_content(),
            Some("message number 19 with padding content here")
        );
    }

    fn make_tool_call_msg_with_args(id: &str, name: &str, args: &str) -> ChatMessage {
        ChatMessage {
            role: "assistant".to_string(),
            content: Some(MessageContent::Text(format!("calling {}", name))),
            tool_calls: Some(vec![ToolCall {
                id: id.to_string(),
                name: name.to_string(),
                arguments: args.to_string(),
            }]),
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            cache_control: None,
        }
    }

    fn make_tool_call_msg(id: &str, name: &str) -> ChatMessage {
        make_tool_call_msg_with_args(id, name, "{}")
    }

    fn make_tool_result_msg(id: &str, output: &str) -> ChatMessage {
        ChatMessage {
            role: "tool".to_string(),
            content: Some(MessageContent::Text(output.to_string())),
            tool_calls: None,
            tool_call_id: Some(id.to_string()),
            name: None,
            reasoning_content: None,
            cache_control: None,
        }
    }

    #[test]
    fn test_smart_trim_noop_when_under_budget() {
        // Small messages — under token budget, nothing should change.
        let initial_len = 2;
        let mut msgs = vec![make_msg("system", "sys"), make_msg("user", "hi")];
        for i in 0..3 {
            msgs.push(make_tool_call_msg(&format!("call_{}", i), "bash"));
            msgs.push(make_tool_result_msg(&format!("call_{}", i), "short output"));
        }
        let len_before = msgs.len();
        smart_trim_tool_loop(&mut msgs, initial_len);
        assert_eq!(msgs.len(), len_before);
        // Content should be unchanged
        assert_eq!(msgs[3].text_content(), Some("short output"));
    }

    #[test]
    fn test_smart_trim_replaces_old_tool_outputs() {
        // Create enough large tool outputs to exceed the token budget.
        let initial_len = 2;
        let mut msgs = vec![make_msg("system", "sys"), make_msg("user", "hi")];
        let big_output: String = "x".repeat(40_000); // ~10K tokens each
        for i in 0..12 {
            msgs.push(make_tool_call_msg_with_args(
                &format!("call_{}", i),
                "bash",
                &format!(r#"{{"command": "npm run test_{}"}}"#, i),
            ));
            msgs.push(make_tool_result_msg(
                &format!("call_{}", i),
                &format!(
                    "<stdout>\n{}\n</stdout>\n<return_code>\n0\n</return_code>",
                    big_output
                ),
            ));
        }
        // Total loop tokens ≈ 12 * 10K = 120K > 60K budget

        smart_trim_tool_loop(&mut msgs, initial_len);

        // Message count should not change (no deletion)
        assert_eq!(msgs.len(), 2 + 12 * 2);

        // Prefix must be preserved
        assert_eq!(msgs[0].text_content(), Some("sys"));
        assert_eq!(msgs[1].text_content(), Some("hi"));

        // Recent rounds (last 6) should still have their full content
        let last_tool = msgs.last().unwrap();
        assert!(last_tool.text_content().unwrap().contains(&big_output));

        // Old rounds should be summarized (not the raw big output)
        // Round 0 tool result is at index 3
        let old_tool = &msgs[3];
        let content = old_tool.text_content().unwrap();
        assert!(content.starts_with("[bash]"));
        assert!(content.contains("exit 0"));
        assert!(!content.contains(&big_output));
    }

    #[test]
    fn test_smart_trim_preserves_tool_call_structure() {
        let initial_len = 1;
        let mut msgs = vec![make_msg("system", "sys")];
        let big_output: String = "y".repeat(40_000);
        for i in 0..10 {
            msgs.push(make_tool_call_msg(&format!("c{}", i), "read_file"));
            msgs.push(make_tool_result_msg(&format!("c{}", i), &big_output));
        }

        smart_trim_tool_loop(&mut msgs, initial_len);

        // All assistant messages should still have tool_calls
        for msg in &msgs {
            if msg.role == "assistant" {
                assert!(msg.tool_calls.is_some());
            }
        }
    }

    #[test]
    fn test_smart_trim_preserves_prefix_entirely() {
        let initial_len = 5;
        let mut msgs = vec![
            make_msg("system", "sys"),
            make_msg("system", "env"),
            make_msg("system", "skills"),
            make_msg("user", "previous question"),
            make_msg("assistant", "previous answer"),
        ];
        let big_output: String = "z".repeat(40_000);
        for i in 0..10 {
            msgs.push(make_tool_call_msg(&format!("c{}", i), "bash"));
            msgs.push(make_tool_result_msg(&format!("c{}", i), &big_output));
        }

        smart_trim_tool_loop(&mut msgs, initial_len);

        assert_eq!(msgs[0].text_content(), Some("sys"));
        assert_eq!(msgs[4].text_content(), Some("previous answer"));
    }

    #[test]
    fn test_summarize_bash_tool_output() {
        let args = r#"{"command": "npm test"}"#;
        let output = "<stdout>\nOK\n</stdout>\n<return_code>\n0\n</return_code>";
        let summary = summarize_tool_output("bash", args, output);
        assert!(summary.contains("[bash]"));
        assert!(summary.contains("`npm test`"));
        assert!(summary.contains("exit 0"));
    }

    #[test]
    fn test_summarize_read_file_output() {
        let args = r#"{"path": "/src/main.rs", "offset": 10}"#;
        let output = "fn main() { ... }";
        let summary = summarize_tool_output("read_file", args, output);
        assert!(summary.contains("[read_file]"));
        assert!(summary.contains("/src/main.rs"));
        assert!(summary.contains("line 10"));
    }

    #[test]
    fn test_summarize_grep_output() {
        let args = r#"{"pattern": "TODO", "path": "src/"}"#;
        let output = "file1.rs:1: TODO fix\nfile2.rs:5: TODO refactor";
        let summary = summarize_tool_output("grep", args, output);
        assert!(summary.contains("[grep]"));
        assert!(summary.contains("TODO"));
        assert!(summary.contains("src/"));
    }

    #[test]
    fn test_summarize_unknown_tool() {
        let args = r#"{"key": "value"}"#;
        let output = "some result";
        let summary = summarize_tool_output("custom_tool", args, output);
        assert!(summary.contains("[custom_tool]"));
        assert!(summary.contains("11 chars"));
    }

    #[test]
    fn test_microcompact_chat_messages_clears_old_tool_output() {
        let policy = ContextBudgetPolicy {
            micro_keep_recent_messages: 1,
            enable_token_estimation: false,
            ..ContextBudgetPolicy::default()
        };
        let mut msgs = vec![
            make_msg("system", "sys"),
            ChatMessage::tool_result(
                "call-old",
                "<stdout>very noisy output</stdout>\n<return_code>0</return_code>",
            ),
            ChatMessage::tool_result("call-new", "<stdout>recent output</stdout>"),
        ];

        let report = microcompact_chat_messages(&mut msgs, &policy);
        assert_eq!(report.changed_messages, 1);
        assert!(msgs[1]
            .text_content()
            .unwrap()
            .contains("old tool output cleared"));
        assert!(msgs[2].text_content().unwrap().contains("recent output"));
    }

    #[test]
    fn test_full_compact_chat_messages_adds_summary_and_preserves_recent() {
        let policy = ContextBudgetPolicy {
            micro_keep_recent_messages: 2,
            summary_max_tokens: 200,
            ..ContextBudgetPolicy::default()
        };
        let msgs = vec![
            make_msg("system", "sys"),
            make_msg("user", "old request"),
            make_msg("assistant", "old answer"),
            make_msg("user", "recent request"),
            make_msg("assistant", "recent answer"),
        ];

        let result = full_compact_chat_messages(msgs, &policy).unwrap();
        assert_eq!(result[0].role, "system");
        assert!(result.iter().any(|m| m
            .text_content()
            .unwrap_or("")
            .contains("conversation-summary")));
        assert_eq!(result.last().unwrap().text_content(), Some("recent answer"));
    }

    #[test]
    fn test_format_compact_summary_wraps_model_text() {
        let formatted = format_compact_summary(
            "<analysis>scratch</analysis><summary>Current Goal:\n- Diagnose nginx</summary>"
                .to_string(),
        );
        assert!(formatted.starts_with("<conversation-summary"));
        assert!(formatted.contains("Diagnose nginx"));
        assert!(!formatted.contains("scratch"));
    }

    #[tokio::test]
    async fn test_prepare_messages_for_send_triggers_microcompact_without_model() {
        let mut session = LlmSession::new("http://localhost", "key", "model", None, None);
        session.set_context_budget_policy(ContextBudgetPolicy {
            context_window_tokens: 2_000,
            reserved_output_tokens: 200,
            auto_compact_buffer_tokens: 200,
            warning_buffer_tokens: 200,
            blocking_buffer_tokens: 100,
            micro_keep_recent_messages: 2,
            full_compact_enabled: false,
            enable_token_estimation: false,
            ..ContextBudgetPolicy::default()
        });

        let mut msgs = vec![make_msg("system", "sys")];
        msgs.push(ChatMessage::tool_result(
            "call-old",
            format!(
                "<stdout>{}</stdout>\n<return_code>1</return_code>",
                "old noisy output\n".repeat(400)
            ),
        ));
        msgs.push(make_msg("user", "recent request"));
        msgs.push(make_msg("assistant", "recent answer"));

        let prepared = session.prepare_messages_for_send(msgs).await;
        assert!(prepared.iter().any(|m| m
            .text_content()
            .unwrap_or("")
            .contains("old tool output cleared")));
        assert_eq!(
            prepared.last().unwrap().text_content(),
            Some("recent answer")
        );
    }

    #[test]
    fn test_session_set_langfuse() {
        let mut session = LlmSession::new("http://localhost", "key", "model", None, None);
        let config = crate::langfuse::LangfuseConfig {
            enabled: true,
            public_key: "pk".into(),
            secret_key: "sk".into(),
            base_url: "http://localhost:3000".into(),
        };
        session.set_langfuse(crate::langfuse::LangfuseClient::new(config));
        assert!(session.langfuse.is_some());
    }

    #[test]
    fn test_create_subsession_shares_client() {
        let session = LlmSession::new(
            "https://api.openai.com/v1",
            "sk-test-key",
            "gpt-4o",
            Some(0.7),
            Some(4096),
        );
        let sub = session.create_subsession();
        // Subsession has empty tools
        assert!(sub.tool_specs().is_empty());
        // Subsession has independent cancellation (not cancelled)
        assert!(!sub.cancellation_token().is_cancelled());
    }

    #[test]
    fn test_create_subsession_inherits_tool_execution_policy() {
        let mut session = LlmSession::new("http://localhost", "key", "model", None, None);
        session.set_tool_execution_policy(crate::ToolExecutionPolicy {
            enforce_read_only_bash: true,
        });
        let sub = session.create_subsession();
        assert!(sub.tool_execution_policy().enforce_read_only_bash);
    }

    #[test]
    fn test_subsession_independent_cancellation() {
        let session = LlmSession::new("https://api.openai.com/v1", "sk-test", "gpt-4o", None, None);
        let sub = session.create_subsession();
        // Cancel parent
        session.cancellation_token().cancel();
        // Sub should NOT be cancelled
        assert!(session.cancellation_token().is_cancelled());
        assert!(!sub.cancellation_token().is_cancelled());
    }

    #[test]
    fn test_session_codex_api_key_uses_openai_responses() {
        let session = LlmSession::new(
            "https://api.openai.com/v1",
            "sk-test",
            "openai-codex/gpt-5.4",
            None,
            None,
        );
        assert_eq!(session.api_dialect(), ApiDialect::OpenAiResponses);
        assert_eq!(session.resolved_model_name(), "gpt-5.4");
    }

    #[test]
    fn test_session_codex_oauth_uses_chatgpt_responses() {
        let session = LlmSession::new(
            "https://chatgpt.com/backend-api/codex",
            "",
            "openai-codex/gpt-5.4",
            None,
            None,
        );
        assert_eq!(session.api_dialect(), ApiDialect::OpenAiChatgptResponses);
    }

    #[test]
    fn test_stream_context_getters() {
        let session = LlmSession::new(
            "https://api.example.com/v1",
            "sk-key123",
            "gpt-4o",
            None,
            None,
        );
        assert_eq!(session.model_name(), "gpt-4o");
        assert_eq!(session.resolved_model_name(), "gpt-4o");
        assert_eq!(session.api_dialect(), ApiDialect::OpenAiCompletions);
    }

    #[test]
    fn test_main_chat_tool_specs_normal_mode() {
        use crate::prompt::{PromptAssembly, PromptContext};

        let session = LlmSession::new("http://localhost", "key", "model", None, None);

        let specs = PromptAssembly::build(&session, PromptContext::MainChat, "").tool_specs;
        assert_eq!(specs.len(), 0);
    }

    #[test]
    fn test_main_chat_tool_specs_planning_mode() {
        use crate::prompt::{PromptAssembly, PromptContext};
        use aish_core::PlanPhase;

        let session = LlmSession::new("http://localhost", "key", "model", None, None);

        {
            let mut state = session.plan_state.lock().unwrap();
            state.phase = PlanPhase::Planning;
        }

        let specs = PromptAssembly::build(&session, PromptContext::MainChat, "").tool_specs;
        assert_eq!(specs.len(), 0);
    }

    #[test]
    fn test_plan_state_accessor() {
        let session = LlmSession::new("http://localhost", "key", "model", None, None);
        let state = session.plan_state();
        assert_eq!(state.lock().unwrap().phase, aish_core::PlanPhase::Normal);
    }

    #[test]
    fn test_session_update_model() {
        let mut session = LlmSession::new(
            "https://api.example.com/v1",
            "sk-test",
            "gpt-4",
            Some(0.7),
            Some(1000),
        );
        session.update_model("gpt-4o", None, None);
        assert_eq!(session.model_name(), "gpt-4o");
    }

    #[test]
    fn test_session_update_model_resolves_after_api_base_change() {
        let mut session = LlmSession::new(
            "https://openrouter.ai/api/v1",
            "sk-test",
            "openai/gpt-4o",
            None,
            None,
        );
        assert_eq!(session.model_name(), "openai/gpt-4o");

        session.update_model("openai/gpt-4o", Some("https://api.openai.com/v1"), None);
        assert_eq!(session.model_name(), "gpt-4o");
    }

    #[test]
    fn test_subsession_independent_plan_state() {
        let session = LlmSession::new("http://localhost", "key", "model", None, None);

        // Modify parent plan state
        {
            let mut state = session.plan_state.lock().unwrap();
            state.phase = aish_core::PlanPhase::Planning;
        }

        let sub = session.create_subsession();

        // Subsession should have independent plan state in Normal mode
        assert_eq!(
            sub.plan_state().lock().unwrap().phase,
            aish_core::PlanPhase::Normal
        );

        // Parent should still be in Planning mode
        assert_eq!(
            session.plan_state().lock().unwrap().phase,
            aish_core::PlanPhase::Planning
        );
    }

    // Mock tool for testing
    struct MockTool {
        name: String,
        prompt: String,
    }

    impl MockTool {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
                prompt: String::new(),
            }
        }

        fn with_prompt(name: &str, prompt: &str) -> Self {
            Self {
                name: name.to_string(),
                prompt: prompt.to_string(),
            }
        }
    }

    impl Tool for MockTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "Mock tool for testing"
        }

        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}})
        }

        fn prompt(&self) -> &str {
            &self.prompt
        }

        fn execute(&self, _args: serde_json::Value) -> crate::types::ToolResult {
            crate::types::ToolResult::success("mock result")
        }
    }

    #[test]
    fn test_tool_prompt_section_uses_only_non_empty_prompts() {
        use crate::prompt::{PromptAssembly, PromptContext};

        let mut session = LlmSession::new("http://localhost", "key", "model", None, None);
        session.register_tool(Box::new(MockTool::new("empty_tool")));
        session.register_tool(Box::new(MockTool::with_prompt(
            "prompt_tool",
            "Use carefully.",
        )));

        let section = PromptAssembly::build(&session, PromptContext::MainChat, "").system_message;

        assert!(section.contains("## Tool Instructions"));
        assert!(section.contains("### prompt_tool"));
        assert!(section.contains("Use carefully."));
        assert!(!section.contains("empty_tool"));
    }

    #[test]
    fn test_tool_prompt_section_respects_planning_filter() {
        use crate::prompt::{PromptAssembly, PromptContext};

        let mut session = LlmSession::new("http://localhost", "key", "model", None, None);
        session.register_tool(Box::new(MockTool::with_prompt(
            "read_file",
            "Read files during planning.",
        )));
        session.register_tool(Box::new(MockTool::with_prompt(
            "bash_exec",
            "Run commands.",
        )));

        {
            let mut state = session.plan_state.lock().unwrap();
            state.phase = aish_core::PlanPhase::Planning;
        }

        let section = PromptAssembly::build(&session, PromptContext::MainChat, "").system_message;

        assert!(section.contains("### read_file"));
        assert!(section.contains("Read files during planning."));
        assert!(!section.contains("bash_exec"));
        assert!(!section.contains("Run commands."));
    }

    #[test]
    fn test_prompt_assembly_appends_tool_section_to_base_system() {
        use crate::prompt::{PromptAssembly, PromptContext};

        let mut session = LlmSession::new("http://localhost", "key", "model", None, None);
        session.register_tool(Box::new(MockTool::with_prompt("mock", "Mock guidance.")));

        let system_prompt =
            PromptAssembly::build(&session, PromptContext::MainChat, "Base prompt.\n")
                .system_message;

        assert!(system_prompt.starts_with("Base prompt."));
        assert!(system_prompt.contains("## Tool Instructions"));
        assert!(system_prompt.contains("### mock"));
        assert!(system_prompt.contains("Mock guidance."));
    }

    #[test]
    fn test_tool_filtering_with_registered_tools() {
        use crate::prompt::{PromptAssembly, PromptContext};
        use aish_core::PlanPhase;

        let mut session = LlmSession::new("http://localhost", "key", "model", None, None);

        session.register_tool(Box::new(MockTool::new("read_file")));
        session.register_tool(Box::new(MockTool::new("bash_exec")));
        session.register_tool(Box::new(MockTool::new("grep")));

        let specs = PromptAssembly::build(&session, PromptContext::MainChat, "").tool_specs;
        assert_eq!(specs.len(), 3);

        {
            let mut state = session.plan_state.lock().unwrap();
            state.phase = PlanPhase::Planning;
        }

        let specs = PromptAssembly::build(&session, PromptContext::MainChat, "").tool_specs;
        assert_eq!(specs.len(), 2);
        let tool_names: Vec<_> = specs.iter().map(|s| s.function.name.as_str()).collect();
        assert!(tool_names.contains(&"read_file"));
        assert!(tool_names.contains(&"grep"));
        assert!(!tool_names.contains(&"bash_exec"));
    }

    #[tokio::test]
    async fn tool_execution_end_event_includes_tool_args() {
        let mut session = LlmSession::new("http://localhost", "key", "model", None, None);
        session.register_tool(Box::new(MockTool::new("bash")));

        let seen_event = std::sync::Arc::new(std::sync::Mutex::new(None));
        let seen_event_cb = seen_event.clone();
        session.set_event_callback(std::sync::Arc::new(move |event| {
            if matches!(event.event_type, aish_core::LlmEventType::ToolExecutionEnd) {
                *seen_event_cb.lock().unwrap() = Some(event);
            }
            None
        }));

        let tool_call = ToolCall {
            id: "call_1".into(),
            name: "bash".into(),
            arguments: serde_json::json!({ "command": "sudo ls /root" }).to_string(),
        };

        let result = session.execute_tool_external(&tool_call).await;
        assert!(result.ok);

        let event = seen_event
            .lock()
            .unwrap()
            .clone()
            .expect("missing ToolExecutionEnd event");
        assert_eq!(
            event.data["tool_args"]["command"].as_str(),
            Some("sudo ls /root")
        );
    }
}
