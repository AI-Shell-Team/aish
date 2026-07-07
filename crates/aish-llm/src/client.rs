use aish_core::AishError;
use reqwest::Client;

use crate::llm_stream::LlmStream;
use crate::model_id::resolve_model_for_api;
use crate::types::{ChatMessage, ToolSpec};

/// Default max_tokens when config does not specify a value.
pub const DEFAULT_MAX_TOKENS: u32 = 4096;

/// HTTP client for OpenAI-compatible chat completion APIs.
pub struct LlmClient {
    http: Client,
    api_base: String,
    api_key: String,
    model: std::sync::Mutex<String>,
}

impl LlmClient {
    pub fn new(api_base: &str, api_key: &str, model: &str) -> Self {
        let model = resolve_model_for_api(model, api_base);
        Self {
            http: Client::new(),
            api_base: api_base.trim_end_matches('/').into(),
            api_key: api_key.into(),
            model: std::sync::Mutex::new(model),
        }
    }

    /// Return the API base URL.
    pub fn api_base(&self) -> &str {
        &self.api_base
    }

    /// Return the API key.
    pub fn api_key(&self) -> &str {
        &self.api_key
    }

    /// Return the model name used for this client.
    pub fn model_name(&self) -> String {
        self.model.lock().unwrap().clone()
    }

    /// Update the model name (used for runtime model switching).
    pub fn update_model(&self, model: &str) {
        *self.model.lock().unwrap() = resolve_model_for_api(model, &self.api_base);
    }

    /// Update the API key.
    pub fn update_api_key(&mut self, api_key: &str) {
        self.api_key = api_key.to_string();
    }

    /// Update the API base URL.
    pub fn update_api_base(&mut self, api_base: &str) {
        self.api_base = api_base.trim_end_matches('/').to_string();
    }

    /// Test connectivity by sending a lightweight request to the API.
    pub async fn test_connection(&self) -> Result<(), String> {
        let url = format!("{}/models", self.api_base);
        let resp = self
            .http
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| format!("Connection failed: {}", e))?;

