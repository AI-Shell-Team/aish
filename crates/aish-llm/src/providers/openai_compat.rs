//! Default OpenAI-compatible provider that acts as the universal fallback.
//!
//! This provider matches every model and API base, providing sensible defaults
//! for any endpoint that speaks the OpenAI chat-completions API.

use super::types::{ProviderCapabilities, ProviderMetadata};

/// The universal fallback provider for OpenAI-compatible endpoints.
pub struct OpenAiCompatProvider;

impl super::types::ProviderAdapter for OpenAiCompatProvider {
    fn provider_id(&self) -> &str {
        "openai-compat"
    }

    fn display_name(&self) -> &str {
        "OpenAI Compatible"
    }

    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            provider_id: self.provider_id().to_string(),
            display_name: self.display_name().to_string(),
            dashboard_url: None,
            api_key_env_var: "OPENAI_API_KEY".to_string(),
            supports_streaming: true,
            supports_tools: true,
            uses_custom_client: false,
        }
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities::default()
    }

    /// Always returns `true` -- this provider is the universal fallback.
    fn matches_model(&self, _model: &str) -> bool {
        true
    }

    /// Always returns `true` -- this provider is the universal fallback.
    fn matches_api_base(&self, _api_base: &str) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::types::ProviderAdapter;

    #[test]
    fn test_default_provider_matches_all() {
        let provider = OpenAiCompatProvider;
        assert!(provider.matches_model("gpt-4o"));
        assert!(provider.matches_model("claude-3-opus"));
        assert!(provider.matches_model("random-model"));
        assert!(provider.matches_api_base("https://api.openai.com/v1"));
        assert!(provider.matches_api_base("http://localhost:11434/v1"));
        assert!(provider.matches_api_base("https://anything.example.com"));
    }

    #[test]
    fn test_default_provider_metadata() {
        let provider = OpenAiCompatProvider;
        assert_eq!(provider.provider_id(), "openai-compat");
        assert_eq!(provider.display_name(), "OpenAI Compatible");

        let meta = provider.metadata();
        assert_eq!(meta.provider_id, "openai-compat");
        assert_eq!(meta.display_name, "OpenAI Compatible");
        assert!(meta.dashboard_url.is_none());
        assert_eq!(meta.api_key_env_var, "OPENAI_API_KEY");
        assert!(meta.supports_streaming);
        assert!(meta.supports_tools);
        assert!(!meta.uses_custom_client);
        assert!(provider.auth_config().is_none());
    }
}
