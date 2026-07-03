use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use aish_core::AishError;
use bytes::Bytes;
use reqwest::Response;
use serde_json::{json, Value};

use crate::llm_stream::LlmStream;
use crate::providers::codex::{extract_stream_failure, parse_sse_text};

fn openai_content_delta(text: &str) -> String {
    let payload = json!({
        "choices": [{
            "index": 0,
            "delta": {"content": text},
            "finish_reason": null,
        }]
    });
    format!("data: {payload}\n\n")
}

fn openai_tool_call_delta(
    index: usize,
    id: Option<&str>,
    name: Option<&str>,
    arguments: Option<&str>,
) -> String {
    let mut tool_call = json!({"index": index, "type": "function"});
    if let Some(id) = id.filter(|s| !s.is_empty()) {
        tool_call["id"] = json!(id);
    }
    let mut function = serde_json::Map::new();
    if let Some(name) = name.filter(|s| !s.is_empty()) {
        function.insert("name".to_string(), json!(name));
    }
    if let Some(args) = arguments.filter(|s| !s.is_empty()) {
        function.insert("arguments".to_string(), json!(args));
    }
    if !function.is_empty() {
        tool_call["function"] = Value::Object(function);
    }
    let payload = json!({
        "choices": [{
            "index": 0,
            "delta": {"tool_calls": [tool_call]},
            "finish_reason": null,
        }]
    });
    format!("data: {payload}\n\n")
}

fn openai_finish_chunk(finish_reason: &str, usage: Option<(u64, u64)>) -> String {
    let mut payload = json!({
        "choices": [{
            "index": 0,
            "delta": {},
            "finish_reason": finish_reason,
        }]
    });
    if let Some((prompt, completion)) = usage {
        payload["usage"] = json!({
            "prompt_tokens": prompt,
            "completion_tokens": completion,
        });
    }
    format!("data: {payload}\n\n")
}

const OPENAI_SSE_DONE: &str = "data: [DONE]\n\n";

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn find_sse_block_delimiter(buf: &[u8]) -> Option<(usize, usize)> {
    find_subsequence(buf, b"\r\n\r\n")
        .map(|pos| (pos, 4))
        .or_else(|| find_subsequence(buf, b"\n\n").map(|pos| (pos, 2)))
}

fn decode_sse_block(bytes: &[u8]) -> Result<String, AishError> {
    std::str::from_utf8(bytes)
        .map(|s| s.to_string())
        .map_err(|e| AishError::Llm(format!("Invalid UTF-8 in SSE block: {e}")))
}

struct AnthropicSseTranslator {
    tool_calls: HashMap<usize, (String, String)>,
    stop_reason: Option<String>,
    usage: Option<(u64, u64)>,
    finished: bool,
}

impl AnthropicSseTranslator {
    fn new() -> Self {
        Self {
            tool_calls: HashMap::new(),
            stop_reason: None,
            usage: None,
            finished: false,
        }
    }

    fn finish_reason(&self) -> &'static str {
        match self.stop_reason.as_deref() {
            Some("tool_use") => "tool_calls",
            _ if !self.tool_calls.is_empty() => "tool_calls",
            _ => "stop",
        }
    }

    fn finish_chunks(&mut self) -> Vec<String> {
        if self.finished {
            return Vec::new();
        }
        self.finished = true;
        vec![
            openai_finish_chunk(self.finish_reason(), self.usage),
            OPENAI_SSE_DONE.to_string(),
        ]
    }

    fn process_data_line(&mut self, data: &str) -> Result<Vec<String>, AishError> {
        if data == "[DONE]" {
            return Ok(self.finish_chunks());
        }
        let Ok(event) = serde_json::from_str::<Value>(data) else {
            return Ok(Vec::new());
        };
        let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let mut out = Vec::new();

        match event_type {
            "content_block_start" => {
                if let Some(block) = event.get("content_block") {
                    if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                        let index =
                            event.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
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
                        self.tool_calls.insert(index, (id.clone(), name.clone()));
                        out.push(openai_tool_call_delta(
                            index,
                            Some(&id),
                            Some(&name),
                            Some(""),
                        ));
                    }
                }
            }
            "content_block_delta" => {
                if let Some(delta) = event.get("delta") {
                    match delta.get("type").and_then(|t| t.as_str()) {
                        Some("text_delta") => {
                            if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
                                if !text.is_empty() {
                                    out.push(openai_content_delta(text));
                                }
                            }
                        }
                        Some("input_json_delta") => {
                            let index =
                                event.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                            if let Some(partial) =
                                delta.get("partial_json").and_then(|v| v.as_str())
                            {
                                out.push(openai_tool_call_delta(index, None, None, Some(partial)));
                            }
                        }
                        _ => {}
                    }
                }
            }
            "message_delta" => {
                if let Some(delta) = event.get("delta") {
                    if let Some(reason) = delta.get("stop_reason").and_then(|v| v.as_str()) {
                        self.stop_reason = Some(reason.to_string());
                    }
                }
                if let Some(usage) = event.get("usage") {
                    self.usage = Some((
                        usage
                            .get("input_tokens")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0),
                        usage
                            .get("output_tokens")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0),
                    ));
                }
            }
            "message_start" => {
                if let Some(message) = event.get("message") {
                    if let Some(usage) = message.get("usage") {
                        self.usage = Some((
                            usage
                                .get("input_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0),
                            usage
                                .get("output_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0),
                        ));
                    }
                }
            }
            "message_stop" => {
                out.extend(self.finish_chunks());
            }
            _ => {}
        }

        Ok(out)
    }
}