        if resp.status().is_success() {
            Ok(())
        } else {
            let status = resp.status().as_u16();
            // 401/403 means server is reachable but auth failed — still OK for connectivity check
            if status == 401 || status == 403 {
                Ok(())
            } else {
                Err(format!("Server returned status {}", status))
            }
        }
    }

    /// Create a new client with retry logic for transient failures.
    /// Retries up to `max_retries` times with exponential backoff.
    /// Returns a client even if all retries fail (it may work later when the network recovers).
    pub fn new_with_retry(api_base: &str, api_key: &str, model: &str, max_retries: u32) -> Self {
        let rt = tokio::runtime::Runtime::new().expect("failed to create tokio runtime");
        let client = Self::new(api_base, api_key, model);

        for attempt in 0..=max_retries {
            if attempt > 0 {
                let delay = std::time::Duration::from_millis(200 * 2u64.pow(attempt - 1));
                tracing::info!(
                    "Retrying LLM connectivity check (attempt {}/{}) after {:?}",
                    attempt + 1,
                    max_retries + 1,
                    delay
                );
                std::thread::sleep(delay);
            }

            match rt.block_on(client.test_connection()) {
                Ok(()) => {
                    if attempt > 0 {
                        tracing::info!(
                            "LLM connectivity check succeeded on attempt {}",
                            attempt + 1
                        );
                    }
                    return client;
                }
                Err(e) => {
                    tracing::warn!(
                        "LLM connectivity check attempt {} failed: {}",
                        attempt + 1,
                        e
                    );
                }
            }
        }

        tracing::warn!(
            "LLM connectivity check failed after {} retries, proceeding anyway",
            max_retries
        );
        client
    }

    /// Send a chat completion request with optional streaming.
    pub async fn chat_completion(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[ToolSpec]>,
        stream: bool,
        temperature: Option<f32>,
        max_tokens: Option<u32>,
    ) -> Result<LlmResponse, AishError> {
        self.chat_completion_with_extras(
            messages,
            tools,
            stream,
            temperature,
            max_tokens,
            serde_json::Map::new(),
        )
        .await
    }

    /// Like `chat_completion`, but merges provider-specific extra fields into
    /// the JSON request body. Used for vendor extensions such as
    /// `{"thinking": {"type": "disabled"}}` (Anthropic-style reasoning toggle)
    /// or `{"enable_thinking": false}` (DeepSeek/Qwen-style).
    pub async fn chat_completion_with_extras(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[ToolSpec]>,
        stream: bool,
        temperature: Option<f32>,
        max_tokens: Option<u32>,
        extras: serde_json::Map<String, serde_json::Value>,
    ) -> Result<LlmResponse, AishError> {
        let mut body = serde_json::json!({
            "model": self.model.lock().unwrap().clone(),
            "messages": messages,
            "stream": stream,
        });

        if let Some(temp) = temperature {
            body["temperature"] = serde_json::json!(temp);
        }
        body["max_tokens"] = serde_json::json!(effective_max_tokens(max_tokens));
        if let Some(tools) = tools {
            body["tools"] = serde_json::json!(tools);
            body["tool_choice"] = serde_json::json!("auto");
        }
        if let serde_json::Value::Object(body_map) = &mut body {
            for (k, v) in extras {
                body_map.insert(k, v);
            }
        }

        let url = format!("{}/chat/completions", self.api_base);
        let resp = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .timeout(std::time::Duration::from_secs(120))
            .json(&body)
            .send()
            .await
            .map_err(|e| AishError::Llm(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(AishError::Llm(format_http_error(status, &text)));
        }

        if stream {
            Ok(LlmResponse::Stream(LlmStream::from_http(resp)))
        } else {
            let text = resp
                .text()
                .await
                .map_err(|e| AishError::Llm(e.to_string()))?;
            let json: serde_json::Value = serde_json::from_str(&text)
                .map_err(|e| AishError::Llm(format!("JSON parse error: {}", e)))?;
            Ok(LlmResponse::Json(json))
        }
    }
}

fn effective_max_tokens(max_tokens: Option<u32>) -> u32 {
    match max_tokens {
        None | Some(0) => DEFAULT_MAX_TOKENS,
        Some(n) => n,
    }
}

