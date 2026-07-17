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
    /// Parse a non-streaming JSON response into final content, reasoning content,
    /// and tool calls.
    pub fn parse_response(
        response: &serde_json::Value,
    ) -> (
        Option<String>,
        Option<String>,
        Vec<ToolCall>,
        Option<TokenUsage>,
    ) {
        let choices = response.get("choices").and_then(|c| c.as_array());
        if let Some(choices) = choices {
            if let Some(choice) = choices.first() {
                let message = choice.get("message");
                let content = extract_message_text(message.and_then(|m| m.get("content")));
                let reasoning_content = message
                    .and_then(|m| m.get("reasoning_content"))
                    .and_then(|c| c.as_str())
                    .map(|s| s.to_string());
                let tool_calls = Self::parse_tool_calls_from_message(message);
                let usage = TokenUsage::from_response_json(response);
                let has_usage = usage.prompt_tokens > 0 || usage.completion_tokens > 0;
                return (
                    content,
                    reasoning_content,
                    tool_calls,
                    if has_usage { Some(usage) } else { None },
                );
            }
        }
        (None, None, Vec::new(), None)
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
        // Extract usage from top-level "usage" field if present.
        // With stream_options.include_usage=true, the final chunk carries
        // usage alongside (or instead of) choices.
        let mut extracted_usage = None;
        let top_usage = TokenUsage::from_response_json(&json);
        if top_usage.prompt_tokens > 0 || top_usage.completion_tokens > 0 {
            extracted_usage = Some(top_usage);
        }

        let choices = match json.get("choices").and_then(|c| c.as_array()) {
            Some(c) if !c.is_empty() => c,
            // Empty or missing choices — return usage if we found it
            // (APIs may send a usage-only final chunk).
            _ => return (Vec::new(), extracted_usage),
        };
        let choice = match choices.first() {
            Some(c) => c,
            None => return (Vec::new(), extracted_usage),
        };

        let delta = choice.get("delta");
        let mut events = Vec::new();

        // Content delta
        if let Some(content) = extract_message_text(delta.and_then(|d| d.get("content"))) {
            if !content.is_empty() {
                events.push(SseEvent::ContentDelta(content.to_string()));
                return (events, extracted_usage);
            }
        }

        // Reasoning delta
        if let Some(reasoning) = delta
            .and_then(|d| d.get("reasoning_content"))
            .and_then(|c| c.as_str())
        {
            if !reasoning.is_empty() {
                events.push(SseEvent::ReasoningDelta(reasoning.to_string()));
                return (events, extracted_usage);
            }
        }

        // Tool call deltas — process ALL tool calls in the array
        if let Some(tcs) = delta
            .and_then(|d| d.get("tool_calls"))
            .and_then(|t| t.as_array())
        {
            for tc in tcs {
                let index = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                // Some providers send tool-call id as a number instead of a string.
                let id = tc.get("id").and_then(|i| match i {
                    serde_json::Value::String(s) => Some(s.clone()),
                    serde_json::Value::Number(n) => Some(n.to_string()),
                    _ => None,
                });
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
                return (events, extracted_usage);
            }
        }

        // Finish reason
        if let Some(reason) = choice.get("finish_reason").and_then(|r| r.as_str()) {
            events.push(SseEvent::Finish(reason.to_string()));
            return (events, extracted_usage);
        }

        (Vec::new(), extracted_usage)
    }
}

