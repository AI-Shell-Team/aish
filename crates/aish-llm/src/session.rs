use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use aish_context::{ContextBudgetPolicy, ContextMessage, ContextPressureLevel, MicrocompactReport};
use aish_core::{AishError, LlmEvent, LlmEventType, MemoryType, PlanModeState, PlanPhase};

use crate::client::{LlmClient, LlmResponse};
use crate::langfuse::LangfuseClient;
use crate::streaming::{SseEvent, StreamParser};
use crate::types::*;

fn is_short_circuit_result(result: &ToolResult) -> bool {
    result
        .meta
        .as_ref()
        .and_then(|meta| meta.get("dispatch_status"))
        .and_then(|value| value.as_str())
        .is_some_and(|status| status.eq_ignore_ascii_case("short_circuit"))
}

/// Maximum consecutive tool failures before pausing for user confirmation.
const MAX_CONSECUTIVE_FAILURES: usize = 3;

const COMPACT_SUMMARY_SYSTEM_PROMPT: &str = "You summarize old AI Shell context for a shell operations assistant. Respond with TEXT ONLY. Do not call tools. Preserve operational facts, commands, failures, important stderr, environment constraints, plan state, and pending user intent. Output a concise <conversation-summary> block with stable section headings.";

/// Main LLM session that orchestrates the chat loop with tool calling.
pub struct LlmSession {
    client: LlmClient,
    tools: HashMap<String, Box<dyn Tool>>,
    cancellation_token: Arc<CancellationToken>,
    event_callback: Option<Arc<dyn Fn(LlmEvent) -> Option<LlmCallbackResult> + Send + Sync>>,
    confirmation_callback: Option<Arc<dyn Fn(&PreflightSecurityContext) -> bool + Send + Sync>>,
    security_notice_callback: Option<Arc<dyn Fn(&PreflightSecurityContext) + Send + Sync>>,
    /// Callback invoked when the tool-call iteration limit is reached.
    /// Receives the current iteration count and returns true to reset and continue.
    iteration_limit_callback: Option<Arc<dyn Fn(u32) -> bool + Send + Sync>>,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    langfuse: Option<LangfuseClient>,
    /// Maximum context token budget. Messages are trimmed when exceeded.
    max_context_tokens: usize,
    context_budget_policy: ContextBudgetPolicy,
    compact_consecutive_failures: std::sync::Mutex<usize>,
    /// Plan mode state for dynamic tool filtering.
    plan_state: Arc<Mutex<PlanModeState>>,
    /// Cumulative token usage statistics for this session.
    token_stats: std::sync::Mutex<crate::usage::TokenStats>,
}

impl LlmSession {
    pub fn new(
        api_base: &str,
        api_key: &str,
        model: &str,
        temperature: Option<f32>,
        max_tokens: Option<u32>,
    ) -> Self {
        Self {
            client: LlmClient::new(api_base, api_key, model),
            tools: HashMap::new(),
            cancellation_token: Arc::new(CancellationToken::new()),
            event_callback: None,
            confirmation_callback: None,
            security_notice_callback: None,
            iteration_limit_callback: None,
            temperature,
            max_tokens,
            langfuse: None,
            max_context_tokens: 100_000,
            context_budget_policy: ContextBudgetPolicy::default(),
            compact_consecutive_failures: std::sync::Mutex::new(0),
            plan_state: Arc::new(Mutex::new(PlanModeState::default())),
            token_stats: std::sync::Mutex::new(crate::usage::TokenStats::default()),
        }
    }

