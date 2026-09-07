//! Live probes used by CLI checks and setup verification.
//!
//! Probes mirror the interactive shell request path (dialect routing + streaming
//! + normal token budget) so proxy backends can remap models without breaking CI.

use std::collections::HashMap;
use std::time::Duration;

use aish_core::AishError;

use crate::api::{resolve_api_dialect, stream_simple, StreamContext};
use crate::client::LlmResponse;
use crate::streaming::{SseEvent, StreamParser};
use crate::types::{ChatMessage, ToolCall, ToolSpec};

/// Name of the deterministic read-only test tool the probe requires the model
/// to call. A response without a well-formed call to this tool fails the probe.
const PROBE_TOOL_NAME: &str = "aish_probe_echo";
/// Expected argument value for the probe tool — proves the provider serialized
/// a concrete argument rather than echoing an empty object.
const PROBE_TOOL_ARGS: &str = r#"{"value":"ping"}"#;
/// Payload returned by the (locally evaluated) probe tool and fed back to the
/// provider as the tool result.
const PROBE_TOOL_OUTPUT: &str = "PROBE_ECHO_OK";
/// Marker text the provider must include in its final answer after receiving
/// the tool result.
const PROBE_FINAL_MARKER: &str = "PROBE_ECHO_OK";

/// Verify that the configured endpoint supports tool calling by driving one
/// full, side-effect-free tool-call round trip through the production path:
///
/// 1. Request with a single probe tool registered.
/// 2. Require a well-formed `tool_calls` entry: expected name, parseable JSON
///    arguments, non-empty call id.
/// 3. "Execute" the probe tool locally (pure string check, no side effects)
///    and send the tool result back.
/// 4. Require a final assistant answer that references the tool result.
///
/// Plain text replies, empty responses, missing tool name, malformed
/// arguments, wrong finish reasons and timeouts all fail.
pub async fn probe_live_tool_support(
    api_base: &str,
    api_key: &str,
    model: &str,
) -> Result<(), AishError> {
    let ctx = StreamContext::new(api_base, api_key, model, None);
    let dialect = resolve_api_dialect(&ctx.config_model, &ctx.api_base, &ctx.api_key);
    let tool = probe_tool_spec();

    // Step 1: ask the model to call the probe tool.
    let messages = vec![ChatMessage::user(format!(
        "Call the `{PROBE_TOOL_NAME}` tool exactly once with arguments \
         {PROBE_TOOL_ARGS}, then report its output verbatim. Do not answer \
         in any other way."
    ))];

    let response = stream_simple(
        dialect,
        &ctx,
        &messages,
        Some(std::slice::from_ref(&tool)),
        true,
        Some(0.0),
        None,
    )
    .await?;

    let (content, tool_calls, finish_reason) = collect_response(response).await?;

    // Step 2: the first response must contain exactly one well-formed tool
    // call to the probe tool. A plain-text reply means the endpoint ignores
    // the tools field — the primary failure mode this probe exists to catch.
    let calls = tool_calls.ok_or_else(|| {
        AishError::Llm(
            "endpoint did not issue a tool call (tools ignored or unsupported)".to_string(),
        )
    })?;
    if calls.len() != 1 {
        return Err(AishError::Llm(format!(
            "expected exactly 1 tool call, got {}",
            calls.len()
        )));
    }
    let call = &calls[0];
    if call.name != PROBE_TOOL_NAME {
        return Err(AishError::Llm(format!(
            "unexpected tool name: {}",
            call.name
        )));
    }
    if call.id.trim().is_empty() {
        return Err(AishError::Llm("tool call id is missing".to_string()));
    }
    let args: serde_json::Value = serde_json::from_str(&call.arguments)
        .map_err(|e| AishError::Llm(format!("tool arguments are not valid JSON: {e}")))?;
    if args.get("value").and_then(|v| v.as_str()) != Some("ping") {
        return Err(AishError::Llm(
            "tool arguments missing expected value".to_string(),
        ));
    }
    if matches!(finish_reason.as_deref(), Some("content_filter")) {
        return Err(AishError::Llm("response stopped by content filter".into()));
    }

    // Step 3: execute the probe tool locally (read-only, deterministic) and
    // feed the tool result back through the production path.
    let mut followup = messages;
    followup.push(ChatMessage {
        role: "assistant".into(),
        content: if content.is_empty() {
            None
        } else {
            Some(crate::types::MessageContent::Text(content))
        },
        tool_calls: Some(calls.clone()),
        tool_call_id: None,
        name: None,
        reasoning_content: None,
        cache_control: None,
    });
    followup.push(ChatMessage::tool_result(&call.id, PROBE_TOOL_OUTPUT));

    let response = stream_simple(
        dialect,
        &ctx,
        &followup,
        Some(std::slice::from_ref(&tool)),
        true,
        Some(0.0),
        None,
    )
    .await?;

    // Step 4: the provider must produce a final answer that reflects the
    // tool result.
    let (content, calls, finish_reason) = collect_response(response).await?;
    if calls.is_some() {
        return Err(AishError::Llm(
            "provider issued another tool call after receiving the tool result".to_string(),
        ));
    }
    if matches!(
        finish_reason.as_deref(),
        Some("length") | Some("content_filter")
    ) {
        return Err(AishError::Llm(format!(
            "final response ended with unusable finish reason: {}",
            finish_reason.unwrap_or_default()
        )));
    }
    if !content.contains(PROBE_FINAL_MARKER) {
        return Err(AishError::Llm(
            "final response does not reference the tool result".to_string(),
        ));
    }

    Ok(())
}

