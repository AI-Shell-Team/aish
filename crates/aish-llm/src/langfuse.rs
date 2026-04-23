//! Optional Langfuse observability integration.
//!
//! Provides best-effort tracing of LLM sessions, generation spans, and tool call spans.
//! All methods are async and non-blocking — errors are logged but never propagated to callers.
//!
//! Internally delegates to the `langfuse-ergonomic` crate for HTTP communication with the
//! Langfuse ingestion API.

use std::sync::Arc;

use serde_json::json;
use tracing::warn;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for the Langfuse observability client.
#[derive(Debug, Clone)]
pub struct LangfuseConfig {
    pub enabled: bool,
    pub public_key: String,
    pub secret_key: String,
    pub base_url: String,
}

impl LangfuseConfig {
    /// Build a LangfuseConfig from the optional fields in the application config.
    ///
    /// Environment variables `LANGFUSE_PUBLIC_KEY`, `LANGFUSE_SECRET_KEY`, and
    /// `LANGFUSE_BASE_URL` take priority over the passed-in values.
    ///
    /// Returns `None` if both the public key and secret key are missing.
    pub fn from_parts(
        public_key: Option<&str>,
        secret_key: Option<&str>,
        host: Option<&str>,
    ) -> Option<Self> {
        // Env vars take priority over passed-in config values
        let public_key = std::env::var("LANGFUSE_PUBLIC_KEY")
            .ok()
            .or_else(|| public_key.map(|s| s.to_string()));
        let secret_key = std::env::var("LANGFUSE_SECRET_KEY")
            .ok()
            .or_else(|| secret_key.map(|s| s.to_string()));

        let public_key = public_key?;
        let secret_key = secret_key?;
        if public_key.is_empty() || secret_key.is_empty() {
            return None;
        }

        let base_url = std::env::var("LANGFUSE_BASE_URL")
            .ok()
            .or_else(|| host.map(|h| h.trim_end_matches('/').to_string()))
            .unwrap_or_else(|| "https://cloud.langfuse.com".to_string());

        Some(Self {
            enabled: true,
            public_key,
            secret_key,
            base_url,
        })
    }
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// Best-effort Langfuse client backed by `langfuse-ergonomic`.
///
/// All public methods swallow errors and log warnings instead of propagating them.
/// Each method is async but returns quickly because the actual HTTP work is
/// fire-and-forget (spawned on the Tokio runtime).
#[derive(Clone)]
pub struct LangfuseClient {
    inner: Arc<langfuse_ergonomic::LangfuseClient>,
}

impl std::fmt::Debug for LangfuseClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LangfuseClient")
            .field("inner", &"langfuse_ergonomic::LangfuseClient")
            .finish()
    }
}

impl LangfuseClient {
    /// Create a new Langfuse client from the given configuration.
    ///
    /// Panics if the underlying `langfuse-ergonomic` client cannot be built
    /// (e.g. invalid URL). This should only happen during initial setup.
    pub fn new(config: LangfuseConfig) -> Self {
        let inner = langfuse_ergonomic::ClientBuilder::new()
            .public_key(&config.public_key)
            .secret_key(&config.secret_key)
            .base_url(&config.base_url)
            .build()
            .expect("Failed to create Langfuse client: invalid configuration");
        Self {
            inner: Arc::new(inner),
        }
    }

    /// Create a trace for a session and return the trace ID immediately.
    ///
    /// The trace ID is a pre-generated UUID so callers can use it right away.
    /// The actual HTTP ingestion is fire-and-forget.
    pub async fn trace_session(&self, session_id: &str, metadata: &serde_json::Value) -> String {
        let trace_id = Uuid::new_v4().to_string();
        let name = format!("session-{}", session_id);
        let client = self.inner.clone();
        let id_for_call = trace_id.clone();
        let metadata_clone = metadata.clone();

        tokio::spawn(async move {
            if let Err(e) = client
                .trace()
                .id(&id_for_call)
                .name(&name)
                .metadata(metadata_clone)
                .call()
                .await
            {
                warn!("Langfuse trace creation failed: {}", e);
            }
        });

        trace_id
    }

