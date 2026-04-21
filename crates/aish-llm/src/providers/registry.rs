//! Provider registry that manages adapters and resolves the right one for a
//! given model or API base.

use std::sync::Arc;

use super::codex::CodexProviderAdapter;
use super::openai_compat::OpenAiCompatProvider;
use super::types::ProviderAdapter;

/// Registry that holds a list of provider adapters ordered by specificity.
///
/// The last registered adapter is typically the universal fallback
/// (`OpenAiCompatProvider`), which matches everything.
pub struct ProviderRegistry {
    providers: Vec<Arc<dyn ProviderAdapter>>,
}

impl ProviderRegistry {
    /// Create a new registry pre-loaded with Codex and OpenAI-compat adapters.
    pub fn new() -> Self {
        Self {
            providers: vec![
                Arc::new(CodexProviderAdapter),
                Arc::new(OpenAiCompatProvider),
            ],
        }
    }

    /// Register a custom provider adapter.
    ///
    /// The provider is inserted before the fallback, so it takes priority
    /// during matching.
    pub fn register(&mut self, provider: Arc<dyn ProviderAdapter>) {
        // Keep the fallback (last element) at the end.
        let len = self.providers.len();
        if len > 0 {
            self.providers.insert(len - 1, provider);
        } else {
            self.providers.push(provider);
        }
    }

    /// Find the best-matching provider for the given model name.
    ///
    /// Iterates providers in registration order (most specific first) and
    /// returns the first one whose `matches_model` returns `true`. Falls back
    /// to the last provider (should always be `OpenAiCompatProvider`).
    pub fn get_provider_for_model(&self, model: &str) -> Arc<dyn ProviderAdapter> {
        for provider in &self.providers {
            if provider.matches_model(model) {
                return Arc::clone(provider);
            }
        }
        // Fallback to last provider (OpenAiCompatProvider).
        Arc::clone(self.providers.last().unwrap())
    }

    /// Find the best-matching provider for the given API base URL.
    ///
    /// Same strategy as `get_provider_for_model` but uses `matches_api_base`.
    pub fn get_provider_for_api_base(&self, api_base: &str) -> Arc<dyn ProviderAdapter> {
        for provider in &self.providers {
            if provider.matches_api_base(api_base) {
                return Arc::clone(provider);
            }
        }
        Arc::clone(self.providers.last().unwrap())
    }

    /// Look up a provider by its unique identifier.
    pub fn get_provider_by_id(&self, id: &str) -> Option<Arc<dyn ProviderAdapter>> {
        self.providers
            .iter()
            .find(|p| p.provider_id() == id)
            .cloned()
    }

    /// Return the identifiers of all registered providers.
    pub fn list_provider_ids(&self) -> Vec<String> {
        self.providers
            .iter()
            .map(|p| p.provider_id().to_string())
            .collect()
    }