/// Format an HTTP error response into a user-friendly error message.
fn format_http_error(status: reqwest::StatusCode, body: &str) -> String {
    let hint = match status.as_u16() {
        401 | 403 => "Authentication failed. Please check your API key.".to_string(),
        404 => "Model not found. The model name may be incorrect or the API endpoint may not support it.".to_string(),
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

/// Response from the LLM API, either a complete JSON body or a streaming response.
pub enum LlmResponse {
    Json(serde_json::Value),
    Stream(LlmStream),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_client_fields() {
        let client = LlmClient::new("https://api.openai.com/v1", "sk-test-key", "gpt-4o");
        assert_eq!(client.model_name(), "gpt-4o");
        assert_eq!(client.api_base(), "https://api.openai.com/v1");
        assert_eq!(client.api_key(), "sk-test-key");
    }

    #[test]
    fn test_update_model() {
        let client = LlmClient::new("https://api.example.com/v1", "sk-test", "gpt-4");
        assert_eq!(client.model_name(), "gpt-4");
        client.update_model("gpt-4o");
        assert_eq!(client.model_name(), "gpt-4o");
    }

    #[test]
    fn test_new_client_strips_provider_prefix() {
        let client = LlmClient::new("https://api.openai.com/v1", "sk-test", "openai/gpt-4o");
        assert_eq!(client.model_name(), "gpt-4o");
    }

    #[test]
    fn test_new_client_preserves_prefix_for_non_openai() {
        let client = LlmClient::new(
            "https://gateway.example.com/v1",
            "sk-test",
            "provider/model-name",
        );
        assert_eq!(client.model_name(), "provider/model-name");
    }

    #[test]
    fn test_new_client_strips_openai_prefix_for_custom_gateway() {
        let client = LlmClient::new(
            "http://www.aishell.ai:8080/aitest/v1",
            "sk-test",
            "openai/gpt-5.1",
        );
        assert_eq!(client.model_name(), "gpt-5.1");
    }

    #[test]
    fn test_new_client_preserves_prefix_for_openrouter() {
        let client = LlmClient::new("https://openrouter.ai/api/v1", "sk-test", "openai/gpt-4o");
        assert_eq!(client.model_name(), "openai/gpt-4o");
    }

    #[test]
    fn test_update_model_preserves_prefix_for_non_openai() {
        let client = LlmClient::new("https://gateway.example.com/v1", "sk-test", "gpt-4");
        client.update_model("provider/model-name");
        assert_eq!(client.model_name(), "provider/model-name");
    }

    #[test]
    fn test_new_with_retry_returns_client() {
        // Even with unreachable server, should return a client
        let client = LlmClient::new_with_retry("http://127.0.0.1:1/v1", "sk-test", "gpt-4o", 1);
        assert_eq!(client.model_name(), "gpt-4o");
    }

    #[test]
    fn test_llm_client_api_base_trimming() {
        let client = LlmClient::new("https://api.openai.com/v1/", "sk-test", "gpt-4o");
        assert_eq!(client.api_base(), "https://api.openai.com/v1");
    }

    #[test]
    fn test_provider_prefix_stripping() {
        let client1 = LlmClient::new("https://api.openai.com/v1", "sk-test", "openai/gpt-4o");
        assert_eq!(client1.model_name(), "gpt-4o");

        let client2 = LlmClient::new(
            "https://api.openai.com/v1",
            "sk-test",
            "anthropic/claude-3-opus",
        );
        assert_eq!(client2.model_name(), "claude-3-opus");

        let client3 = LlmClient::new("https://api.openai.com/v1", "sk-test", "google/gemini-pro");
        assert_eq!(client3.model_name(), "gemini-pro");

        let client4 = LlmClient::new(
            "https://api.openai.com/v1",
            "sk-test",
            "deepseek/deepseek-coder",
        );
        assert_eq!(client4.model_name(), "deepseek-coder");
    }

    #[test]
    fn test_format_http_error_404() {
        let msg = format_http_error(reqwest::StatusCode::NOT_FOUND, "Sorry, Page Not Found");
        assert!(msg.contains("404"));
        assert!(msg.contains("Model not found"));
    }

    #[test]
    fn test_format_http_error_401() {
        let msg = format_http_error(reqwest::StatusCode::UNAUTHORIZED, "Invalid API key");
        assert!(msg.contains("401"));
        assert!(msg.contains("Authentication failed"));
    }

    #[test]
    fn test_format_http_error_429() {
        let msg = format_http_error(
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            "Rate limit exceeded",
        );
        assert!(msg.contains("429"));
        assert!(msg.contains("Rate limited"));
    }

    #[test]
    fn test_format_http_error_500() {
        let msg = format_http_error(reqwest::StatusCode::INTERNAL_SERVER_ERROR, "Internal error");
        assert!(msg.contains("500"));
        assert!(msg.contains("Server error"));
    }

    #[test]
    fn test_effective_max_tokens() {
        assert_eq!(effective_max_tokens(None), DEFAULT_MAX_TOKENS);
        assert_eq!(effective_max_tokens(Some(0)), DEFAULT_MAX_TOKENS);
        assert_eq!(effective_max_tokens(Some(1000)), 1000);
    }

    #[test]
    fn test_format_http_error_other() {
        let msg = format_http_error(reqwest::StatusCode::BAD_REQUEST, "Bad request");
        assert!(msg.contains("400"));
        assert!(msg.contains("Bad request"));
        assert!(!msg.contains("Authentication"));
    }
}
