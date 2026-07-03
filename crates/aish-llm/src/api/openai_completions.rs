//! OpenAI-compatible `/chat/completions` dialect.

use aish_core::AishError;

use crate::client::{LlmClient, LlmResponse};
use crate::types::{ChatMessage, ToolSpec};

use super::StreamContext;

pub async fn stream(
    ctx: &StreamContext,
    messages: &[ChatMessage],
    tools: Option<&[ToolSpec]>,
    stream: bool,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
) -> Result<LlmResponse, AishError> {
    let client = LlmClient::new(&ctx.api_base, &ctx.api_key, &ctx.model);
    client
        .chat_completion(messages, tools, stream, temperature, max_tokens)
        .await
}

pub async fn test_openai_completions_connection(ctx: &StreamContext) -> Result<(), String> {
    let client = LlmClient::new(&ctx.api_base, &ctx.api_key, &ctx.model);
    client.test_connection().await
}
