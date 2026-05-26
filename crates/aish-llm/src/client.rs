use aish_core::AishError;
use reqwest::Client;

use crate::types::{ChatMessage, ToolSpec};

/// HTTP client using litellm-rs for multi-provider LLM support.
///
/// LiteLLMClient wraps litellm-rs to support 100+ LLM providers while maintaining
/// compatibility with our existing ChatMessage and ToolSpec types.
///
/// For tool calling scenarios, this client falls back to direct reqwest API calls
/// since litellm-rs lite mode doesn't support native tool calling.
pub struct LiteLLMClient {
    api_base: Option<String>,
    api_key: Option<String>,
    model: String,
}

impl LiteLLMClient {
    /// Create a new LiteLLMClient.
    ///
    /// The model name can include a provider prefix (e.g., "openai/gpt-4o") which will be
    /// stripped automatically. API keys are read from environment variables by litellm-rs.
    pub fn new(api_base: Option<&str>, api_key: Option<&str>, model: &str) -> Self {
        // Strip LiteLLM-style provider prefix (e.g. "openai/gpt-5.1" → "gpt-5.1")
        let model = match model.split_once('/') {
            Some((_provider, name)) => name.to_string(),
            None => model.to_string(),
        };
        Self {
            api_base: api_base.map(|s| s.trim_end_matches('/').to_string()),
            api_key: api_key.map(|s| s.to_string()),
            model,
        }
    }

    /// Return the API base URL, if provided.
    pub fn api_base(&self) -> Option<&str> {
        self.api_base.as_deref()
    }

    /// Return the API key, if provided.
    pub fn api_key(&self) -> Option<&str> {
        self.api_key.as_deref()
    }

    /// Return the model name used for this client.
    pub fn model_name(&self) -> &str {
        &self.model
    }

    /// Test connectivity by sending a lightweight request.
    pub async fn test_connection(&self) -> Result<(), String> {
        // For litellm-rs, we can test by making a minimal completion request
        let messages = vec![litellm_rs::user_message("hi")];
        let result = litellm_rs::completion(&self.model, messages, None).await;

        match result {
            Ok(_) => Ok(()),
            Err(e) => {
                // Check if it's an auth error (server is reachable but credentials failed)
                let err_msg = e.to_string().to_lowercase();
                if err_msg.contains("401")
                    || err_msg.contains("403")
                    || err_msg.contains("unauthorized")
                {
                    Ok(())
                } else {
                    Err(format!("Connection failed: {}", e))
                }
            }
        }
    }

    /// Convert our ChatMessage to litellm_rs message format.
    fn convert_message(msg: &ChatMessage) -> litellm_rs::Message {
        match msg.role.as_str() {
            "system" => litellm_rs::system_message(msg.content.as_deref().unwrap_or("")),
            "user" => litellm_rs::user_message(msg.content.as_deref().unwrap_or("")),
            "assistant" => litellm_rs::assistant_message(msg.content.as_deref().unwrap_or("")),
            _ => litellm_rs::user_message(msg.content.as_deref().unwrap_or("")),
        }
    }

    /// Send a non-streaming chat completion request.
    ///
    /// Falls back to reqwest for tool calling since litellm-rs lite mode doesn't support it.
    pub async fn chat_completion(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[ToolSpec]>,
        temperature: Option<f32>,
        max_tokens: Option<u32>,
    ) -> Result<LlmResponse, AishError> {
        // If tools are requested, fall back to direct API call
        if tools.is_some() && tools.unwrap().len() > 0 {
            return self
                .fallback_chat_completion(messages, tools, false, temperature, max_tokens)
                .await;
        }

        // Convert messages to litellm_rs format
        let litellm_messages: Vec<litellm_rs::Message> =
            messages.iter().map(Self::convert_message).collect();

        // Build optional parameters using CompletionOptions
        let litellm_options = if temperature.is_some() || max_tokens.is_some() {
            let mut opts = litellm_rs::CompletionOptions::default();
            opts.temperature = temperature;
            opts.max_tokens = max_tokens;
            Some(opts)
        } else {
            None
        };

        // Call litellm-rs
        let response = litellm_rs::completion(&self.model, litellm_messages, litellm_options)
            .await
            .map_err(|e| AishError::Llm(format!("LiteLLM completion error: {}", e)))?;

        // Convert response to our format
        let json = serde_json::to_value(&response)
            .map_err(|e| AishError::Llm(format!("JSON serialization error: {}", e)))?;

        Ok(LlmResponse::Json(json))
    }

    /// Send a streaming chat completion request.
    ///
    /// Falls back to reqwest for tool calling since litellm-rs lite mode doesn't support it.
    pub async fn chat_completion_stream(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[ToolSpec]>,
        temperature: Option<f32>,
        max_tokens: Option<u32>,
    ) -> Result<LlmResponse, AishError> {
        // For now, always fall back to direct API call for streaming
        // to maintain compatibility with our existing StreamParser
        self.fallback_chat_completion(messages, tools, true, temperature, max_tokens)
            .await
    }