    /// Resolve rich metadata by combining the adapter lookup with the existing
    /// `detect_provider` function.
    ///
    /// Uses the adapter for routing/capabilities and `detect_provider` for
    /// provider-specific metadata (dashboard URL, etc.).
    pub fn resolve_metadata(&self, model: &str, api_base: &str) -> ResolvedProviderInfo {
        let adapter = self.get_provider_for_model(model);

        // Use the existing detection logic for rich provider metadata.
        let detected = crate::provider::detect_provider(model, api_base);

        ResolvedProviderInfo {
            adapter_id: adapter.provider_id().to_string(),
            adapter_display_name: adapter.display_name().to_string(),
            detected_provider_id: detected.id,
            detected_display_name: detected.display_name,
            dashboard_url: detected.dashboard_url,
            capabilities: adapter.capabilities(),
            auth_config: adapter.auth_config(),
        }
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Combined metadata returned by `resolve_metadata`.
#[derive(Debug, Clone)]
pub struct ResolvedProviderInfo {
    /// ID of the matched adapter.
    pub adapter_id: String,
    /// Display name from the adapter.
    pub adapter_display_name: String,
    /// Provider ID from `detect_provider` (may differ for fallback adapters).
    pub detected_provider_id: String,
    /// Display name from `detect_provider`.
    pub detected_display_name: String,
    /// Dashboard URL (if any).
    pub dashboard_url: Option<String>,
    /// Capabilities from the adapter.
    pub capabilities: super::types::ProviderCapabilities,
    /// Auth configuration (if the adapter provides one).
    pub auth_config: Option<super::types::ProviderAuthConfig>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::types::{ProviderAdapter, ProviderCapabilities, ProviderMetadata};

    /// A stub provider that matches only "test-*" models.
    struct TestProvider;

    impl ProviderAdapter for TestProvider {
        fn provider_id(&self) -> &str {
            "test-provider"
        }
        fn display_name(&self) -> &str {
            "Test Provider"
        }
        fn metadata(&self) -> ProviderMetadata {
            ProviderMetadata {
                provider_id: "test-provider".to_string(),
                display_name: "Test Provider".to_string(),
                dashboard_url: Some("https://test.example.com".to_string()),
                api_key_env_var: "TEST_API_KEY".to_string(),
                supports_streaming: true,
                supports_tools: false,
                uses_custom_client: false,
            }
        }
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                supports_streaming: true,
                supports_tools: false,
                should_trim_messages: false,
            }
        }
        fn matches_model(&self, model: &str) -> bool {
            model.starts_with("test-")
        }
        fn matches_api_base(&self, api_base: &str) -> bool {
            api_base.contains("test.example.com")
        }
    }

    #[test]
    fn test_registry_default() {
        let registry = ProviderRegistry::new();
        let ids = registry.list_provider_ids();
        assert_eq!(ids, vec!["openai-codex", "openai-compat"]);
    }

    #[test]
    fn test_get_provider_for_model() {
        let mut registry = ProviderRegistry::new();
        registry.register(Arc::new(TestProvider));

        // "test-model" should match TestProvider.
        let p = registry.get_provider_for_model("test-model");
        assert_eq!(p.provider_id(), "test-provider");

        // Other models fall through to the default OpenAiCompatProvider.
        let p2 = registry.get_provider_for_model("gpt-4o");
        assert_eq!(p2.provider_id(), "openai-compat");
    }

    #[test]
    fn test_get_provider_by_id() {
        let mut registry = ProviderRegistry::new();
        registry.register(Arc::new(TestProvider));

        assert!(registry.get_provider_by_id("test-provider").is_some());
        assert!(registry.get_provider_by_id("openai-compat").is_some());
        assert!(registry.get_provider_by_id("nonexistent").is_none());
    }

    #[test]
    fn test_resolve_metadata() {
        let registry = ProviderRegistry::new();

        // Resolve for a known model -- detect_provider should identify OpenAI.
        let info = registry.resolve_metadata("gpt-4o", "https://api.openai.com/v1");
        assert_eq!(info.adapter_id, "openai-compat");
        assert_eq!(info.detected_provider_id, "openai");
        assert_eq!(
            info.dashboard_url.as_deref(),
            Some("https://platform.openai.com/usage")
        );
        assert!(info.capabilities.supports_streaming);
        assert!(info.auth_config.is_none());
    }

    #[test]
    fn test_resolve_metadata_with_custom_provider() {
        let mut registry = ProviderRegistry::new();
        registry.register(Arc::new(TestProvider));

        let info = registry.resolve_metadata("test-model", "https://api.test.example.com/v1");
        assert_eq!(info.adapter_id, "test-provider");
        // detect_provider won't recognize "test-model" so it returns "unknown".
        assert_eq!(info.detected_provider_id, "unknown");
        assert!(!info.capabilities.supports_tools);
    }

    #[test]
    fn test_list_provider_ids_after_register() {
        let mut registry = ProviderRegistry::new();
        registry.register(Arc::new(TestProvider));
        let ids = registry.list_provider_ids();
        // TestProvider should appear before the fallbacks.
        assert_eq!(ids, vec!["openai-codex", "test-provider", "openai-compat"]);
    }
}
