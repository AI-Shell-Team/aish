//! Shared ChatMessage / ToolSpec conversion for API dialect adapters.

use aish_core::AishError;
use serde_json::Value;

use crate::providers::codex::CodexError;
use crate::types::{ChatMessage, ToolSpec};

pub(crate) fn chat_messages_to_values(messages: &[ChatMessage]) -> Result<Vec<Value>, AishError> {
    messages
        .iter()
        .map(|msg| {
            serde_json::to_value(msg)
                .map_err(|e| AishError::Llm(format!("Failed to serialize message: {e}")))
        })
        .collect()
}

pub(crate) fn tools_to_values(tools: &[ToolSpec]) -> Result<Vec<Value>, AishError> {
    tools
        .iter()
        .map(|tool| {
            serde_json::to_value(tool)
                .map_err(|e| AishError::Llm(format!("Failed to serialize tool: {e}")))
        })
        .collect()
}

pub(crate) fn codex_error_to_aish(err: CodexError) -> AishError {
    AishError::Llm(err.to_string())
}
