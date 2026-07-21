//! API dialect registry — dispatches LLM requests by protocol (OpenClaw-style).
//!
//! Each dialect (`openai-completions`, `openai-responses`, `anthropic-messages`,
//! `openai-chatgpt-responses`) has an independent HTTP implementation. Session code
//! calls [`stream_simple`] with a resolved [`ApiDialect`].

mod anthropic_messages;
mod convert;
mod openai_chatgpt_responses;
mod openai_completions;
mod openai_responses;

use std::path::PathBuf;

use aish_core::AishError;

use crate::client::LlmResponse;
use crate::providers::codex::is_codex_model;
use crate::types::{ChatMessage, ToolSpec};

pub use anthropic_messages::resolve_anthropic_messages_url;
pub use openai_completions::test_openai_completions_connection;

/// Protocol dialect for LLM HTTP APIs (maps to OpenClaw `model.api`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ApiDialect {
    OpenAiCompletions,
    OpenAiResponses,
    AnthropicMessages,
    OpenAiChatgptResponses,
}

/// Runtime context shared by all dialect adapters.
#[derive(Debug, Clone)]
pub struct StreamContext {
    pub api_base: String,
    pub api_key: String,
    /// Model name as stored in config (used for dialect routing).
    pub config_model: String,
    /// Model name sent to the HTTP API (prefixes may be stripped).
    pub model: String,
    pub codex_auth_path: Option<PathBuf>,
}

impl StreamContext {
    pub fn new(
        api_base: &str,
        api_key: &str,
        model: &str,
        codex_auth_path: Option<PathBuf>,
    ) -> Self {
        let api_base = api_base.trim_end_matches('/').to_string();
        let config_model = model.trim().to_string();
        let model = crate::model_id::resolve_model_for_api(model, &api_base);
        Self {
            api_base,
            api_key: api_key.to_string(),
            config_model,
            model,
            codex_auth_path,
        }
    }

    pub fn resolved_model(&self) -> String {
        self.model.clone()
    }

    pub fn refresh_dialect(&mut self, model: &str, api_base: Option<&str>, api_key: Option<&str>) {
        if let Some(base) = api_base {
            self.api_base = base.trim_end_matches('/').to_string();
        }
        if let Some(key) = api_key {
            self.api_key = key.to_string();
        }
        self.config_model = model.trim().to_string();
        self.model = crate::model_id::resolve_model_for_api(model, &self.api_base);
    }
}

/// Resolve which API dialect to use for the given model, endpoint, and credentials.
///
/// Codex models follow OpenClaw-style routing:
/// - OAuth / subscription → `OpenAiChatgptResponses` (`chatgpt.com/backend-api/codex`)
/// - API key on OpenAI Platform → `OpenAiResponses` (`api.openai.com/v1/responses`)
pub fn resolve_api_dialect(model: &str, api_base: &str, api_key: &str) -> ApiDialect {
    if is_codex_endpoint(api_base) {
        return ApiDialect::OpenAiChatgptResponses;
    }
    if is_codex_model(model) {
        if has_usable_api_key(api_key) && is_openai_api_base(api_base) {
            return ApiDialect::OpenAiResponses;
        }
        if is_openai_api_base(api_base) && !has_usable_api_key(api_key) {
            return ApiDialect::OpenAiChatgptResponses;
        }
    }
    if is_anthropic_native_endpoint(api_base) {
        return ApiDialect::AnthropicMessages;
    }
    ApiDialect::OpenAiCompletions
}

fn has_usable_api_key(api_key: &str) -> bool {
    !api_key.trim().is_empty()
}

pub fn is_openai_api_base(api_base: &str) -> bool {
    normalize_host(api_base)
        .as_deref()
        .is_some_and(|host| host == "api.openai.com")
}

fn is_codex_endpoint(api_base: &str) -> bool {
    let base = api_base.trim().to_lowercase();
    base.contains("backend-api/codex")
        || (base.contains("chatgpt.com") && base.contains("codex"))
        || base.contains("chat.openai.com") && base.contains("codex")
}

fn is_anthropic_native_endpoint(api_base: &str) -> bool {
    normalize_host(api_base)
        .as_deref()
        .is_some_and(|host| host == "api.anthropic.com")
}

fn normalize_host(api_base: &str) -> Option<String> {
    let trimmed = api_base.trim().trim_end_matches('/');
    let without_scheme = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(trimmed);
    without_scheme.split('/').next().map(|h| h.to_lowercase())
}

/// Dispatch a chat completion to the adapter for `dialect`.
pub async fn stream_simple(
    dialect: ApiDialect,
    ctx: &StreamContext,
    messages: &[ChatMessage],
    tools: Option<&[ToolSpec]>,
    stream: bool,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
) -> Result<LlmResponse, AishError> {
    match dialect {
        ApiDialect::OpenAiCompletions => {
            openai_completions::stream(ctx, messages, tools, stream, temperature, max_tokens).await
        }
        ApiDialect::OpenAiResponses => {
            openai_responses::stream(ctx, messages, tools, stream, temperature, max_tokens).await
        }
        ApiDialect::AnthropicMessages => {
            anthropic_messages::stream(ctx, messages, tools, stream, temperature, max_tokens).await
        }
        ApiDialect::OpenAiChatgptResponses => {
            openai_chatgpt_responses::stream(ctx, messages, tools, stream, temperature, max_tokens)
                .await
        }
    }
}

