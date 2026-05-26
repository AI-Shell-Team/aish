//! Provider trait and data types for the adapter pattern.
//!
//! Each provider implements `ProviderAdapter` to supply metadata, capabilities,
//! and model/API-base matching logic. The actual LLM API calls remain in
//! `LlmClient`; this layer only handles routing and configuration.

use std::fmt;

/// Static metadata that describes an LLM provider.
#[derive(Debug, Clone)]
pub struct ProviderMetadata {
    /// Unique identifier for the provider (e.g. "openai-compat", "anthropic").
    pub provider_id: String,
    /// Human-readable name shown in UI and logs.
    pub display_name: String,
    /// URL of the provider dashboard / usage page.
    pub dashboard_url: Option<String>,
    /// Environment variable name that holds the API key.
    pub api_key_env_var: String,
    /// Whether the provider supports SSE streaming.
    pub supports_streaming: bool,
    /// Whether the provider supports tool/function calling.
    pub supports_tools: bool,
    /// Whether this provider requires a custom HTTP client instead of the
    /// generic OpenAI-compatible one.
    pub uses_custom_client: bool,
}

/// Capability flags that a provider can override from defaults.
#[derive(Debug, Clone)]
pub struct ProviderCapabilities {
    /// Provider supports streaming responses.
    pub supports_streaming: bool,
    /// Provider supports tool/function calling.
    pub supports_tools: bool,
    /// Whether to trim older messages when context window is exceeded.
    pub should_trim_messages: bool,
}

impl Default for ProviderCapabilities {
    fn default() -> Self {
        Self {
            supports_streaming: true,
            supports_tools: true,
            should_trim_messages: true,
        }
    }
}

/// Authentication-related configuration for a provider.
#[derive(Debug, Clone)]
pub struct ProviderAuthConfig {
    /// Key inside the auth config file where credentials are stored.
    pub auth_path_config_key: String,
    /// Default model identifier for this provider.
    pub default_model: String,
    /// Supported auth flows (e.g. "api_key", "oauth").
    pub supported_flows: Vec<String>,
}

/// Trait that each provider adapter must implement.
///
/// All methods are synchronous because the adapter only provides metadata and
/// routing information -- actual API calls are handled by `LlmClient`.
pub trait ProviderAdapter: Send + Sync {
    /// Unique identifier for this provider.
    fn provider_id(&self) -> &str;

    /// Human-readable display name.
    fn display_name(&self) -> &str;

    /// Full metadata for this provider.
    fn metadata(&self) -> ProviderMetadata;

    /// Capability flags.
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    /// Return `true` if this provider should handle the given model name.
    fn matches_model(&self, model: &str) -> bool;

    /// Return `true` if this provider should handle the given API base URL.
    fn matches_api_base(&self, api_base: &str) -> bool;

    /// Optional authentication configuration.
    fn auth_config(&self) -> Option<ProviderAuthConfig> {
        None
    }
}

/// Blanket implementation so `Arc<dyn ProviderAdapter>` can be formatted.
impl fmt::Debug for dyn ProviderAdapter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderAdapter")
            .field("provider_id", &self.provider_id())
            .field("display_name", &self.display_name())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_capabilities_default() {
        let caps = ProviderCapabilities::default();
        assert!(caps.supports_streaming);
        assert!(caps.supports_tools);
        assert!(caps.should_trim_messages);
    }
}