/// Extract text from content field, handling both plain string and content-blocks array.
/// Non-text blocks (e.g. image_url) are intentionally skipped — LLM responses don't
/// carry image data back.
pub fn extract_message_text(content: Option<&serde_json::Value>) -> Option<String> {
    match content {
        None => None,
        Some(serde_json::Value::String(text)) => {
            if text.is_empty() {
                None
            } else {
                Some(text.clone())
            }
        }
        Some(serde_json::Value::Array(items)) => {
            let parts: Vec<String> = items
                .iter()
                .filter_map(|item| match item {
                    serde_json::Value::String(text) => Some(text.clone()),
                    serde_json::Value::Object(obj) => obj
                        .get("text")
                        .or_else(|| obj.get("content"))
                        .and_then(|value| value.as_str())
                        .map(|text| text.to_string()),
                    _ => None,
                })
                .map(|part| part.trim().to_string())
                .filter(|part| !part.is_empty())
                .collect();

            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n"))
            }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{SseEvent, StreamParser};

    #[test]
    fn parse_response_accepts_array_content() {
        let response = serde_json::json!({
            "choices": [{
                "message": {
                    "content": [
                        {"type": "output_text", "text": "第一段"},
                        {"type": "output_text", "text": "第二段"}
                    ]
                }
            }]
        });

        let (content, reasoning, tool_calls, usage) = StreamParser::parse_response(&response);
        assert_eq!(content.as_deref(), Some("第一段\n第二段"));
        assert!(reasoning.is_none());
        assert!(tool_calls.is_empty());
        assert!(usage.is_none());
    }

    #[test]
    fn parse_sse_chunk_accepts_array_content_delta() {
        let line = r#"data: {"choices":[{"delta":{"content":[{"type":"output_text","text":"总结已生成"}]}}]}"#;

        let (events, usage) = StreamParser::parse_sse_chunk(line);
        assert!(usage.is_none());
        assert_eq!(events.len(), 1);
        match &events[0] {
            SseEvent::ContentDelta(text) => assert_eq!(text, "总结已生成"),
            other => panic!("unexpected event: {:?}", other),
        }
    }

    #[test]
    fn parse_sse_chunk_extracts_usage_from_empty_choices_final_chunk() {
        // Regression test: when stream_options.include_usage is true, OpenAI emits
        // a final chunk carrying `usage` alongside an EMPTY `choices: []` array.
        // The parser must surface the usage instead of dropping it with the
        // empty choices (the original bug returned None here).
        let line = r#"data: {"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":50,"total_tokens":150}}"#;

        let (events, usage) = StreamParser::parse_sse_chunk(line);
        // Empty choices produce no stream events.
        assert!(
            events.is_empty(),
            "expected no events for empty-choices chunk"
        );
        // ...but the usage must still be propagated.
        let usage = usage.expect("usage must be extracted from empty-choices final chunk");
        assert_eq!(usage.prompt_tokens, 100);
        assert_eq!(usage.completion_tokens, 50);
    }

    #[test]
    fn parse_sse_chunk_extracts_usage_alongside_finish_event() {
        // Usage may arrive on the same chunk that carries a finish_reason with
        // non-empty choices. Both the Finish event and the usage must be returned.
        let line = r#"data: {"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":200,"completion_tokens":80,"total_tokens":280}}"#;

        let (events, usage) = StreamParser::parse_sse_chunk(line);
        let usage = usage.expect("usage must be extracted from finish chunk");
        assert_eq!(usage.prompt_tokens, 200);
        assert_eq!(usage.completion_tokens, 80);

        // The finish event must still be emitted alongside the usage.
        assert_eq!(events.len(), 1);
        match &events[0] {
            SseEvent::Finish(reason) => assert_eq!(reason, "stop"),
            other => panic!("unexpected event: {:?}", other),
        }
    }

    #[test]
    fn parse_sse_chunk_returns_no_usage_for_content_delta() {
        // A mid-stream content delta carries no usage field; the parser must
        // report None so callers do not record phantom zero-token usage.
        let line = r#"data: {"choices":[{"index":0,"delta":{"content":"hello"}}]}"#;

        let (events, usage) = StreamParser::parse_sse_chunk(line);
        assert!(usage.is_none(), "content delta must not report usage");
        assert_eq!(events.len(), 1);
        match &events[0] {
            SseEvent::ContentDelta(text) => assert_eq!(text, "hello"),
            other => panic!("unexpected event: {:?}", other),
        }
    }

    #[test]
    fn parse_sse_chunk_done_marker_carries_no_usage() {
        // The terminal [DONE] sentinel never carries usage; None is expected.
        let line = "data: [DONE]";

        let (events, usage) = StreamParser::parse_sse_chunk(line);
        assert!(usage.is_none(), "[DONE] marker must not report usage");
        assert_eq!(events.len(), 1);
        match &events[0] {
            SseEvent::Done => {}
            other => panic!("unexpected event: {:?}", other),
        }
    }
}