/// Build the deterministic read-only probe tool spec.
fn probe_tool_spec() -> ToolSpec {
    ToolSpec {
        r#type: "function".into(),
        function: crate::types::FunctionSpec {
            name: PROBE_TOOL_NAME.into(),
            description: "Echo probe tool used by aish to verify tool calling. \
                          Always call it with the requested value."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "value": {"type": "string", "description": "Value to echo"}
                },
                "required": ["value"]
            }),
        },
    }
}

/// Drain a streaming (or JSON) response into (text, tool calls, finish reason)
/// using the same SSE aggregation rules as the interactive session.
async fn collect_response(
    response: LlmResponse,
) -> Result<(String, Option<Vec<ToolCall>>, Option<String>), AishError> {
    match response {
        LlmResponse::Json(json) => {
            let (content, _reasoning, tool_calls, _usage) = StreamParser::parse_response(&json);
            let finish = json
                .get("choices")
                .and_then(|c| c.as_array())
                .and_then(|c| c.first())
                .and_then(|c| c.get("finish_reason"))
                .and_then(|r| r.as_str())
                .map(|s| s.to_string());
            Ok((content.unwrap_or_default(), Some(tool_calls), finish))
        }
        LlmResponse::Stream(mut stream) => {
            let mut accumulated = String::new();
            let mut tool_calls_accum: HashMap<usize, (String, String, String)> =
                HashMap::with_capacity(2);
            let mut finish_reason: Option<String> = None;
            let mut text_buffer = String::with_capacity(1024);

            // Apply one parsed SSE event to the accumulators.
            fn apply_event(
                event: SseEvent,
                accumulated: &mut String,
                tool_calls_accum: &mut HashMap<usize, (String, String, String)>,
                finish_reason: &mut Option<String>,
            ) {
                match event {
                    SseEvent::ContentDelta(delta) => accumulated.push_str(&delta),
                    SseEvent::ToolCallDelta {
                        index,
                        id,
                        name,
                        arguments,
                    } => {
                        let entry = tool_calls_accum
                            .entry(index)
                            .or_insert_with(|| (String::new(), String::new(), String::new()));
                        if let Some(i) = id.filter(|s| !s.is_empty()) {
                            entry.0 = i;
                        }
                        if let Some(n) = name.filter(|s| !s.is_empty()) {
                            entry.1 = n;
                        }
                        if let Some(a) = arguments {
                            entry.2.push_str(&a);
                        }
                    }
                    SseEvent::Finish(reason) => *finish_reason = Some(reason),
                    SseEvent::Done | SseEvent::ReasoningDelta(_) => {}
                }
            }

            // Parse one SSE block (already split off the buffer) into events.
            fn apply_block(
                block: &str,
                accumulated: &mut String,
                tool_calls_accum: &mut HashMap<usize, (String, String, String)>,
                finish_reason: &mut Option<String>,
            ) {
                for line in block.lines() {
                    let (events, _usage) = StreamParser::parse_sse_chunk(line);
                    for event in events {
                        apply_event(event, accumulated, tool_calls_accum, finish_reason);
                    }
                }
            }

            loop {
                match stream.chunk().await {
                    Ok(Some(chunk)) => {
                        text_buffer.push_str(&String::from_utf8_lossy(&chunk));
                        // Process complete SSE blocks (delimited by blank line).
                        while let Some(pos) = text_buffer.find("\n\n") {
                            let block = text_buffer[..pos].to_string();
                            text_buffer.drain(..pos + 2);
                            apply_block(
                                &block,
                                &mut accumulated,
                                &mut tool_calls_accum,
                                &mut finish_reason,
                            );
                        }
                    }
                    Ok(None) => break,
                    Err(e) => return Err(AishError::Llm(format!("Stream error: {}", e))),
                }
            }

            // Some providers close the stream without a trailing blank line;
            // the final event (tool-call delta or finish reason) would otherwise
            // be lost with the residual buffer.
            if !text_buffer.trim().is_empty() {
                apply_block(
                    &text_buffer,
                    &mut accumulated,
                    &mut tool_calls_accum,
                    &mut finish_reason,
                );
            }

            if tool_calls_accum.is_empty() {
                return Ok((accumulated, None, finish_reason));
            }
            let mut sorted: Vec<(usize, (String, String, String))> =
                tool_calls_accum.into_iter().collect();
            sorted.sort_by_key(|(i, _)| *i);
            let calls: Vec<ToolCall> = sorted
                .into_iter()
                .map(|(_, (id, name, args))| ToolCall {
                    id,
                    name,
                    arguments: args,
                })
                .collect();
            Ok((accumulated, Some(calls), finish_reason))
        }
    }
}

