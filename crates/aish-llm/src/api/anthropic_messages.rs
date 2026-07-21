//! Anthropic Messages API dialect (`/v1/messages`).

use aish_core::AishError;
use reqwest::Client;
use serde_json::{json, Value};

use crate::client::LlmResponse;
use crate::openai_sse_bridge::translate_anthropic_sse_stream;
use crate::types::{ChatMessage, ToolSpec};
use crate::usage::TokenUsage;

use super::{
    effective_max_tokens, format_http_error, is_retryable_network_err, is_retryable_status,
    retry_backoff_delay, StreamContext, MAX_HTTP_RETRIES,
};

pub fn resolve_anthropic_messages_url(base_url: &str) -> String {
    let normalized = base_url.trim().trim_end_matches('/');
    if normalized.is_empty() {
        return "https://api.anthropic.com/v1/messages".to_string();
    }
    if normalized.ends_with("/v1/messages") {
        normalized.to_string()
    } else if normalized.ends_with("/v1") {
        format!("{normalized}/messages")
    } else if normalized.ends_with("/messages") {
        normalized.to_string()
    } else {
        format!("{normalized}/v1/messages")
    }
}

pub async fn stream(
    ctx: &StreamContext,
    messages: &[ChatMessage],
    tools: Option<&[ToolSpec]>,
    stream: bool,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
) -> Result<LlmResponse, AishError> {
    let (system, anthropic_messages) = convert_messages(messages);
    let model = ctx.resolved_model();

    let mut body = json!({
        "model": model,
        "max_tokens": effective_max_tokens(max_tokens),
        "messages": anthropic_messages,
        "stream": stream,
    });

    if !system.is_empty() {
        body["system"] = json!(system);
    }
    if let Some(temp) = temperature {
        body["temperature"] = json!(temp);
    }
    if let Some(tool_specs) = tools {
        if !tool_specs.is_empty() {
            body["tools"] = json!(convert_tools(tool_specs));
            body["tool_choice"] = json!({"type": "auto"});
        }
    }

    let url = resolve_anthropic_messages_url(&ctx.api_base);
    let http = Client::new();

    // Retry transient failures (5xx, 429, connect/timeout errors) with
    // exponential backoff. Non-retryable status codes surface immediately.
    let mut last_err: Option<AishError> = None;
    for attempt in 0..=MAX_HTTP_RETRIES {
        if attempt > 0 {
            let delay = retry_backoff_delay(attempt);
            tracing::warn!(
                "Retrying Anthropic request (attempt {}/{}) after {:?}",
                attempt + 1,
                MAX_HTTP_RETRIES + 1,
                delay
            );
            tokio::time::sleep(delay).await;
        }

        let resp = match http
            .post(&url)
            .header("x-api-key", &ctx.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .timeout(std::time::Duration::from_secs(120))
            .json(&body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) if is_retryable_network_err(&e) => {
                last_err = Some(AishError::Llm(e.to_string()));
                continue;
            }
            Err(e) => return Err(AishError::Llm(e.to_string())),
        };

        let status = resp.status();
        if status.is_success() {
            if stream {
                return Ok(LlmResponse::Stream(translate_anthropic_sse_stream(resp)));
            }
            let json_body: Value = resp
                .json()
                .await
                .map_err(|e| AishError::Llm(e.to_string()))?;
            return Ok(LlmResponse::Json(anthropic_response_to_openai_json(
                &json_body,
            )));
        }

        let text = resp.text().await.unwrap_or_default();
        if is_retryable_status(status) && attempt < MAX_HTTP_RETRIES {
            last_err = Some(AishError::Llm(format_http_error(status, &text)));
            continue;
        }
        return Err(AishError::Llm(format_http_error(status, &text)));
    }

    Err(last_err.unwrap_or_else(|| AishError::Llm("Request failed after retries".into())))
}

pub async fn test_connection(ctx: &StreamContext) -> Result<(), String> {
    let url = resolve_anthropic_messages_url(&ctx.api_base);
    let body = json!({
        "model": ctx.resolved_model(),
        "max_tokens": 16,
        "messages": [{"role": "user", "content": [{"type": "text", "text": "Hi"}]}],
    });

    let resp = Client::new()
        .post(&url)
        .header("x-api-key", &ctx.api_key)
        .header("anthropic-version", "2023-06-01")
        .header("Content-Type", "application/json")
        .timeout(std::time::Duration::from_secs(15))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Connection failed: {e}"))?;

    let status = resp.status();
    if status.is_success() {
        Ok(())
    } else {
        let text = resp.text().await.unwrap_or_default();
        Err(format!("Server returned status {}: {}", status, text))
    }
}

fn normalize_tool_call_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect()
}