struct CodexResponsesSseTranslator {
    tool_index: usize,
    usage: Option<(u64, u64)>,
    finished: bool,
}

impl CodexResponsesSseTranslator {
    fn new() -> Self {
        Self {
            tool_index: 0,
            usage: None,
            finished: false,
        }
    }

    fn process_event(
        &mut self,
        event_type: &str,
        payload: &Value,
    ) -> Result<Vec<String>, AishError> {
        let mut out = Vec::new();
        match event_type {
            "response.output_text.delta" => {
                if let Some(delta) = payload.get("delta").and_then(|v| v.as_str()) {
                    if !delta.is_empty() {
                        out.push(openai_content_delta(delta));
                    }
                }
            }
            "response.output_item.done" => {
                if let Some(item) = payload.get("item") {
                    let item_type = item
                        .get("type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_lowercase();
                    if item_type == "function_call" {
                        let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
                        let call_id = item
                            .get("call_id")
                            .or_else(|| item.get("id"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let arguments = item.get("arguments").cloned().unwrap_or(json!({}));
                        let args_str = match &arguments {
                            Value::String(s) => s.clone(),
                            other => {
                                serde_json::to_string(other).unwrap_or_else(|_| "{}".to_string())
                            }
                        };
                        if !name.is_empty() && !call_id.is_empty() {
                            let index = self.tool_index;
                            self.tool_index += 1;
                            out.push(openai_tool_call_delta(
                                index,
                                Some(call_id),
                                Some(name),
                                Some(&args_str),
                            ));
                        }
                    }
                }
            }
            "response.failed" => {
                return Err(AishError::Llm(extract_stream_failure(payload)));
            }
            "response.incomplete" => {
                let reason = payload
                    .get("response")
                    .and_then(|v| v.get("incomplete_details"))
                    .and_then(|v| v.get("reason"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                return Err(AishError::Llm(format!("Incomplete response: {reason}")));
            }
            "response.completed" => {
                if let Some(resp) = payload.get("response") {
                    if let Some(usage) = resp.get("usage") {
                        self.usage = Some((
                            usage
                                .get("input_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0),
                            usage
                                .get("output_tokens")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0),
                        ));
                    }
                }
                self.finished = true;
                out.push(openai_finish_chunk("stop", self.usage));
                out.push(OPENAI_SSE_DONE.to_string());
            }
            _ => {}
        }
        Ok(out)
    }

    fn finish(&mut self) -> Result<Vec<String>, AishError> {
        if self.finished {
            return Ok(Vec::new());
        }
        Err(AishError::Llm(
            "Stream ended before response.completed".to_string(),
        ))
    }
}

fn spawn_sse_translation<F, G>(
    mut resp: Response,
    mut translate_block: F,
    mut on_end: G,
) -> Pin<Box<dyn futures::Stream<Item = Result<Bytes, AishError>> + Send>>
where
    F: FnMut(&str) -> Result<Vec<String>, AishError> + Send + 'static,
    G: FnMut() -> Result<Vec<String>, AishError> + Send + 'static,
{
    let (tx, rx) = futures::channel::mpsc::unbounded();

    tokio::spawn(async move {
        let mut buffer = Vec::new();
        loop {
            match resp.chunk().await {
                Ok(Some(chunk)) => {
                    buffer.extend_from_slice(&chunk);
                    while let Some((pos, delim_len)) = find_sse_block_delimiter(&buffer) {
                        let block = match decode_sse_block(&buffer[..pos]) {
                            Ok(block) => block,
                            Err(err) => {
                                let _ = tx.unbounded_send(Err(err));
                                return;
                            }
                        };
                        buffer.drain(..pos + delim_len);
                        if block.trim().is_empty() {
                            continue;
                        }
                        match translate_block(&block) {
                            Ok(lines) => {
                                for line in lines {
                                    if tx.unbounded_send(Ok(Bytes::from(line))).is_err() {
                                        return;
                                    }
                                }
                            }
                            Err(err) => {
                                let _ = tx.unbounded_send(Err(err));
                                return;
                            }
                        }
                    }
                }
                Ok(None) => {
                    if !buffer.is_empty() {
                        match decode_sse_block(&buffer) {
                            Ok(block) if !block.trim().is_empty() => {
                                match translate_block(&block) {
                                    Ok(lines) => {
                                        for line in lines {
                                            if tx.unbounded_send(Ok(Bytes::from(line))).is_err() {
                                                return;
                                            }
                                        }
                                    }
                                    Err(err) => {
                                        let _ = tx.unbounded_send(Err(err));
                                        return;
                                    }
                                }
                            }
                            Ok(_) => {}
                            Err(err) => {
                                let _ = tx.unbounded_send(Err(err));
                                return;
                            }
                        }
                    }
                    match on_end() {
                        Ok(lines) => {
                            for line in lines {
                                let _ = tx.unbounded_send(Ok(Bytes::from(line)));
                            }
                        }
                        Err(err) => {
                            let _ = tx.unbounded_send(Err(err));
                        }
                    }
                    break;
                }
                Err(err) => {
                    let _ = tx.unbounded_send(Err(AishError::Llm(err.to_string())));
                    return;
                }
            }
        }
    });

    Box::pin(rx)
}

pub fn translate_anthropic_sse_stream(resp: Response) -> LlmStream {
    let translator = Arc::new(Mutex::new(AnthropicSseTranslator::new()));
    let translator_block = Arc::clone(&translator);
    let translator_end = Arc::clone(&translator);
    let stream = spawn_sse_translation(
        resp,
        move |block| {
            let mut translator = translator_block
                .lock()
                .map_err(|e| AishError::Llm(format!("SSE translator lock poisoned: {e}")))?;
            let mut out = Vec::new();
            for line in block.lines() {
                let line = line.trim();
                if !line.starts_with("data: ") {
                    continue;
                }
                out.extend(translator.process_data_line(&line[6..])?);
            }
            Ok(out)
        },
        move || {
            let mut translator = translator_end
                .lock()
                .map_err(|e| AishError::Llm(format!("SSE translator lock poisoned: {e}")))?;
            Ok(translator.finish_chunks())
        },
    );
    LlmStream::from_translated(stream)
}

pub fn translate_codex_responses_sse_stream(resp: Response) -> LlmStream {
    let translator = Arc::new(Mutex::new(CodexResponsesSseTranslator::new()));
    let translator_block = Arc::clone(&translator);
    let translator_end = Arc::clone(&translator);
    let stream = spawn_sse_translation(
        resp,
        move |block| {
            let mut translator = translator_block
                .lock()
                .map_err(|e| AishError::Llm(format!("SSE translator lock poisoned: {e}")))?;
            let events = parse_sse_text(block);
            let mut out = Vec::new();
            for (event_type, payload) in events {
                out.extend(translator.process_event(&event_type, &payload)?);
            }
            Ok(out)
        },
        move || {
            let mut translator = translator_end
                .lock()
                .map_err(|e| AishError::Llm(format!("SSE translator lock poisoned: {e}")))?;
            translator.finish()
        },
    );
    LlmStream::from_translated(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::streaming::{SseEvent, StreamParser};

    #[test]
    fn anthropic_text_delta_translates_to_openai_sse() {
        let mut translator = AnthropicSseTranslator::new();
        let out = translator
            .process_data_line(
                r#"{"type":"content_block_delta","delta":{"type":"text_delta","text":"Hi"}}"#,
            )
            .unwrap();
        assert_eq!(out.len(), 1);
        let (events, _) = StreamParser::parse_sse_chunk(out[0].trim());
        assert!(matches!(events.first(), Some(SseEvent::ContentDelta(text)) if text == "Hi"));
    }

    #[test]
    fn codex_text_delta_translates_to_openai_sse() {
        let mut translator = CodexResponsesSseTranslator::new();
        let out = translator
            .process_event("response.output_text.delta", &json!({"delta": "Hello"}))
            .unwrap();
        assert_eq!(out.len(), 1);
        let (events, _) = StreamParser::parse_sse_chunk(out[0].trim());
        assert!(matches!(events.first(), Some(SseEvent::ContentDelta(text)) if text == "Hello"));
    }

    #[test]
    fn sse_block_delimiter_handles_lf_and_crlf() {
        assert_eq!(
            find_sse_block_delimiter(b"event: x\r\n\r\nrest"),
            Some((8, 4))
        );
        assert_eq!(find_sse_block_delimiter(b"event: x\n\nrest"), Some((8, 2)));
    }

    #[test]
    fn decode_sse_block_rejects_invalid_utf8() {
        assert!(decode_sse_block(&[0xff, 0xfe]).is_err());
    }
}
