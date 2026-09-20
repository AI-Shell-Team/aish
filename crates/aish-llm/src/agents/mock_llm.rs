//! Helpers for building scripted LLM JSON responses in tests.

use crate::client::LlmResponse;

/// Build a non-streaming JSON response with plain assistant text and no tool calls.
pub fn mock_text_response(text: &str) -> LlmResponse {
    LlmResponse::Json(serde_json::json!({
        "choices": [{
            "message": {
                "content": text,
            }
        }]
    }))
}

/// Build a non-streaming JSON response with one or more tool calls.
///
/// Each tuple is `(call_id, tool_name, arguments_json)`.
pub fn mock_tool_call_response(calls: &[(&str, &str, &str)]) -> LlmResponse {
    let tool_calls: Vec<serde_json::Value> = calls
        .iter()
        .map(|(id, name, args)| {
            serde_json::json!({
                "id": id,
                "type": "function",
                "function": {
                    "name": name,
                    "arguments": args,
                }
            })
        })
        .collect();

    LlmResponse::Json(serde_json::json!({
        "choices": [{
            "message": {
                "content": null,
                "tool_calls": tool_calls,
            }
        }]
    }))
}

/// Build a non-streaming JSON response whose `usage` block reports the given
/// prompt/completion tokens. Lets tests assert real accounting through
/// `TokenStats` (recorded once per request, totals and last-prompt).
pub fn mock_text_response_with_usage(
    text: &str,
    prompt_tokens: u64,
    completion_tokens: u64,
) -> LlmResponse {
    LlmResponse::Json(serde_json::json!({
        "choices": [{
            "message": {
                "content": text,
            }
        }],
        "usage": {
            "prompt_tokens": prompt_tokens,
            "completion_tokens": completion_tokens,
        }
    }))
}

/// [`mock_tool_call_response`] with a `usage` block attached, for tests that
/// assert usage accounting across multi-turn tool loops.
pub fn mock_tool_call_response_with_usage(
    calls: &[(&str, &str, &str)],
    prompt_tokens: u64,
    completion_tokens: u64,
) -> LlmResponse {
    let LlmResponse::Json(mut json) = mock_tool_call_response(calls) else {
        unreachable!("mock_tool_call_response always returns Json");
    };
    json["usage"] = serde_json::json!({
        "prompt_tokens": prompt_tokens,
        "completion_tokens": completion_tokens,
    });
    LlmResponse::Json(json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::streaming::StreamParser;

    #[test]
    fn mock_tool_call_response_parses_tool_calls() {
        let response = mock_tool_call_response(&[("c1", "grep", r#"{"pattern":"nginx"}"#)]);
        let LlmResponse::Json(json) = response else {
            panic!("expected json");
        };
        let (_, _, tool_calls, _) = StreamParser::parse_response(&json);
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].name, "grep");
    }
}
