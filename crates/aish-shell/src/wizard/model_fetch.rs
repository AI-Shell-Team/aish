//! Dynamic model fetching from provider APIs.
//!
//! Known providers with a local catalog skip the network during setup.
//! Ollama, vLLM, and providers without a local catalog discover models live.

use tracing::debug;

use aish_llm::trim_model_name;

use super::get_provider_models;

/// How the setup model list was obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelCatalogKind {
    /// Built-in catalog; no network request was made.
    Local,
    /// Result of a live discovery request (success or failure).
    Discovered,
}

/// Models shown in setup, plus whether they came from the local catalog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCatalog {
    pub models: Vec<String>,
    pub kind: ModelCatalogKind,
    pub error: Option<String>,
}

impl ModelCatalog {
    fn local(models: Vec<String>) -> Self {
        Self {
            models,
            kind: ModelCatalogKind::Local,
            error: None,
        }
    }

    fn discovered(models: Vec<String>) -> Self {
        Self {
            models,
            kind: ModelCatalogKind::Discovered,
            error: None,
        }
    }

    fn discover_failed(error: String) -> Self {
        Self {
            models: Vec::new(),
            kind: ModelCatalogKind::Discovered,
            error: Some(error),
        }
    }
}

/// Providers with no reliable local catalog (Ollama, vLLM, custom/unknown, or
/// hosted providers that ship an empty built-in list) must be discovered online.
pub fn uses_live_discovery(provider_key: &str) -> bool {
    matches!(provider_key, "ollama" | "vllm") || get_provider_models(provider_key).is_empty()
}

// ---------------------------------------------------------------------------
// Default timeout
// ---------------------------------------------------------------------------

/// Default timeout for model-fetch requests (seconds).
const DEFAULT_FETCH_TIMEOUT_S: u64 = 10;

/// Maximum number of models to collect (prevents unbounded pagination).
const MAX_MODELS: usize = 200;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Fetch models from an OpenAI-compatible `/models` endpoint.
///
/// Sends a GET request to `{api_base}/models` with a Bearer token and parses
/// the `data[].id` fields from the JSON response.  Handles pagination via
/// `has_more` / `last_id` query parameters (OpenAI style).
pub fn fetch_models_from_api(
    api_base: &str,
    api_key: &str,
    timeout_s: u64,
) -> Result<Vec<String>, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_s))
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {}", e))?;

    let base_url = format!("{}/models", api_base.trim_end_matches('/'));
    let mut all_models: Vec<String> = Vec::new();
    let mut last_id: Option<String> = None;

    loop {
        let url = match &last_id {
            Some(id) => format!("{}?after={}&limit=100", base_url, id),
            None => format!("{}?limit=100", base_url),
        };

        debug!("Fetching models from: {}", url);

        let response = client
            .get(&url)
            .header("Authorization", format!("Bearer {}", api_key))
            .send()
            .map_err(|e| {
                if e.is_timeout() {
                    format!("Request timed out after {}s", timeout_s)
                } else if e.is_connect() {
                    "Connection refused or unreachable".to_string()
                } else {
                    format!("Request failed: {}", e)
                }
            })?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().unwrap_or_default();
            let detail = if body.len() > 200 {
                let mut end = 200;
                while end > 0 && !body.is_char_boundary(end) {
                    end -= 1;
                }
                format!("{}...", &body[..end])
            } else {
                body
            };
            return Err(format!("HTTP {}: {}", status.as_u16(), detail));
        }

        let body: serde_json::Value = response
            .json()
            .map_err(|e| format!("Failed to parse JSON response: {}", e))?;

        // Extract model IDs from data[].id
        if let Some(data) = body.get("data").and_then(|d| d.as_array()) {
            for entry in data {
                if let Some(id) = entry.get("id").and_then(|v| v.as_str()) {
                    all_models.push(trim_model_name(id));
                }
            }
        } else {
            // No data array — nothing to iterate.
            break;
        }

        // Check pagination: some providers use "has_more" + "last_id".
        let has_more = body
            .get("has_more")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if !has_more || all_models.len() >= MAX_MODELS {
            break;
        }

        // Advance pagination cursor: use the last model id seen.
        if let Some(last) = all_models.last() {
            last_id = Some(last.clone());
        } else {
            break;
        }
    }

    Ok(all_models)
}

/// Fetch models from a local Ollama instance.
///
/// Sends a GET request to `http://localhost:11434/api/tags` and parses the
/// `models[].name` fields.  No authentication is needed.
pub fn fetch_ollama_models(timeout_s: u64) -> Result<Vec<String>, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_s))
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {}", e))?;

    let url = "http://localhost:11434/api/tags";
    debug!("Fetching Ollama models from: {}", url);

    let response = client.get(url).send().map_err(|e| {
        if e.is_timeout() {
            format!("Request timed out after {}s", timeout_s)
        } else if e.is_connect() {
            "Ollama is not running (connection refused)".to_string()
        } else {
            format!("Request failed: {}", e)
        }
    })?;

    let status = response.status();
    if !status.is_success() {
        return Err(format!("HTTP {} from Ollama", status.as_u16()));
    }

    let body: serde_json::Value = response
        .json()
        .map_err(|e| format!("Failed to parse Ollama response: {}", e))?;

    let mut models = Vec::new();
    if let Some(arr) = body.get("models").and_then(|v| v.as_array()) {
        for entry in arr {
            if let Some(name) = entry.get("name").and_then(|v| v.as_str()) {
                models.push(trim_model_name(name));
            }
        }
    }

    Ok(models)
}

