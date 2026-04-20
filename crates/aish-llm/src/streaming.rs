use crate::types::ToolCall;
use crate::usage::TokenUsage;

/// Events emitted while parsing an SSE stream from the LLM.
#[derive(Debug, Clone)]
pub enum SseEvent {
    ContentDelta(String),
    ReasoningDelta(String),
    ToolCallDelta {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments: Option<String>,
    },
    Finish(String),
    Done,
}

/// Parser for SSE (Server-Sent Events) responses from OpenAI-compatible APIs.
pub struct StreamParser;

impl StreamParser {
    /// Parse a non-streaming JSON response into final content and tool calls.
    pub fn parse_response(
        response: &serde_json::Value,
    ) -> (Option<String>, Vec<ToolCall>, Option<TokenUsage>) {
        let choices = response.get("choices").and_then(|c| c.as_array());
        if let Some(choices) = choices {
            if let Some(choice) = choices.first() {
                let message = choice.get("message");
                let content = message
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_str())
                    .map(|s| s.to_string());
                let tool_calls = Self::parse_tool_calls_from_message(message);
                let usage = TokenUsage::from_response_json(response);
                let has_usage = usage.prompt_tokens > 0 || usage.completion_tokens > 0;
                return (
                    content,
                    tool_calls,
                    if has_usage { Some(usage) } else { None },
                );
            }
        }
        (None, Vec::new(), None)
    }

    fn parse_tool_calls_from_message(message: Option<&serde_json::Value>) -> Vec<ToolCall> {
        let mut calls = Vec::new();
        if let Some(msg) = message {
            if let Some(tcs) = msg.get("tool_calls").and_then(|t| t.as_array()) {
                for tc in tcs {
                    if let (Some(id), Some(name), Some(args)) = (
                        tc.get("id").and_then(|v| v.as_str()),
                        tc.get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|n| n.as_str()),
                        tc.get("function")
                            .and_then(|f| f.get("arguments"))
                            .and_then(|a| a.as_str()),
                    ) {
                        calls.push(ToolCall {
                            id: id.into(),
                            name: name.into(),
                            arguments: args.into(),
                        });
                    }
                }
            }
        }
        calls
    }

    /// Parse a single SSE chunk line and extract structured events.
    ///
    /// SSE format: `"data: {json}\n\n"` or `"data: [DONE]\n\n"`.
    /// Returns a Vec because a single chunk may contain multiple tool call deltas.
    pub fn parse_sse_chunk(line: &str) -> (Vec<SseEvent>, Option<TokenUsage>) {
        let line = line.trim();
        if !line.starts_with("data: ") {
            return (Vec::new(), None);
        }
        let data = &line[6..];
        if data == "[DONE]" {
            return (vec![SseEvent::Done], None);
        }

        let json = match serde_json::from_str::<serde_json::Value>(data) {
            Ok(v) => v,
            Err(_) => return (Vec::new(), None),
        };

        // Check for usage data in the top-level response (present in final streaming chunk)
        let mut extracted_usage = None;
        if let Some(choice) = json
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
        {
            if choice.get("finish_reason").is_some() {
                let usage = TokenUsage::from_response_json(&json);
                if usage.prompt_tokens > 0 || usage.completion_tokens > 0 {
                    extracted_usage = Some(usage);
                }
            }
        }

        let choices = match json.get("choices").and_then(|c| c.as_array()) {
            Some(c) => c,
            None => return (Vec::new(), None),
        };
        let choice = match choices.first() {
            Some(c) => c,
            None => return (Vec::new(), None),
        };

        let delta = choice.get("delta");
        let mut events = Vec::new();

        // Content delta
        if let Some(content) = delta
            .and_then(|d| d.get("content"))
            .and_then(|c| c.as_str())
        {
            if !content.is_empty() {
                events.push(SseEvent::ContentDelta(content.to_string()));
                return (events, None);
            }
        }

        // Reasoning delta
        if let Some(reasoning) = delta
            .and_then(|d| d.get("reasoning_content"))
            .and_then(|c| c.as_str())
        {
            if !reasoning.is_empty() {
                events.push(SseEvent::ReasoningDelta(reasoning.to_string()));
                return (events, None);
            }
        }

        // Tool call deltas — process ALL tool calls in the array
        if let Some(tcs) = delta
            .and_then(|d| d.get("tool_calls"))
            .and_then(|t| t.as_array())
        {
            for tc in tcs {
                let index = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                let id = tc.get("id").and_then(|i| i.as_str()).map(|s| s.to_string());
                let name = tc
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(|n| n.as_str())
                    .map(|s| s.to_string());
                let args = tc
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(|a| a.as_str())
                    .map(|s| s.to_string());
                events.push(SseEvent::ToolCallDelta {
                    index,
                    id,
                    name,
                    arguments: args,
                });
            }
            if !events.is_empty() {
                return (events, None);
            }
        }

        // Finish reason
        if let Some(reason) = choice.get("finish_reason").and_then(|r| r.as_str()) {
            events.push(SseEvent::Finish(reason.to_string()));
            return (events, extracted_usage);
        }

        (Vec::new(), None)
    }
}
