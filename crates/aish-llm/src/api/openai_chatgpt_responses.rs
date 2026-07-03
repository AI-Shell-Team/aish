//! OpenAI ChatGPT Responses API dialect (Codex OAuth `/responses`).

use aish_core::AishError;

use crate::client::LlmResponse;
use crate::openai_sse_bridge::translate_codex_responses_sse_stream;
use crate::providers::codex::{
    convert_codex_response, create_codex_chat_completion, create_codex_http_response,
    load_codex_auth,
};
use crate::types::{ChatMessage, ToolSpec};

use super::convert::{chat_messages_to_values, codex_error_to_aish, tools_to_values};
use super::{effective_max_tokens, StreamContext};

pub async fn stream(
    ctx: &StreamContext,
    messages: &[ChatMessage],
    tools: Option<&[ToolSpec]>,
    stream: bool,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
) -> Result<LlmResponse, AishError> {
    let message_values = chat_messages_to_values(messages)?;
    let tool_values = tools.map(tools_to_values).transpose()?;
    let tool_refs = tool_values.as_deref();
    let max_output = Some(effective_max_tokens(max_tokens));

    let auth_path = ctx.codex_auth_path.as_deref();
    let api_base = if ctx.api_base.is_empty() {
        None
    } else {
        Some(ctx.api_base.as_str())
    };

    if stream {
        let resp = create_codex_http_response(
            &ctx.model,
            &message_values,
            tool_refs,
            "auto",
            max_output,
            temperature,
            api_base,
            auth_path,
            120,
        )
        .await
        .map_err(codex_error_to_aish)?;
        return Ok(LlmResponse::Stream(translate_codex_responses_sse_stream(
            resp,
        )));
    }

    let payload = create_codex_chat_completion(
        &ctx.model,
        &message_values,
        tool_refs,
        "auto",
        max_output,
        temperature,
        api_base,
        auth_path,
        120,
    )
    .await
    .map_err(codex_error_to_aish)?;

    let openai_json = convert_codex_response(&payload);
    Ok(LlmResponse::Json(openai_json))
}

pub async fn test_connection(ctx: &StreamContext) -> Result<(), String> {
    let auth_path = ctx.codex_auth_path.as_deref();
    load_codex_auth(auth_path).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::codex::build_codex_request;
    use crate::types::{FunctionSpec, ToolSpec};

    #[test]
    fn test_build_codex_request_from_chat_messages() {
        let messages = vec![ChatMessage::system("sys"), ChatMessage::user("hi")];
        let values = chat_messages_to_values(&messages).unwrap();
        let body = build_codex_request("openai-codex/gpt-5.4", &values, None, "auto", None, None);
        assert_eq!(body["model"], "gpt-5.4");
        assert!(body["instructions"].as_str().unwrap().contains("sys"));
    }

    #[test]
    fn test_responses_payload_to_openai_json_roundtrip() {
        let raw = serde_json::json!({
            "output": [{
                "type": "message",
                "content": [{"type": "output_text", "text": "done"}]
            }],
            "usage": {"input_tokens": 3, "output_tokens": 1}
        });
        let openai = convert_codex_response(&raw);
        assert_eq!(openai["choices"][0]["message"]["content"], "done");
        assert_eq!(openai["usage"]["prompt_tokens"], 3);
    }

    #[test]
    fn test_tools_to_values() {
        let tools = vec![ToolSpec {
            r#type: "function".into(),
            function: FunctionSpec {
                name: "bash".into(),
                description: "run".into(),
                parameters: serde_json::json!({"type": "object"}),
            },
        }];
        let values = tools_to_values(&tools).unwrap();
        assert_eq!(values[0]["function"]["name"], "bash");
    }
}
