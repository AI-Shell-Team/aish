//! Live probes used by CLI checks and setup verification.
//!
//! Probes mirror the interactive shell request path (dialect routing + streaming
//! + normal token budget) so proxy backends can remap models without breaking CI.

use aish_core::AishError;

use crate::api::{resolve_api_dialect, stream_simple, StreamContext};
use crate::client::LlmResponse;
use crate::types::{ChatMessage, ToolSpec};

/// Verify that the configured endpoint supports tool calling via the production path.
pub async fn probe_live_tool_support(
    api_base: &str,
    api_key: &str,
    model: &str,
) -> Result<(), AishError> {
    let ctx = StreamContext::new(api_base, api_key, model, None);
    let dialect = resolve_api_dialect(&ctx.config_model, &ctx.api_base, &ctx.api_key);
    let messages = vec![ChatMessage::user(
        "Reply with just 'ok'. Do not use any tools.",
    )];
    let tool = ToolSpec {
        r#type: "function".into(),
        function: crate::types::FunctionSpec {
            name: "test_tool".into(),
            description: "A test tool".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "input": {"type": "string"}
                }
            }),
        },
    };

    let response = stream_simple(
        dialect,
        &ctx,
        &messages,
        Some(&[tool]),
        true,
        Some(0.0),
        None,
    )
    .await?;

    match response {
        LlmResponse::Json(_) => Ok(()),
        LlmResponse::Stream(mut stream) => match stream.chunk().await? {
            Some(_) => Ok(()),
            None => Err(AishError::Llm(
                "empty streaming response from API".to_string(),
            )),
        },
    }
}
