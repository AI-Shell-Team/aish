use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::time::Instant;

use aish_llm::{
    ChatMessage, LlmResponse, LlmSession, PreflightResult, PreflightSecurityContext,
    SecurityPanelMode, StreamParser, Tool, ToolResult,
};
use reqwest::Url;

use super::preapproved::is_preapproved_host;
use super::prompt;
use super::utils::{
    fetch_url_content, format_redirect_message, make_secondary_model_prompt, truncate_for_model,
    validate_and_normalize_url, FetchFailure, FetchedContent,
};

const TOOL_NAME: &str = "WebFetch";

/// Fetch a URL, extract readable text, and answer a focused prompt about it.
pub struct WebFetchTool {
    api_base: String,
    api_key: String,
    model: String,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
}

impl WebFetchTool {
    pub fn new(
        api_base: &str,
        api_key: &str,
        model: &str,
        temperature: Option<f32>,
        max_tokens: Option<u32>,
    ) -> Self {
        Self {
            api_base: api_base.to_string(),
            api_key: api_key.to_string(),
            model: model.to_string(),
            temperature,
            max_tokens,
        }
    }

    async fn fetch_url_content(&self, raw_url: &str) -> Result<FetchedContent, FetchFailure> {
        fetch_url_content(raw_url).await
    }

    async fn apply_prompt_to_content(
        &self,
        prompt: &str,
        content: &str,
        is_preapproved_domain: bool,
    ) -> Result<String, String> {
        let model_prompt = make_secondary_model_prompt(content, prompt, is_preapproved_domain);
        let session = LlmSession::new(
            &self.api_base,
            &self.api_key,
            &self.model,
            self.temperature.or(Some(0.1)),
            self.max_tokens.or(Some(2048)),
        );
        let temperature = self.temperature.or(Some(0.1));
        let max_tokens = self.max_tokens.or(Some(2048));
        let messages = vec![ChatMessage::user(model_prompt)];
        match session
            .chat_completion_raw(&messages, None, false, temperature, max_tokens)
            .await
            .map_err(|error| error.to_string())?
        {
            LlmResponse::Json(json) => {
                let (content, _reasoning, _tool_calls, _usage) =
                    StreamParser::parse_response(&json);
                Ok(content.unwrap_or_else(|| "No response from model".to_string()))
            }
            LlmResponse::Stream(_) => Err("secondary model unexpectedly returned a stream".into()),
        }
    }
}

impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        TOOL_NAME
    }

    fn description(&self) -> &str {
        prompt::DESCRIPTION
    }

    fn parameters(&self) -> serde_json::Value {
        prompt::parameters()
    }

    fn prompt(&self) -> &str {
        prompt::PROMPT
    }

    fn preflight(&self, args: &serde_json::Value) -> PreflightResult {
        let url = match args.get("url").and_then(|value| value.as_str()) {
            Some(value) if !value.trim().is_empty() => value,
            _ => {
                return PreflightResult::Block {
                    message: aish_i18n::t("tools.web_fetch.missing_url"),
                    security: Some(PreflightSecurityContext::fallback(
                        TOOL_NAME,
                        None,
                        aish_i18n::t("tools.web_fetch.missing_url"),
                        SecurityPanelMode::Blocked,
                    )),
                }
            }
        };

        let normalized = match validate_and_normalize_url(url) {
            Ok(parsed) => parsed,
            Err(message) => {
                return PreflightResult::Block {
                    message: message.clone(),
                    security: Some(PreflightSecurityContext::fallback(
                        TOOL_NAME,
                        Some(url.to_string()),
                        message,
                        SecurityPanelMode::Blocked,
                    )),
                }
            }
        };

        let hostname = normalized.host_str().unwrap_or("");
        if is_preapproved_host(hostname, normalized.path()) {
            return PreflightResult::Allow;
        }

        let host = super::preapproved::host_key(hostname);
        let message = aish_i18n::t_with_args(
            "tools.web_fetch.confirm_fetch",
            &HashMap::from([("host".to_string(), host)]),
        );
        PreflightResult::Confirm {
            message: message.clone(),
            security: Some(PreflightSecurityContext::fallback(
                TOOL_NAME,
                Some(url.to_string()),
                message,
                SecurityPanelMode::Confirm,
            )),
        }
    }

    fn approval_memory_key(&self, args: &serde_json::Value) -> Option<String> {
        let url = args.get("url")?.as_str()?;
        let parsed = validate_and_normalize_url(url).ok()?;
        parsed.host_str().map(super::preapproved::host_key)
    }

    fn execute(&self, _args: serde_json::Value) -> ToolResult {
        ToolResult::error("WebFetch requires async execution; use execute_async")
    }

    fn execute_async<'a>(
        &'a self,
        args: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        Box::pin(async move {
            let url = match args.get("url").and_then(|value| value.as_str()) {
                Some(value) if !value.trim().is_empty() => value.trim(),
                _ => return ToolResult::error(aish_i18n::t("tools.web_fetch.missing_url")),
            };
            let prompt = match args.get("prompt").and_then(|value| value.as_str()) {
                Some(value) if !value.trim().is_empty() => value.trim(),
                _ => return ToolResult::error(aish_i18n::t("tools.web_fetch.missing_prompt")),
            };

            let start = Instant::now();
            let fetched = match self.fetch_url_content(url).await {
                Ok(content) => content,
                Err(FetchFailure::Redirect(info)) => {
                    let message = format_redirect_message(&info, prompt);
                    return ToolResult {
                        ok: true,
                        output: message.clone(),
                        meta: Some(serde_json::json!({
                            "url": url,
                            "redirect_url": info.redirect_url,
                            "code": info.status_code,
                            "result": message,
                            "durationMs": start.elapsed().as_millis() as u64,
                        })),
                    };
                }
                Err(FetchFailure::Blocked(message)) | Err(FetchFailure::Request(message)) => {
                    return ToolResult::error(message)
                }
            };

            let truncated_content = truncate_for_model(&fetched.content);
            let parsed_url = Url::parse(&fetched.url).ok();
            let is_preapproved_domain = parsed_url
                .as_ref()
                .and_then(|parsed| parsed.host_str().map(|host| (host, parsed.path())))
                .is_some_and(|(host, path)| is_preapproved_host(host, path));

            let result = match self
                .apply_prompt_to_content(prompt, &truncated_content, is_preapproved_domain)
                .await
            {
                Ok(result) => result,
                Err(error) => {
                    let mut args_map = HashMap::new();
                    args_map.insert("error".to_string(), error);
                    return ToolResult::error(aish_i18n::t_with_args(
                        "tools.web_fetch.secondary_model_failed",
                        &args_map,
                    ));
                }
            };

            let duration_ms = start.elapsed().as_millis() as u64;
            let output = format!(
                "Fetched: {}\nStatus: {} {}\nBytes: {}\nDuration: {}ms\nCached: {}\n\n{}",
                fetched.url,
                fetched.code,
                fetched.code_text,
                fetched.bytes,
                duration_ms,
                fetched.from_cache,
                result
            );

            ToolResult {
                ok: true,
                output,
                meta: Some(serde_json::json!({
                    "url": fetched.url,
                    "code": fetched.code,
                    "codeText": fetched.code_text,
                    "bytes": fetched.bytes,
                    "contentType": fetched.content_type,
                    "durationMs": duration_ms,
                    "fromCache": fetched.from_cache,
                    "result": result,
                })),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore]
    async fn live_fetch_url() {
        if std::env::var("AISH_LIVE_WEBFETCH").ok().as_deref() != Some("1") {
            eprintln!("set AISH_LIVE_WEBFETCH=1 to run this live network smoke test");
            return;
        }

        let url = std::env::var("AISH_LIVE_WEBFETCH_URL")
            .unwrap_or_else(|_| "https://github.com/mattpocock/skills".to_string());
        let expected = std::env::var("AISH_LIVE_WEBFETCH_EXPECT").ok();
        let tool = WebFetchTool::new("", "", "", Some(0.1), Some(256));
        let fetched = tool
            .fetch_url_content(&url)
            .await
            .expect("expected live page fetch to succeed");

        println!(
            "fetched {} status={} bytes={} content_type={} chars={}",
            fetched.url,
            fetched.code,
            fetched.bytes,
            fetched.content_type,
            fetched.content.len()
        );
        println!(
            "preview:\n{}",
            truncate_for_model(&fetched.content)
                .chars()
                .take(800)
                .collect::<String>()
        );

        assert_eq!(fetched.code, 200);
        if let Some(expected) = expected {
            assert!(
                fetched
                    .content
                    .to_ascii_lowercase()
                    .contains(&expected.to_ascii_lowercase()),
                "expected fetched content to contain {expected:?}"
            );
        }
    }

    fn tool() -> WebFetchTool {
        WebFetchTool::new("", "", "", Some(0.1), Some(256))
    }

    #[test]
    fn preflight_allows_preapproved_host_key() {
        let args = serde_json::json!({
            "url": "https://WWW.docs.python.org./3/",
            "prompt": "summarize"
        });
        assert_eq!(tool().preflight(&args), PreflightResult::Allow);
    }

    #[test]
    fn preflight_confirms_host_key_and_shows_full_url() {
        let url = "https://WWW.Example.COM/path?token=secret";
        let args = serde_json::json!({ "url": url, "prompt": "summarize" });
        match tool().preflight(&args) {
            PreflightResult::Confirm { message, security } => {
                assert!(message.contains("example.com"), "{message}");
                assert!(
                    !message.to_ascii_lowercase().contains("www.example.com"),
                    "{message}"
                );
                let security = security.expect("confirm should include security context");
                assert_eq!(security.target.as_deref(), Some(url));
            }
            other => panic!("expected Confirm, got {other:?}"),
        }
    }

    #[test]
    fn preflight_still_confirms_github_outside_anthropics() {
        let args = serde_json::json!({
            "url": "https://github.com/AI-Shell-Team/aish",
            "prompt": "summarize"
        });
        assert!(matches!(
            tool().preflight(&args),
            PreflightResult::Confirm { .. }
        ));
    }

    #[test]
    fn approval_memory_key_is_host_key() {
        let args = serde_json::json!({ "url": "https://WWW.Example.COM./a" });
        assert_eq!(
            tool().approval_memory_key(&args).as_deref(),
            Some("example.com")
        );
    }
}