    /// Fallback to direct reqwest API call for tool calling scenarios.
    async fn fallback_chat_completion(
        &self,
        messages: &[ChatMessage],
        tools: Option<&[ToolSpec]>,
        stream: bool,
        temperature: Option<f32>,
        max_tokens: Option<u32>,
    ) -> Result<LlmResponse, AishError> {
        // Determine API base - default to OpenAI if not provided
        let api_base = self
            .api_base
            .as_deref()
            .unwrap_or("https://api.openai.com/v1");

        // Determine API key - try environment variable if not provided
        let api_key = self.api_key.clone().unwrap_or_else(|| {
            std::env::var("OPENAI_API_KEY").expect("No API key provided and OPENAI_API_KEY not set")
        });

        let mut body = serde_json::json!({
            "model": self.model,
            "messages": messages,
            "stream": stream,
        });

        if let Some(temp) = temperature {
            body["temperature"] = serde_json::json!(temp);
        }
        if let Some(tokens) = max_tokens {
            body["max_tokens"] = serde_json::json!(tokens);
        }
        if let Some(tools) = tools {
            body["tools"] = serde_json::json!(tools);
            body["tool_choice"] = serde_json::json!("auto");
        }

        let url = format!("{}/chat/completions", api_base);
        let client = Client::new();
        let resp = client
            .post(&url)
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Content-Type", "application/json")
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
            Ok(LlmResponse::Stream(resp))
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

/// HTTP client for OpenAI-compatible chat completion APIs.
pub struct LlmClient {
    http: Client,
    api_base: String,
    api_key: String,
    model: String,
}

impl LlmClient {
    pub fn new(api_base: &str, api_key: &str, model: &str) -> Self {
        // Strip LiteLLM-style provider prefix (e.g. "openai/gpt-5.1" → "gpt-5.1")
        let model = match model.split_once('/') {
            Some((_provider, name)) => name.to_string(),
            None => model.to_string(),
        };
        Self {
            http: Client::new(),
            api_base: api_base.trim_end_matches('/').into(),
            api_key: api_key.into(),
            model,
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
    pub fn model_name(&self) -> &str {
        &self.model
    }

    /// Update the model name (used for runtime model switching).
    pub fn update_model(&mut self, model: &str) {
        let model = match model.split_once('/') {
            Some((_provider, name)) => name.to_string(),
            None => model.to_string(),
        };
        self.model = model;
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
        let mut body = serde_json::json!({
            "model": self.model,
            "messages": messages,
            "stream": stream,
        });

        if let Some(temp) = temperature {
            body["temperature"] = serde_json::json!(temp);
        }
        if let Some(tokens) = max_tokens {
            body["max_tokens"] = serde_json::json!(tokens);
        }
        if let Some(tools) = tools {
            body["tools"] = serde_json::json!(tools);
            body["tool_choice"] = serde_json::json!("auto");
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
            Ok(LlmResponse::Stream(resp))
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
    Stream(reqwest::Response),
    // Note: For now we don't expose a separate LitellmStream variant.
    // We fall back to reqwest streaming for all cases to maintain compatibility.
    // In the future, we could add dedicated litellm stream handling.
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
        let mut client = LlmClient::new("https://api.example.com/v1", "sk-test", "gpt-4");
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
    fn test_new_with_retry_returns_client() {
        // Even with unreachable server, should return a client
        let client = LlmClient::new_with_retry("http://127.0.0.1:1/v1", "sk-test", "gpt-4o", 1);
        assert_eq!(client.model_name(), "gpt-4o");
    }

    // LiteLLMClient tests
    #[test]
    fn test_litellm_client_construction() {
        let client =
            LiteLLMClient::new(Some("https://api.openai.com/v1"), Some("sk-test"), "gpt-4o");
        assert_eq!(client.model_name(), "gpt-4o");
        assert_eq!(client.api_base(), Some("https://api.openai.com/v1"));
        assert_eq!(client.api_key(), Some("sk-test"));
    }

    #[test]
    fn test_litellm_client_optional_fields() {
        let client = LiteLLMClient::new(None, None, "gpt-4o");
        assert_eq!(client.model_name(), "gpt-4o");
        assert_eq!(client.api_base(), None);
        assert_eq!(client.api_key(), None);
    }

    #[test]
    fn test_litellm_client_strips_provider_prefix() {
        let client = LiteLLMClient::new(None, None, "openai/gpt-4o");
        assert_eq!(client.model_name(), "gpt-4o");

        let client2 = LiteLLMClient::new(None, None, "anthropic/claude-3-opus");
        assert_eq!(client2.model_name(), "claude-3-opus");
    }

    #[test]
    fn test_litellm_message_conversion() {
        let sys_msg = ChatMessage::system("You are a helpful assistant");
        let _litellm_sys = LiteLLMClient::convert_message(&sys_msg);
        // The message should be converted - we can't inspect internals but it should not panic

        let user_msg = ChatMessage::user("Hello");
        let _litellm_user = LiteLLMClient::convert_message(&user_msg);

        let asst_msg = ChatMessage::assistant("Hi there");
        let _litellm_asst = LiteLLMClient::convert_message(&asst_msg);
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
    fn test_format_http_error_other() {
        let msg = format_http_error(reqwest::StatusCode::BAD_REQUEST, "Bad request");
        assert!(msg.contains("400"));
        assert!(msg.contains("Bad request"));
        assert!(!msg.contains("Authentication"));
    }
}