/// Run the probe with a wall-clock budget so a provider that accepts the
/// request but stalls the stream cannot hang callers that lack their own
/// timeout.
pub async fn probe_live_tool_support_with_timeout(
    api_base: &str,
    api_key: &str,
    model: &str,
    timeout: Duration,
) -> Result<(), AishError> {
    tokio::time::timeout(timeout, probe_live_tool_support(api_base, api_key, model))
        .await
        .map_err(|_| {
            AishError::Llm(format!(
                "tool-call probe timed out after {}s",
                timeout.as_secs().max(1)
            ))
        })?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::thread;

    /// Successful round trip: tool call -> tool result -> final answer.
    #[tokio::test]
    async fn test_probe_success_full_loop() {
        let server = spawn_mock(|req_num, _body| {
            if req_num == 1 {
                sse_body(vec![
                    tool_call_delta(0, "call_probe_1", PROBE_TOOL_NAME, PROBE_TOOL_ARGS),
                    finish_chunk("tool_calls"),
                    done(),
                ])
            } else {
                sse_body(vec![
                    content_delta(&format!("Result: {PROBE_FINAL_MARKER}")),
                    finish_chunk("stop"),
                    done(),
                ])
            }
        });
        probe_live_tool_support_with_timeout(
            &server.url,
            "test-key",
            "test-model",
            Duration::from_secs(10),
        )
        .await
        .unwrap();
    }

    /// Provider closes the stream without a trailing blank line after the last
    /// SSE block. The final tool-call delta must still be parsed (regression:
    /// residual-buffer bytes used to be discarded, producing a false
    /// "did not issue a tool call" on compliant-ish endpoints).
    #[tokio::test]
    async fn test_probe_survives_missing_trailing_blank_line() {
        let server = spawn_mock(|req_num, _body| {
            if req_num == 1 {
                // Same events as the success test, but the last block is
                // terminated by a bare "\n" (no blank line) and no [DONE].
                format!(
                    "{}{}",
                    tool_call_delta(0, "call_probe_1", PROBE_TOOL_NAME, PROBE_TOOL_ARGS),
                    finish_chunk("tool_calls")
                        .strip_suffix("\n\n")
                        .map(|s| format!("{s}\n"))
                        .unwrap_or_default(),
                )
            } else {
                sse_body(vec![
                    content_delta(&format!("Result: {PROBE_FINAL_MARKER}")),
                    finish_chunk("stop"),
                    done(),
                ])
            }
        });
        probe_live_tool_support_with_timeout(
            &server.url,
            "test-key",
            "test-model",
            Duration::from_secs(10),
        )
        .await
        .unwrap();
    }

    /// Endpoint ignores tools and replies plain text -> probe must fail.
    #[tokio::test]
    async fn test_probe_fails_on_plain_text() {
        let server =
            spawn_mock(|_, _| sse_body(vec![content_delta("ok"), finish_chunk("stop"), done()]));
        let err = probe_live_tool_support_with_timeout(
            &server.url,
            "test-key",
            "test-model",
            Duration::from_secs(10),
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("did not issue a tool call"),
            "unexpected error: {err}"
        );
        assert_eq!(server.request_count(), 1);
    }

    /// Tool call with a missing function name -> probe must fail.
    #[tokio::test]
    async fn test_probe_fails_on_missing_name() {
        let server = spawn_mock(|_, _| {
            sse_body(vec![
                tool_call_delta(0, "call_probe_1", "", PROBE_TOOL_ARGS),
                finish_chunk("tool_calls"),
                done(),
            ])
        });
        let err = probe_live_tool_support_with_timeout(
            &server.url,
            "test-key",
            "test-model",
            Duration::from_secs(10),
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("unexpected tool name"),
            "unexpected error: {err}"
        );
    }

    /// Tool call with malformed (unclosed) JSON arguments -> probe must fail.
    #[tokio::test]
    async fn test_probe_fails_on_malformed_arguments() {
        let server = spawn_mock(|_, _| {
            sse_body(vec![
                tool_call_delta(0, "call_probe_1", PROBE_TOOL_NAME, r#"{"value":"ping""#),
                finish_chunk("tool_calls"),
                done(),
            ])
        });
        let err = probe_live_tool_support_with_timeout(
            &server.url,
            "test-key",
            "test-model",
            Duration::from_secs(10),
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("not valid JSON"),
            "unexpected error: {err}"
        );
    }

    /// Provider keeps issuing tool calls and never produces a final answer.
    #[tokio::test]
    async fn test_probe_fails_on_repeated_tool_calls() {
        let server = spawn_mock(|_, _| {
            sse_body(vec![
                tool_call_delta(0, "call_probe_1", PROBE_TOOL_NAME, PROBE_TOOL_ARGS),
                finish_chunk("tool_calls"),
                done(),
            ])
        });
        let err = probe_live_tool_support_with_timeout(
            &server.url,
            "test-key",
            "test-model",
            Duration::from_secs(10),
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("another tool call"),
            "unexpected error: {err}"
        );
    }

    /// Final answer that ignores the tool result -> probe must fail.
    #[tokio::test]
    async fn test_probe_fails_on_final_without_marker() {
        let server = spawn_mock(|req_num, _| {
            if req_num == 1 {
                sse_body(vec![
                    tool_call_delta(0, "call_probe_1", PROBE_TOOL_NAME, PROBE_TOOL_ARGS),
                    finish_chunk("tool_calls"),
                    done(),
                ])
            } else {
                sse_body(vec![content_delta("ok"), finish_chunk("stop"), done()])
            }
        });
        let err = probe_live_tool_support_with_timeout(
            &server.url,
            "test-key",
            "test-model",
            Duration::from_secs(10),
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("does not reference the tool result"),
            "unexpected error: {err}"
        );
    }

    /// Provider that closes the stream without sending anything.
    #[tokio::test]
    async fn test_probe_fails_on_empty_stream() {
        let server = spawn_mock(|_, _| sse_body(vec![]));
        let err = probe_live_tool_support_with_timeout(
            &server.url,
            "test-key",
            "test-model",
            Duration::from_secs(10),
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("did not issue a tool call"),
            "unexpected error: {err}"
        );
    }

    /// Provider stalling mid-stream -> timeout wrapper must abort.
    #[tokio::test]
    async fn test_probe_times_out_on_stalled_stream() {
        let server = spawn_mock_stall();
        let err = probe_live_tool_support_with_timeout(
            &server.url,
            "test-key",
            "test-model",
            Duration::from_secs(2),
        )
        .await
        .unwrap_err();
        assert!(
            err.to_string().contains("timed out"),
            "unexpected error: {err}"
        );
    }

    // -- mock server plumbing ------------------------------------------------

    struct MockServer {
        url: String,
        shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
        handle: Option<thread::JoinHandle<()>>,
        requests: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl MockServer {
        fn request_count(&self) -> usize {
            self.requests.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl Drop for MockServer {
        fn drop(&mut self) {
            self.shutdown
                .store(true, std::sync::atomic::Ordering::SeqCst);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    type RespondFn = Box<dyn Fn(usize, &serde_json::Value) -> String + Send>;

    /// Bind a loopback server that answers `/v1/chat/completions` with the
    /// body produced by `respond` (1-based request counter passed in).
    fn spawn_mock(
        respond: impl Fn(usize, &serde_json::Value) -> String + Send + 'static,
    ) -> MockServer {
        spawn_mock_inner(Box::new(respond), false)
    }

    /// Same as [`spawn_mock`] but the first response stalls forever.
    fn spawn_mock_stall() -> MockServer {
        spawn_mock_inner(Box::new(|_, _| String::new()), true)
    }

    fn spawn_mock_inner(respond: RespondFn, stall: bool) -> MockServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let requests_clone = requests.clone();
        let shutdown_clone = shutdown.clone();

        let handle = thread::spawn(move || {
            listener
                .set_nonblocking(true)
                .expect("set_nonblocking failed");
            while !shutdown_clone.load(std::sync::atomic::Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let n =
                            requests_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                        let body = read_http_request(&mut stream);
                        if stall && n == 1 {
                            // Send headers but never a body: the client hangs
                            // on read until the shutdown flag closes it.
                            let _ = stream.write_all(
                                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n",
                            );
                            let _ = stream.flush();
                            while !shutdown_clone.load(std::sync::atomic::Ordering::SeqCst) {
                                thread::sleep(Duration::from_millis(20));
                            }
                            // Dropping `stream` here closes the connection.
                            continue;
                        }
                        let payload = respond(n, &body);
                        // Always emit a valid HTTP response, even for an
                        // empty SSE body (Content-Length: 0).
                        let http = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            payload.len(),
                            payload
                        );
                        let _ = stream.write_all(http.as_bytes());
                        let _ = stream.flush();
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => return,
                }
            }
        });

        MockServer {
            url: format!("http://127.0.0.1:{port}/v1"),
            shutdown,
            handle: Some(handle),
            requests,
        }
    }

    /// Read one HTTP request (headers + JSON body) and return the parsed body.
    fn read_http_request(stream: &mut std::net::TcpStream) -> serde_json::Value {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let header_end = loop {
            let n = stream.read(&mut chunk).unwrap_or(0);
            if n == 0 {
                return serde_json::Value::Null;
            }
            buf.extend_from_slice(&chunk[..n]);
            if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
                break pos + 4;
            }
        };
        let header_text = String::from_utf8_lossy(&buf[..header_end]).to_string();
        let content_length = header_text
            .lines()
            .find_map(|l| {
                let (name, value) = l.split_once(':')?;
                name.trim()
                    .eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())?
            })
            .unwrap_or(0);
        while buf.len() < header_end + content_length {
            let n = stream.read(&mut chunk).unwrap_or(0);
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        serde_json::from_slice(&buf[header_end..header_end + content_length])
            .unwrap_or(serde_json::Value::Null)
    }

    fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    fn sse_body(events: Vec<String>) -> String {
        let mut body = String::new();
        for e in events {
            body.push_str(&e);
        }
        body
    }

    fn content_delta(text: &str) -> String {
        let payload = serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {"content": text},
                "finish_reason": null,
            }]
        });
        format!("data: {payload}\n\n")
    }

    fn tool_call_delta(index: usize, id: &str, name: &str, args: &str) -> String {
        let payload = serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {"tool_calls": [{
                    "index": index,
                    "id": id,
                    "type": "function",
                    "function": {"name": name, "arguments": args},
                }]},
                "finish_reason": null,
            }]
        });
        format!("data: {payload}\n\n")
    }

    fn finish_chunk(reason: &str) -> String {
        let payload = serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": reason,
            }]
        });
        format!("data: {payload}\n\n")
    }

    fn done() -> String {
        "data: [DONE]\n\n".to_string()
    }
}
