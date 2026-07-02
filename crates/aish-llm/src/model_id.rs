//! Model ID normalization for setup (config storage) and runtime (HTTP requests).

const COMMON_STRIP_PREFIXES: &[&str] = &["openai/", "anthropic/"];

/// Trim whitespace from a model name (used when fetching model lists).
pub fn trim_model_name(model: &str) -> String {
    model.trim().to_string()
}

/// Normalize a model name for storage in config, based on the selected provider.
///
/// Setup-layer logic: decides what goes into `config.yaml`.
pub fn normalize_model_for_provider(provider_key: &str, model: &str) -> String {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    if trimmed.contains('/') {
        if provider_key == "openai" {
            return strip_first_provider_segment(trimmed);
        }
        return trimmed.to_string();
    }

    if provider_key == "openrouter" {
        return format!("openai/{trimmed}");
    }

    trimmed.to_string()
}

/// Resolve the model name to send in HTTP requests, based on the API base URL.
///
/// Runtime-layer logic: replicates LiteLLM behavior for OpenAI-compatible proxies.
pub fn resolve_model_for_api(model: &str, api_base: &str) -> String {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let base_lower = api_base.to_lowercase();
    if base_lower.contains("api.openai.com") {
        return strip_first_provider_segment(trimmed);
    }
    if base_lower.contains("openrouter.ai") {
        return trimmed.to_string();
    }
    strip_common_provider_prefix(trimmed)
}

fn strip_first_provider_segment(model: &str) -> String {
    match model.split_once('/') {
        Some((_provider, name)) => name.to_string(),
        None => model.to_string(),
    }
}

fn strip_common_provider_prefix(model: &str) -> String {
    for prefix in COMMON_STRIP_PREFIXES {
        if let Some(stripped) = model.strip_prefix(prefix) {
            return stripped.to_string();
        }
    }
    model.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trim_model_name() {
        assert_eq!(trim_model_name("  gpt-4o  "), "gpt-4o");
        assert_eq!(trim_model_name(""), "");
    }

    #[test]
    fn test_normalize_openrouter_bare() {
        assert_eq!(
            normalize_model_for_provider("openrouter", "gpt-5.1"),
            "openai/gpt-5.1"
        );
    }

    #[test]
    fn test_normalize_openrouter_prefixed() {
        assert_eq!(
            normalize_model_for_provider("openrouter", "openai/gpt-5.1"),
            "openai/gpt-5.1"
        );
    }

    #[test]
    fn test_normalize_custom_bare() {
        assert_eq!(normalize_model_for_provider("custom", "gpt-5.1"), "gpt-5.1");
    }

    #[test]
    fn test_normalize_custom_prefixed() {
        assert_eq!(
            normalize_model_for_provider("custom", "openai/gpt-5.1"),
            "openai/gpt-5.1"
        );
    }

    #[test]
    fn test_normalize_openai_strips_prefix() {
        assert_eq!(
            normalize_model_for_provider("openai", "openai/gpt-4o"),
            "gpt-4o"
        );
    }

    #[test]
    fn test_normalize_openai_bare() {
        assert_eq!(normalize_model_for_provider("openai", "gpt-4o"), "gpt-4o");
    }

    #[test]
    fn test_resolve_openai_official() {
        assert_eq!(
            resolve_model_for_api("openai/gpt-4o", "https://api.openai.com/v1"),
            "gpt-4o"
        );
    }

    #[test]
    fn test_resolve_openrouter() {
        assert_eq!(
            resolve_model_for_api("openai/gpt-4o", "https://openrouter.ai/api/v1"),
            "openai/gpt-4o"
        );
    }

    #[test]
    fn test_resolve_custom_gateway() {
        let base = "http://www.aishell.ai:8080/aitest/v1";
        assert_eq!(resolve_model_for_api("openai/gpt-5.1", base), "gpt-5.1");
        assert_eq!(resolve_model_for_api("gpt-5.1", base), "gpt-5.1");
    }

    #[test]
    fn test_resolve_custom_preserves_unknown_prefix() {
        assert_eq!(
            resolve_model_for_api("provider/model-name", "https://gateway.example.com/v1"),
            "provider/model-name"
        );
    }
}
