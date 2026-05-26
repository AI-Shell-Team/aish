//! Dynamic model fetching from provider APIs.
//!
//! Provides functions to fetch available models from OpenAI-compatible and
//! Ollama APIs, with graceful fallback to static model lists.

use tracing::debug;

use super::get_provider_models;

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
                format!("{}...", &body[..200])
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
                    all_models.push(normalize_model_name(id));
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
                models.push(normalize_model_name(name));
            }
        }
    }

    Ok(models)
}

/// Get models for a provider, combining dynamic fetch with static fallback.
///
/// - For "ollama": calls [`fetch_ollama_models`], falls back to static list on
///   error.
/// - For known providers with an `api_key`: calls [`fetch_models_from_api`],
///   merges with the static list.
/// - For unknown providers with an `api_key`: tries [`fetch_models_from_api`],
///   returns empty on failure.
/// - Results are deduplicated while preserving order (dynamic models first).
pub fn get_models_for_provider(
    provider_key: &str,
    api_base: &str,
    api_key: Option<&str>,
) -> Vec<String> {
    let static_models = get_provider_models(provider_key);
    let is_known_provider = !static_models.is_empty() || provider_key == "ollama";

    // --- Ollama special path ---
    if provider_key == "ollama" {
        match fetch_ollama_models(DEFAULT_FETCH_TIMEOUT_S) {
            Ok(dynamic) => {
                if dynamic.is_empty() {
                    return static_models;
                }
                return merge_dedup(dynamic, static_models);
            }
            Err(e) => {
                debug!("Ollama fetch failed, using static list: {}", e);
                return static_models;
            }
        }
    }

    // --- Providers with API key ---
    if let Some(key) = api_key {
        if !key.is_empty() {
            match fetch_models_from_api(api_base, key, DEFAULT_FETCH_TIMEOUT_S) {
                Ok(dynamic) => {
                    if is_known_provider {
                        return merge_dedup(dynamic, static_models);
                    } else {
                        // Unknown provider: return whatever we got (may be empty).
                        return dynamic;
                    }
                }
                Err(e) => {
                    debug!(
                        "Dynamic fetch failed for '{}', using fallback: {}",
                        provider_key, e
                    );
                    if is_known_provider {
                        return static_models;
                    }
                    // Unknown provider with failed fetch: return empty.
                    return Vec::new();
                }
            }
        }
    }

    // No API key available — static fallback for known providers.
    if is_known_provider {
        return static_models;
    }

    Vec::new()
}

/// Normalize a model name by trimming whitespace and removing common prefixes.
pub fn normalize_model_name(model: &str) -> String {
    let trimmed = model.trim();

    // Common prefixes users might accidentally include.
    let prefixes_to_strip = [
        "openai/",    // e.g. "openai/gpt-4o" from some aggregators
        "anthropic/", // e.g. "anthropic/claude-3-opus"
    ];

    let mut result = trimmed;
    for prefix in &prefixes_to_strip {
        if let Some(stripped) = result.strip_prefix(prefix) {
            result = stripped;
            break; // Only strip one prefix
        }
    }

    result.to_string()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Merge two Vecs, deduplicating while preserving order (items from `first`
/// appear before items from `second` that are not duplicates).
fn merge_dedup(first: Vec<String>, second: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();

    for item in first.into_iter().chain(second) {
        if seen.insert(item.clone()) {
            result.push(item);
        }
    }

    result
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_model_name_basic() {
        assert_eq!(normalize_model_name("  gpt-4o  "), "gpt-4o");
        assert_eq!(normalize_model_name("gpt-4o"), "gpt-4o");
    }

    #[test]
    fn test_normalize_model_name_strip_prefix() {
        assert_eq!(normalize_model_name("openai/gpt-4o"), "gpt-4o");
        assert_eq!(
            normalize_model_name("anthropic/claude-3-opus"),
            "claude-3-opus"
        );
    }

    #[test]
    fn test_normalize_model_name_no_strip() {
        // Should NOT strip unknown prefixes.
        assert_eq!(normalize_model_name("my-custom-model"), "my-custom-model");
    }

    #[test]
    fn test_normalize_model_name_empty() {
        assert_eq!(normalize_model_name(""), "");
        assert_eq!(normalize_model_name("   "), "");
    }

    #[test]
    fn test_merge_dedup_preserves_order() {
        let a = vec!["a".to_string(), "b".to_string()];
        let b = vec!["b".to_string(), "c".to_string()];
        let result = merge_dedup(a, b);
        assert_eq!(result, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_merge_dedup_empty_first() {
        let a: Vec<String> = vec![];
        let b = vec!["x".to_string()];
        let result = merge_dedup(a, b);
        assert_eq!(result, vec!["x"]);
    }

    #[test]
    fn test_merge_dedup_all_duplicates() {
        let a = vec!["a".to_string(), "b".to_string()];
        let b = vec!["a".to_string(), "b".to_string()];
        let result = merge_dedup(a, b);
        assert_eq!(result, vec!["a", "b"]);
    }

    #[test]
    fn test_get_models_for_provider_ollama_fallback() {
        // Ollama is almost certainly not running in CI, so this tests the
        // fallback to the static list.
        let models = get_models_for_provider("ollama", "http://localhost:11434", None);
        assert!(!models.is_empty());
        // Should contain at least one entry from the static list.
        assert!(models.iter().any(|m| m.contains("llama")));
    }

    #[test]
    fn test_get_models_for_provider_unknown_no_key() {
        let models =
            get_models_for_provider("unknown-provider", "https://api.example.com/v1", None);
        assert!(models.is_empty());
    }

    #[test]
    fn test_get_models_for_provider_known_static_fallback() {
        // Use an unreachable endpoint to force a fetch failure, which should
        // fall back to the static list for known providers.
        let models = get_models_for_provider("openai", "http://127.0.0.1:1", Some("fake-key"));
        assert!(!models.is_empty());
        assert!(models.iter().any(|m| m == "gpt-4o"));
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