/// Test connectivity for the given dialect.
pub async fn test_connection(dialect: ApiDialect, ctx: &StreamContext) -> Result<(), String> {
    match dialect {
        ApiDialect::OpenAiCompletions => test_openai_completions_connection(ctx).await,
        ApiDialect::OpenAiResponses => openai_responses::test_connection(ctx).await,
        ApiDialect::AnthropicMessages => anthropic_messages::test_connection(ctx).await,
        ApiDialect::OpenAiChatgptResponses => openai_chatgpt_responses::test_connection(ctx).await,
    }
}

pub(crate) fn format_http_error(status: reqwest::StatusCode, body: &str) -> String {
    let hint = match status.as_u16() {
        401 | 403 => "Authentication failed. Please check your API key.".to_string(),
        404 => {
            "Model not found. The model name may be incorrect or the API endpoint may not support it."
                .to_string()
        }
        429 => "Rate limited. Please wait a moment and try again.".to_string(),
        500..=599 => "Server error. The API provider may be experiencing issues.".to_string(),
        _ => String::new(),
    };

    if hint.is_empty() {
        format!("API error {}: {}", status, body.trim())
    } else {
        format!("API error {}: {}\n{}", status, body.trim(), hint)
    }
}

// ---------------------------------------------------------------------------
// HTTP retry helpers — shared across all dialect adapters.
// ---------------------------------------------------------------------------

/// Maximum number of retry attempts for transient HTTP failures.
/// Total attempts = MAX_HTTP_RETRIES + 1 (initial attempt + retries).
pub(crate) const MAX_HTTP_RETRIES: u32 = 3;

/// HTTP status codes that indicate a transient failure worth retrying:
/// rate limiting (429) and common server-side errors (500/502/503/504).
/// Excludes 501 (Not Implemented) and 5xx codes that signal bugs.
pub(crate) fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 429 | 500 | 502 | 503 | 504)
}

/// Network-layer errors (connection, timeout, connection reset) that are
/// typically transient and safe to retry.
pub(crate) fn is_retryable_network_err(e: &reqwest::Error) -> bool {
    e.is_connect() || e.is_timeout()
}

/// Exponential backoff delay for a retry attempt (0-indexed).
/// Returns 0 for the first attempt (no delay before initial request).
/// Sequence: attempt 0 = 0ms, 1 = 500ms, 2 = 1000ms, 3 = 2000ms.
pub(crate) fn retry_backoff_delay(attempt: u32) -> std::time::Duration {
    match attempt {
        0 => std::time::Duration::ZERO,
        n => std::time::Duration::from_millis(500u64 << (n - 1).min(4)),
    }
}

pub(crate) fn effective_max_tokens(max_tokens: Option<u32>) -> u32 {
    match max_tokens {
        None | Some(0) => crate::client::DEFAULT_MAX_TOKENS,
        Some(n) => n,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::codex::CODEX_DEFAULT_BASE_URL;

    #[test]
    fn test_resolve_api_dialect_openrouter_claude() {
        assert_eq!(
            resolve_api_dialect(
                "anthropic/claude-3-opus",
                "https://openrouter.ai/api/v1",
                "sk-test"
            ),
            ApiDialect::OpenAiCompletions
        );
    }

    #[test]
    fn test_resolve_api_dialect_anthropic_native() {
        assert_eq!(
            resolve_api_dialect("claude-3-opus", "https://api.anthropic.com", "sk-ant"),
            ApiDialect::AnthropicMessages
        );
        assert_eq!(
            resolve_api_dialect("claude-3-opus", "https://api.anthropic.com/v1", "sk-ant"),
            ApiDialect::AnthropicMessages
        );
    }

    #[test]
    fn test_resolve_api_dialect_codex_model_api_key() {
        assert_eq!(
            resolve_api_dialect(
                "openai-codex/gpt-5.4",
                "https://api.openai.com/v1",
                "sk-test"
            ),
            ApiDialect::OpenAiResponses
        );
    }

    #[test]
    fn test_resolve_api_dialect_codex_model_oauth() {
        assert_eq!(
            resolve_api_dialect("openai-codex/gpt-5.4", "https://api.openai.com/v1", ""),
            ApiDialect::OpenAiChatgptResponses
        );
    }

    #[test]
    fn test_resolve_api_dialect_codex_endpoint() {
        assert_eq!(
            resolve_api_dialect("gpt-5.4", CODEX_DEFAULT_BASE_URL, ""),
            ApiDialect::OpenAiChatgptResponses
        );
        assert_eq!(
            resolve_api_dialect("gpt-5.4", CODEX_DEFAULT_BASE_URL, "sk-test"),
            ApiDialect::OpenAiChatgptResponses
        );
    }

    #[test]
    fn test_resolve_api_dialect_codex_model_on_openrouter() {
        assert_eq!(
            resolve_api_dialect(
                "openai-codex/gpt-5.4",
                "https://openrouter.ai/api/v1",
                "sk-or-test"
            ),
            ApiDialect::OpenAiCompletions
        );
    }

    #[test]
    fn test_is_openai_api_base() {
        assert!(is_openai_api_base("https://api.openai.com/v1"));
        assert!(is_openai_api_base("https://api.openai.com"));
        assert!(!is_openai_api_base("https://chatgpt.com/backend-api/codex"));
    }

    #[test]
    fn test_is_anthropic_native_endpoint() {
        assert!(is_anthropic_native_endpoint("https://api.anthropic.com"));
        assert!(is_anthropic_native_endpoint("https://api.anthropic.com/v1"));
        assert!(!is_anthropic_native_endpoint(
            "https://openrouter.ai/api/v1"
        ));
    }
}