    pub fn register_tool(&mut self, tool: Box<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    pub fn set_event_callback(
        &mut self,
        cb: Arc<dyn Fn(LlmEvent) -> Option<LlmCallbackResult> + Send + Sync>,
    ) {
        self.event_callback = Some(cb);
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

    pub fn cancellation_token(&self) -> &CancellationToken {
        &self.cancellation_token
    }

    /// Return a shared reference to the cancellation token, allowing tools
    /// and other components to monitor cancellation without borrowing self.
    pub fn cancellation_token_arc(&self) -> Arc<CancellationToken> {
        Arc::clone(&self.cancellation_token)
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

    /// Return tool specs filtered based on the current plan phase.
    ///
    /// During planning, only tools in PLANNING_VISIBLE_TOOLS are available.
    /// During normal mode, all tools are visible.
    pub fn filtered_tool_specs(&self) -> Vec<ToolSpec> {
        let all = self.tool_specs();
        let phase = self.plan_state.lock().unwrap().phase.clone();

        match phase {
            PlanPhase::Normal => all,
            PlanPhase::Planning => {
                let visible = aish_core::PLANNING_VISIBLE_TOOLS;
                all.into_iter()
                    .filter(|t| visible.contains(&t.function.name.as_str()))
                    .collect()
            }
        }
    }

    /// Get a reference to the plan state (for external coordination).
    pub fn plan_state(&self) -> Arc<Mutex<PlanModeState>> {
        Arc::clone(&self.plan_state)
    }

    /// Return a snapshot of cumulative token usage statistics.
    pub fn token_stats(&self) -> crate::usage::TokenStats {
        self.token_stats.lock().unwrap().clone()
    }

    /// Record token usage from an API response.
    fn record_usage(&self, usage: crate::usage::TokenUsage) {
        self.token_stats.lock().unwrap().record(usage);
    }

    /// Update the model, optionally also updating API base and key.
    pub fn update_model(&mut self, model: &str, api_base: Option<&str>, api_key: Option<&str>) {
        self.client.update_model(model);
        if let Some(base) = api_base {
            self.client.update_api_base(base);
        }
        if let Some(key) = api_key {
            self.client.update_api_key(key);
        }
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
        self.client
            .chat_completion(messages, tools, stream, temperature, max_tokens)
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
        match self.tools.get(name) {
            Some(tool) => Ok(tool.as_ref().execute_async(args).await),
            None => Err(format!("Unknown tool: {}", name)),
        }
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
        prompt: &str,
        context_messages: &[ChatMessage],
        system_message: Option<&str>,
        stream: bool,
    ) -> Result<crate::types::ProcessResult, AishError> {
        self.cancellation_token.reset();

        // Emit operation start event
        self.emit_event(LlmEvent {
            event_type: LlmEventType::OpStart,
            data: serde_json::json!({"prompt_length": prompt.len()}),
            timestamp: now_timestamp(),
            metadata: None,
        });

        // Start Langfuse trace if configured
        let trace_id = if let Some(ref langfuse) = self.langfuse {
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let id = langfuse
                .trace_session(
                    &format!("turn-{ts}"),
                    &serde_json::json!({"prompt_length": prompt.len()}),
                )
                .await;
            Some(id)
        } else {
            None
        };

        // Build initial message list
        let mut messages: Vec<ChatMessage> = Vec::new();
        if let Some(sys) = system_message {
            messages.push(ChatMessage::system(sys));
        }
        messages.extend_from_slice(context_messages);
        messages.push(ChatMessage::user(prompt));

        messages = self.prepare_messages_for_send(messages).await;
        let initial_len = messages.len();

        let tool_specs = self.filtered_tool_specs();
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
                .client
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

                    if tool_calls.is_empty() {
                        // Log generation span to Langfuse
                        if let (Some(ref langfuse), Some(ref tid)) = (&self.langfuse, &trace_id) {
                            langfuse
                                .span_generation(
                                    tid,
                                    self.client.model_name(),
                                    serde_json::json!(messages),
                                    content.as_deref().unwrap_or(""),
                                    pt,
                                    ct,
                                )
                                .await;
                        }
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
                        chat_msg.content = msg
                            .get("content")
                            .and_then(|c| c.as_str())
                            .map(|s| s.to_string());
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
                            let text = if self.security_notice_callback.is_some() {
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
                }

                LlmResponse::Stream(resp) => {
                    let mut accumulated = String::new();
                    let mut reasoning_accumulated = String::new();
                    let mut tool_calls_accum: HashMap<usize, (String, String, String)> =
                        HashMap::new(); // index -> (id, name, args)

                    let mut stream_done = false;
                    let mut text_buffer = String::new();
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
                                                        entry.0 = i;
                                                    }
                                                    if let Some(n) = name {
                                                        entry.1 = n;
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

                    // No tool calls — return accumulated content
                    if tool_calls_accum.is_empty() {
                        // Log generation span to Langfuse
                        if let (Some(ref langfuse), Some(ref tid)) = (&self.langfuse, &trace_id) {
                            langfuse
                                .span_generation(
                                    tid,
                                    self.client.model_name(),
                                    serde_json::json!(messages),
                                    &accumulated,
                                    stream_prompt_tokens,
                                    stream_completion_tokens,
                                )
                                .await;
                        }
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
                        .filter_map(|(seq_idx, (_, (id, name, args)))| {
                            if id.is_empty() || name.is_empty() {
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
                            "Dropping tool calls with missing id/name at indexes: {:?}",
                            missing_ids
                        );
                        // Emit error event for malformed tool calls
                        self.emit_event(LlmEvent {
                            event_type: LlmEventType::Error,
                            data: serde_json::json!({
                                "error_type": "stream_chunk_builder_error",
                                "error_message": format!(
                                    "tool_calls missing id/name at indexes: {:?}",
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
                        Some(accumulated)
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
                            let text = if self.security_notice_callback.is_some() {
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
            .client
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
                let result = self
                    .process_input(prompt, &[], system_message, true)
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
            // Run preflight check before execution
            match tool.preflight(&args) {
                PreflightResult::Allow => {}
                PreflightResult::Confirm { message, security } => {
                    let security = security.unwrap_or_else(|| {
                        PreflightSecurityContext::fallback(
                            tool_call.name.clone(),
                            None,
                            message.clone(),
                            SecurityPanelMode::Confirm,
                        )
                    });
                    let approved = if let Some(ref cb) = self.confirmation_callback {
                        cb(&security)
                    } else {
                        true // No callback = allow (backward compatible)
                    };
                    if !approved {
                        return ToolResult::error(format!("Tool execution denied: {}", message));
                    }
                }
                PreflightResult::Block { message, security } => {
                    let security = security.unwrap_or_else(|| {
                        PreflightSecurityContext::fallback(
                            tool_call.name.clone(),
                            None,
                            message.clone(),
                            SecurityPanelMode::Blocked,
                        )
                    });
                    if let Some(ref cb) = self.security_notice_callback {
                        cb(&security);
                    }
                    return ToolResult {
                        ok: false,
                        output: format!("Blocked by security policy: {}", message),
                        meta: Some(serde_json::json!({
                            "dispatch_status": "short_circuit",
                            "reason": "security_blocked"
                        })),
                    };
                }
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
                let first = tool.as_ref().execute_async(args.clone()).await;
                if first.ok {
                    first
                } else {
                    // Retry once — log the retry attempt
                    tracing::warn!(
                        "Tool '{}' failed, retrying once: {}",
                        tool_call.name,
                        first.output
                    );
                    let second = tool.as_ref().execute_async(args.clone()).await;
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
            let output_preview = if tool_call.name == "bash" {
                let s = &result.output;
                let limit = 512.min(s.len());
                // Find safe UTF-8 boundary
                let mut end = limit;
                while end > 0 && end < s.len() && !s.is_char_boundary(end) {
                    end -= 1;
                }
                Some(s[..end].to_string())
            } else {
                None
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

            self.emit_event(LlmEvent {
                event_type: LlmEventType::ToolExecutionEnd,
                data: event_data,
                timestamp: now_timestamp(),
                metadata: None,
            });

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
            client: LlmClient::new(
                self.client.api_base(),
                self.client.api_key(),
                self.client.model_name(),
            ),
            tools: HashMap::new(),
            cancellation_token: Arc::new(CancellationToken::new()),
            event_callback: self.event_callback.clone(),
            confirmation_callback: self.confirmation_callback.clone(),
            security_notice_callback: self.security_notice_callback.clone(),
            iteration_limit_callback: self.iteration_limit_callback.clone(),
            temperature: self.temperature,
            max_tokens: self.max_tokens,
            langfuse: self.langfuse.clone(),
            max_context_tokens: self.max_context_tokens,
            context_budget_policy: self.context_budget_policy.clone(),
            compact_consecutive_failures: std::sync::Mutex::new(0),
            plan_state: Arc::new(Mutex::new(PlanModeState::default())),
            token_stats: std::sync::Mutex::new(crate::usage::TokenStats::default()),
        }
    }

    /// Create a sub-session pre-configured for diagnosis with the given tools.
    ///
    /// This is a convenience method that creates a SubSession with diagnostic
    /// configuration and registers the provided tools.
    ///
    /// # Arguments
    /// * `tools` - Tools to register in the diagnostic sub-session
    ///
    /// # Returns
    /// A configured SubSession ready for diagnostic use
    pub fn create_diagnose_subsession(
        &self,
        tools: Vec<Box<dyn Tool>>,
    ) -> crate::subsession::SubSession {
        use crate::diagnose_agent;
        use crate::subsession::{SubSession, SubSessionConfig};

        let config = SubSessionConfig {
            max_context_messages: 30,
            max_iterations: 10,
            system_prompt: Some(diagnose_agent::build_diagnose_prompt()),
        };

        let mut sub = SubSession::new(self, config);
        for tool in tools {
            sub.inner.register_tool(tool);
        }
        sub
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

    async fn prepare_messages_for_send(&self, mut messages: Vec<ChatMessage>) -> Vec<ChatMessage> {
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
            .client
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
        let content = msg.content.as_deref().unwrap_or("");
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
    let content_len = message.content.as_ref().map(|c| c.len()).unwrap_or(0);
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
            if let Some(content) = &msg.content {
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
                    msg.content = Some(replacement);
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
        let content = msg.content.as_deref().unwrap_or("");
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
                let content_len = m.content.as_ref().map(|c| c.len()).unwrap_or(0);
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

    // Split: first message (system) + middle (to trim) + last N (to keep)
    let system = if messages[0].role == "system" {
        vec![messages[0].clone()]
    } else {
        vec![]
    };

    let system_count = system.len();
    let recent_start = messages.len().saturating_sub(preserve_recent);
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
            let content_len = msg.content.as_ref().map(|c| c.len()).unwrap_or(0);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            content: Some(content.to_string()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning_content: None,
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
            result.last().unwrap().content.as_deref(),
            Some("message number 19 with padding content here")
        );
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
            .content
            .as_deref()
            .unwrap()
            .contains("old tool output cleared"));
        assert!(msgs[2]
            .content
            .as_deref()
            .unwrap()
            .contains("recent output"));
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
            .content
            .as_deref()
            .unwrap_or("")
            .contains("conversation-summary")));
        assert_eq!(
            result.last().unwrap().content.as_deref(),
            Some("recent answer")
        );
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
            .content
            .as_deref()
            .unwrap_or("")
            .contains("old tool output cleared")));
        assert_eq!(
            prepared.last().unwrap().content.as_deref(),
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
    fn test_client_getters() {
        use crate::client::LlmClient;
        let client = LlmClient::new("https://api.example.com/v1", "sk-key123", "gpt-4o");
        assert_eq!(client.api_base(), "https://api.example.com/v1");
        assert_eq!(client.api_key(), "sk-key123");
        assert_eq!(client.model_name(), "gpt-4o");
    }

    #[test]
    fn test_filtered_tool_specs_normal_mode() {
        let session = LlmSession::new("http://localhost", "key", "model", None, None);

        // In normal mode, filtered specs should return all registered tools
        let specs = session.filtered_tool_specs();
        assert_eq!(specs.len(), 0); // No tools registered yet
    }

    #[test]
    fn test_filtered_tool_specs_planning_mode() {
        use aish_core::PlanPhase;
        let session = LlmSession::new("http://localhost", "key", "model", None, None);

        // Set planning mode
        {
            let mut state = session.plan_state.lock().unwrap();
            state.phase = PlanPhase::Planning;
        }

        // In planning mode, should return empty (no tools registered)
        let specs = session.filtered_tool_specs();
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
        // compile-time check that method exists
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
    }

    impl MockTool {
        fn new(name: &str) -> Self {
            Self {
                name: name.to_string(),
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

        fn execute(&self, _args: serde_json::Value) -> crate::types::ToolResult {
            crate::types::ToolResult::success("mock result")
        }
    }

    #[test]
    fn test_tool_filtering_with_registered_tools() {
        use aish_core::PlanPhase;
        let mut session = LlmSession::new("http://localhost", "key", "model", None, None);

        // Register mock tools
        session.register_tool(Box::new(MockTool::new("read_file")));
        session.register_tool(Box::new(MockTool::new("bash_exec")));
        session.register_tool(Box::new(MockTool::new("grep")));

        // In normal mode, all tools should be visible
        let specs = session.filtered_tool_specs();
        assert_eq!(specs.len(), 3);

        // Set planning mode
        {
            let mut state = session.plan_state.lock().unwrap();
            state.phase = PlanPhase::Planning;
        }

        // In planning mode, bash_exec should be filtered out
        let specs = session.filtered_tool_specs();
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