/// Resolve the setup model list for a provider.
///
/// Known providers return the local catalog immediately. Ollama, vLLM, and
/// providers without a local catalog discover models from the endpoint and
/// never substitute the built-in list on failure.
pub fn get_models_for_provider(
    provider_key: &str,
    api_base: &str,
    api_key: Option<&str>,
) -> ModelCatalog {
    if !uses_live_discovery(provider_key) {
        return ModelCatalog::local(get_provider_models(provider_key));
    }

    if provider_key == "ollama" {
        return match fetch_ollama_models(DEFAULT_FETCH_TIMEOUT_S) {
            Ok(models) => ModelCatalog::discovered(models),
            Err(e) => {
                debug!("Ollama discovery failed: {}", e);
                ModelCatalog::discover_failed(e)
            }
        };
    }

    match fetch_models_from_api(api_base, api_key.unwrap_or(""), DEFAULT_FETCH_TIMEOUT_S) {
        Ok(models) => ModelCatalog::discovered(models),
        Err(e) => {
            debug!("Live discovery failed for '{}': {}", provider_key, e);
            ModelCatalog::discover_failed(e)
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trim_model_name_in_fetch() {
        assert_eq!(trim_model_name("  gpt-4o  "), "gpt-4o");
        assert_eq!(trim_model_name("openai/gpt-4o"), "openai/gpt-4o");
        assert_eq!(
            trim_model_name("anthropic/claude-3-opus"),
            "anthropic/claude-3-opus"
        );
    }

    #[test]
    fn test_trim_model_name_empty() {
        assert_eq!(trim_model_name(""), "");
        assert_eq!(trim_model_name("   "), "");
    }

    #[test]
    fn known_providers_skip_live_discovery() {
        assert!(!uses_live_discovery("openai"));
        assert!(!uses_live_discovery("anthropic"));
        assert!(!uses_live_discovery("openai-codex"));
    }

    #[test]
    fn discovery_only_providers_use_live_discovery() {
        assert!(uses_live_discovery("ollama"));
        assert!(uses_live_discovery("vllm"));
        assert!(uses_live_discovery("custom"));
        assert!(uses_live_discovery("unknown-provider"));
        assert!(uses_live_discovery("openrouter"));
    }

    #[test]
    fn known_provider_returns_local_catalog_without_using_endpoint() {
        let catalog = get_models_for_provider("openai", "http://127.0.0.1:1", Some("fake-key"));
        assert_eq!(catalog.kind, ModelCatalogKind::Local);
        assert!(catalog.error.is_none());
        assert!(catalog.models.iter().any(|m| m == "gpt-5.6-sol"));
    }

    #[test]
    fn ollama_discovery_failure_does_not_return_builtin_list() {
        let catalog = get_models_for_provider("ollama", "http://localhost:11434", None);
        assert_eq!(catalog.kind, ModelCatalogKind::Discovered);
        if catalog.error.is_some() {
            assert!(catalog.models.is_empty());
        }
    }

    #[test]
    fn vllm_discovery_failure_does_not_return_builtin_list() {
        let catalog = get_models_for_provider("vllm", "http://127.0.0.1:1", None);
        assert_eq!(catalog.kind, ModelCatalogKind::Discovered);
        assert!(catalog.error.is_some());
        assert!(catalog.models.is_empty());
    }

    #[test]
    fn unknown_provider_discovery_failure_is_empty_not_local() {
        let catalog =
            get_models_for_provider("unknown-provider", "http://127.0.0.1:1", Some("fake-key"));
        assert_eq!(catalog.kind, ModelCatalogKind::Discovered);
        assert!(catalog.error.is_some());
        assert!(catalog.models.is_empty());
    }

    #[test]
    fn test_fetch_models_from_api_bad_url() {
        let result = fetch_models_from_api("http://127.0.0.1:1", "fake-key", 2);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("Connection refused")
                || err.contains("unreachable")
                || err.contains("connect")
                || err.contains("error"),
            "Unexpected error: {}",
            err
        );
    }

    #[test]
    fn test_fetch_ollama_models_not_running() {
        // Ollama is unlikely to be running during tests.
        let result = fetch_ollama_models(2);
        // May succeed (if Ollama happens to be running) or fail.
        // We just ensure it doesn't panic.
        let _ = result;
    }

    #[test]
    fn test_default_timeout_value() {
        assert_eq!(DEFAULT_FETCH_TIMEOUT_S, 10);
    }

    #[test]
    fn test_max_models_value() {
        assert_eq!(MAX_MODELS, 200);
    }
}