fn convert_messages(messages: &[ChatMessage]) -> (Vec<Value>, Vec<Value>) {
    let mut system_blocks: Vec<Value> = Vec::new();
    let mut params: Vec<Value> = Vec::new();
    let mut i = 0;

    while i < messages.len() {
        let msg = &messages[i];
        match msg.role.as_str() {
            "system" => {
                if let Some(text) = msg.content.as_ref().and_then(|c| c.to_text()) {
                    if !text.trim().is_empty() {
                        system_blocks.push(json!({"type": "text", "text": text}));
                    }
                }
                i += 1;
            }
            "user" => {
                if let Some(text) = msg.content.as_ref().and_then(|c| c.to_text()) {
                    if !text.trim().is_empty() {
                        params.push(json!({
                            "role": "user",
                            "content": [{"type": "text", "text": text}],
                        }));
                    }
                }
                i += 1;
            }
            "assistant" => {
                let mut blocks: Vec<Value> = Vec::new();
                if let Some(text) = msg.content.as_ref().and_then(|c| c.to_text()) {
                    if !text.trim().is_empty() {
                        blocks.push(json!({"type": "text", "text": text}));
                    }
                }
                if let Some(tool_calls) = &msg.tool_calls {
                    for tc in tool_calls {
                        let args: Value = serde_json::from_str(&tc.arguments).unwrap_or(json!({}));
                        blocks.push(json!({
                            "type": "tool_use",
                            "id": normalize_tool_call_id(&tc.id),
                            "name": tc.name,
                            "input": args,
                        }));
                    }
                }
                if !blocks.is_empty() {
                    params.push(json!({"role": "assistant", "content": blocks}));
                }
                i += 1;
            }
            "tool" => {
                let mut tool_results: Vec<Value> = Vec::new();
                while i < messages.len() && messages[i].role == "tool" {
                    let tool_msg = &messages[i];
                    let tool_use_id = tool_msg
                        .tool_call_id
                        .as_deref()
                        .map(normalize_tool_call_id)
                        .unwrap_or_default();
                    if !tool_use_id.is_empty() {
                        let content = tool_msg
                            .content
                            .as_ref()
                            .and_then(|c| c.to_text())
                            .unwrap_or_default();
                        tool_results.push(json!({
                            "type": "tool_result",
                            "tool_use_id": tool_use_id,
                            "content": content,
                        }));
                    }
                    i += 1;
                }
                if !tool_results.is_empty() {
                    params.push(json!({"role": "user", "content": tool_results}));
                }
            }
            _ => {
                i += 1;
            }
        }
    }

    if params.is_empty() {
        params.push(json!({
            "role": "user",
            "content": [{"type": "text", "text": "."}],
        }));
    }

    (system_blocks, params)
}

fn convert_tools(tools: &[ToolSpec]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            json!({
                "name": tool.function.name,
                "description": tool.function.description,
                "input_schema": tool.function.parameters,
            })
        })
        .collect()
}

fn anthropic_response_to_openai_json(response: &Value) -> Value {
    let content_blocks = response
        .get("content")
        .and_then(|c| c.as_array())
        .cloned()
        .unwrap_or_default();

    let mut text_parts: Vec<String> = Vec::new();
    let mut tool_calls: Vec<Value> = Vec::new();

    for block in content_blocks {
        match block.get("type").and_then(|t| t.as_str()) {
            Some("text") => {
                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                    text_parts.push(text.to_string());
                }
            }
            Some("tool_use") => {
                let id = block
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let name = block
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let input = block.get("input").cloned().unwrap_or(json!({}));
                let args_str = serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_string());
                tool_calls.push(json!({
                    "id": id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": args_str,
                    }
                }));
            }
            _ => {}
        }
    }

    let stop_reason = response
        .get("stop_reason")
        .and_then(|v| v.as_str())
        .unwrap_or("end_turn");
    let finish_reason = if stop_reason == "tool_use" {
        "tool_calls"
    } else {
        "stop"
    };

    let mut message = json!({
        "role": "assistant",
        "content": text_parts.join(""),
    });
    if !tool_calls.is_empty() {
        message["tool_calls"] = json!(tool_calls);
    }

    let usage = TokenUsage::from_anthropic_json(response);

    json!({
        "choices": [{
            "message": message,
            "finish_reason": finish_reason,
        }],
        "usage": {
            "prompt_tokens": usage.prompt_tokens,
            "completion_tokens": usage.completion_tokens,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ChatMessage, MessageContent, ToolCall};

    #[test]
    fn test_resolve_anthropic_messages_url() {
        assert_eq!(
            resolve_anthropic_messages_url("https://api.anthropic.com"),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            resolve_anthropic_messages_url("https://api.anthropic.com/v1"),
            "https://api.anthropic.com/v1/messages"
        );
    }

    #[test]
    fn test_convert_system_and_tool_result() {
        let messages = vec![
            ChatMessage::system("You are helpful"),
            ChatMessage::user("Run ls"),
            ChatMessage {
                role: "assistant".into(),
                content: Some(MessageContent::Text("ok".into())),
                tool_calls: Some(vec![ToolCall {
                    id: "call_1".into(),
                    name: "bash".into(),
                    arguments: r#"{"command":"ls"}"#.into(),
                }]),
                tool_call_id: None,
                name: None,
                reasoning_content: None,
                cache_control: None,
            },
            ChatMessage::tool_result("call_1", "file.txt"),
        ];
        let (system, params) = convert_messages(&messages);
        assert_eq!(system.len(), 1);
        assert_eq!(params.len(), 3);
        assert_eq!(params[2]["role"], "user");
        assert_eq!(params[2]["content"][0]["type"], "tool_result");
    }

    #[test]
    fn test_normalize_tool_call_id() {
        assert_eq!(normalize_tool_call_id("call/abc!"), "call_abc_");
    }

    #[test]
    fn test_anthropic_response_to_openai_json() {
        let response = json!({
            "content": [
                {"type": "text", "text": "Hello"},
                {"type": "tool_use", "id": "t1", "name": "bash", "input": {"command": "ls"}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 10, "output_tokens": 5}
        });
        let openai = anthropic_response_to_openai_json(&response);
        assert_eq!(openai["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(openai["usage"]["prompt_tokens"], 10);
    }
}
