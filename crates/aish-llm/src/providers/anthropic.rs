//! Anthropic provider metadata adapter.

use super::types::{ProviderAdapter, ProviderMetadata};

pub const ANTHROPIC_PROVIDER: &str = "anthropic";

pub struct AnthropicProviderAdapter;

impl ProviderAdapter for AnthropicProviderAdapter {
    fn provider_id(&self) -> &str {
        ANTHROPIC_PROVIDER
    }

    fn display_name(&self) -> &str {
        "Anthropic"
    }

    fn metadata(&self) -> ProviderMetadata {
        ProviderMetadata {
            provider_id: ANTHROPIC_PROVIDER.to_string(),
            display_name: "Anthropic".to_string(),
            dashboard_url: Some("https://console.anthropic.com/".to_string()),
            api_key_env_var: "ANTHROPIC_API_KEY".to_string(),
            supports_streaming: true,
            supports_tools: true,
            uses_custom_client: true,
        }
    }

    fn matches_model(&self, _model: &str) -> bool {
        false
    }

    fn matches_api_base(&self, api_base: &str) -> bool {
        api_base.to_lowercase().contains("api.anthropic.com")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_matches_anthropic_api_base() {
        let adapter = AnthropicProviderAdapter;
        assert!(adapter.matches_api_base("https://api.anthropic.com"));
        assert!(!adapter.matches_api_base("https://openrouter.ai/api/v1"));
    }
}
