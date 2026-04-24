use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use aish_core::{AishError, LlmEvent, LlmEventType, PlanModeState, PlanPhase};

use crate::client::{LlmClient, LlmResponse};
use crate::langfuse::LangfuseClient;
use crate::streaming::{SseEvent, StreamParser};
use crate::types::*;

/// Main LLM session that orchestrates the chat loop with tool calling.
pub struct LlmSession {
    client: LlmClient,
    tools: HashMap<String, Box<dyn Tool>>,
    cancellation_token: CancellationToken,
    event_callback: Option<Arc<dyn Fn(LlmEvent) -> Option<LlmCallbackResult> + Send + Sync>>,
    confirmation_callback: Option<Arc<dyn Fn(&str, &str) -> bool + Send + Sync>>,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    langfuse: Option<LangfuseClient>,
    /// Maximum context token budget. Messages are trimmed when exceeded.
    max_context_tokens: usize,
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
            cancellation_token: CancellationToken::new(),
            event_callback: None,
            confirmation_callback: None,
            temperature,
            max_tokens,
            langfuse: None,
            max_context_tokens: 100_000,
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
    /// The callback receives (tool_name, message) and returns true to approve.
    pub fn set_confirmation_callback(&mut self, cb: Arc<dyn Fn(&str, &str) -> bool + Send + Sync>) {
        self.confirmation_callback = Some(cb);
    }

    pub fn cancellation_token(&self) -> &CancellationToken {
        &self.cancellation_token
    }

    /// Set the maximum context token budget for message trimming.
    pub fn set_max_context_tokens(&mut self, max: usize) {
        self.max_context_tokens = max;
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
    ) -> Result<String, AishError> {
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

        // Trim messages if they exceed the context token budget
        messages = trim_messages(messages, self.max_context_tokens, 5);

        let tool_specs = self.filtered_tool_specs();
        let has_tools = !tool_specs.is_empty();

        // Tool calling loop (max iterations to prevent infinite loops)
        let mut iterations = 0;
        let max_iterations = 20;

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
            iterations += 1;

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

            let response = self
                .client
                .chat_completion(
                    &messages,
                    if has_tools { Some(&tool_specs) } else { None },
                    stream,
                    self.temperature,
                    self.max_tokens,
                )
                .await?;

            match response {
                LlmResponse::Json(json) => {
                    let (content, tool_calls, usage) = StreamParser::parse_response(&json);
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
                        return Ok(content.unwrap_or_default());
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
                        messages.push(chat_msg);
                    }

                    // Execute each tool call and append results
                    for tc in &tool_calls {
                        let result = self.execute_tool(tc).await;
                        // Log tool call span to Langfuse
                        if let (Some(ref langfuse), Some(ref tid)) = (&self.langfuse, &trace_id) {
                            langfuse
                                .span_tool_call(tid, &tc.name, &tc.arguments, &result.output, 0)
                                .await;
                        }
                        messages.push(ChatMessage::tool_result(&tc.id, result.output));
                    }
                }

                LlmResponse::Stream(resp) => {
                    let mut accumulated = String::new();
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
                        return Ok(accumulated);
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
                        return Ok(accumulated);
                    }

                    // Add assistant message
                    let mut assistant_msg = ChatMessage::assistant("");
                    assistant_msg.content = if accumulated.is_empty() {
                        None
                    } else {
                        Some(accumulated)
                    };
                    assistant_msg.tool_calls = Some(tool_calls.clone());
                    messages.push(assistant_msg);

                    // Execute tools
                    for tc in &tool_calls {
                        let result = self.execute_tool(tc).await;
                        // Log tool call span to Langfuse
                        if let (Some(ref langfuse), Some(ref tid)) = (&self.langfuse, &trace_id) {
                            langfuse
                                .span_tool_call(tid, &tc.name, &tc.arguments, &result.output, 0)
                                .await;
                        }
                        messages.push(ChatMessage::tool_result(&tc.id, result.output));
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
                let (content, _, usage) = StreamParser::parse_response(&json);
                if let Some(u) = usage {
                    self.record_usage(u);
                }
                Ok(content.unwrap_or_default())
            }
            LlmResponse::Stream(_) => {
                // Delegate to process_input for streaming handling
                self.process_input(prompt, &[], system_message, true).await
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
                PreflightResult::Confirm { message } => {
                    let approved = if let Some(ref cb) = self.confirmation_callback {
                        cb(&tool_call.name, &message)
                    } else {
                        true // No callback = allow (backward compatible)
                    };
                    if !approved {
                        return ToolResult::error(format!("Tool execution denied: {}", message));
                    }
                }
                PreflightResult::Block { message } => {
                    return ToolResult::error(format!("Blocked by security policy: {}", message));
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
            cancellation_token: CancellationToken::new(),
            event_callback: self.event_callback.clone(),
            confirmation_callback: self.confirmation_callback.clone(),
            temperature: self.temperature,
            max_tokens: self.max_tokens,
            langfuse: self.langfuse.clone(),
            max_context_tokens: self.max_context_tokens,
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
}

/// Helper: current time as a UNIX timestamp in seconds (f64).
fn now_timestamp() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
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
                let len = m.content.as_ref().map(|c| c.len()).unwrap_or(0);
                len / 4
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
        let msg_tokens = msg.content.as_ref().map(|c| c.len() / 4).unwrap_or(0);
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
}