    /// Log a generation span under an existing trace.
    ///
    /// `input` accepts a serialized value (typically the full message list)
    /// so the Langfuse dashboard shows the complete conversation context.
    ///
    /// Token usage is embedded in metadata since langfuse-ergonomic v0.6.x
    /// does not natively wire usage through the generation builder.
    /// The call is fire-and-forget; errors are logged as warnings.
    pub async fn span_generation(
        &self,
        trace_id: &str,
        model: &str,
        input: serde_json::Value,
        output: &str,
        prompt_tokens: u64,
        completion_tokens: u64,
    ) {
        let client = self.inner.clone();
        let trace_id = trace_id.to_string();
        let model = model.to_string();
        let output_val = json!(output);
        let meta = if prompt_tokens > 0 || completion_tokens > 0 {
            json!({
                "usage": {
                    "prompt_tokens": prompt_tokens,
                    "completion_tokens": completion_tokens,
                    "total_tokens": prompt_tokens + completion_tokens,
                }
            })
        } else {
            json!({})
        };

        tokio::spawn(async move {
            if let Err(e) = client
                .generation()
                .trace_id(&trace_id)
                .name("generation")
                .model(&model)
                .input(input)
                .output(output_val)
                .metadata(meta)
                .call()
                .await
            {
                warn!("Langfuse generation span failed: {}", e);
            }
        });
    }

    /// Log a tool-call span under an existing trace.
    ///
    /// The call is fire-and-forget; errors are logged as warnings.
    pub async fn span_tool_call(
        &self,
        trace_id: &str,
        tool_name: &str,
        args: &str,
        result: &str,
        duration_ms: u64,
    ) {
        let client = self.inner.clone();
        let trace_id = trace_id.to_string();
        let name = format!("tool-{}", tool_name);
        let input_val = json!(args);
        let output_val = json!(result);
        let meta = json!({ "duration_ms": duration_ms });

        tokio::spawn(async move {
            if let Err(e) = client
                .span()
                .trace_id(&trace_id)
                .name(&name)
                .input(input_val)
                .output(output_val)
                .metadata(meta)
                .call()
                .await
            {
                warn!("Langfuse tool call span failed: {}", e);
            }
        });
    }

    /// Flush any pending events.
    ///
    /// With the direct API approach each call is already sent immediately,
    /// so this is effectively a no-op. The method is kept for API compatibility.
    pub async fn flush(&self) {
        // No-op: direct API calls are already sent
    }

    /// Shut down the client gracefully.
    ///
    /// With the direct API approach there is no background batcher to flush,
    /// so this is a no-op. The method exists for future compatibility.
    pub async fn shutdown(&self) {
        // No-op: no background batcher to shut down
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_from_parts_with_values() {
        let config = LangfuseConfig::from_parts(
            Some("pk-test"),
            Some("sk-test"),
            Some("https://langfuse.example.com"),
        );
        assert!(config.is_some());
        let config = config.unwrap();
        assert_eq!(config.public_key, "pk-test");
        assert_eq!(config.secret_key, "sk-test");
        assert_eq!(config.base_url, "https://langfuse.example.com");
        assert!(config.enabled);
    }

    #[test]
    fn test_config_from_parts_missing_keys() {
        assert!(LangfuseConfig::from_parts(None, Some("sk-test"), None).is_none());
        assert!(LangfuseConfig::from_parts(Some("pk-test"), None, None).is_none());
        assert!(LangfuseConfig::from_parts(None, None, None).is_none());
    }

    #[test]
    fn test_config_from_parts_empty_keys() {
        assert!(LangfuseConfig::from_parts(Some(""), Some("sk-test"), None).is_none());
        assert!(LangfuseConfig::from_parts(Some("pk-test"), Some(""), None).is_none());
    }

    #[test]
    fn test_config_default_base_url() {
        let config = LangfuseConfig::from_parts(Some("pk-test"), Some("sk-test"), None).unwrap();
        assert_eq!(config.base_url, "https://cloud.langfuse.com");
    }

    #[test]
    fn test_config_strips_trailing_slash() {
        let config = LangfuseConfig::from_parts(
            Some("pk-test"),
            Some("sk-test"),
            Some("https://langfuse.example.com/"),
        )
        .unwrap();
        assert_eq!(config.base_url, "https://langfuse.example.com");
    }
}
