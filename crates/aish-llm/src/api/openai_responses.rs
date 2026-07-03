//! OpenAI Platform `/v1/responses` dialect (API key auth).

use aish_core::AishError;

use crate::client::LlmResponse;
use crate::providers::codex::{
    convert_codex_response, create_openai_responses_with_api_key, CodexError,
};
use crate::types::{ChatMessage, ToolSpec};

use super::convert::{chat_messages_to_values, codex_error_to_aish, tools_to_values};
use super::{effective_max_tokens, StreamContext};

pub async fn stream(
    ctx: &StreamContext,
    messages: &[ChatMessage],
    tools: Option<&[ToolSpec]>,
    _stream: bool,
    _temperature: Option<f32>,
    max_tokens: Option<u32>,
) -> Result<LlmResponse, AishError> {
    let message_values = chat_messages_to_values(messages)?;
    let tool_values = tools.map(tools_to_values).transpose()?;
    let tool_refs = tool_values.as_deref();

    let payload = create_openai_responses_with_api_key(
        &ctx.api_base,
        &ctx.api_key,
        &ctx.model,
        &message_values,
        tool_refs,
        "auto",
        Some(effective_max_tokens(max_tokens)),
        120,
    )
    .await
    .map_err(codex_error_to_aish)?;

    let openai_json = convert_codex_response(&payload);
    Ok(LlmResponse::Json(openai_json))
}

pub async fn test_connection(ctx: &StreamContext) -> Result<(), String> {
    if ctx.api_key.trim().is_empty() {
        return Err("API key is required".to_string());
    }
    create_openai_responses_with_api_key(
        &ctx.api_base,
        &ctx.api_key,
        &ctx.resolved_model(),
        &[serde_json::json!({"role": "user", "content": "Hi"})],
        None,
        "auto",
        Some(16),
        30,
    )
    .await
    .map_err(|e: CodexError| e.to_string())?;
    Ok(())
}
