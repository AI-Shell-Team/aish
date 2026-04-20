//! Model fetching from provider APIs and tool-support filtering.

use std::time::Duration;

/// Basic metadata about a model offered by a provider.
#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub id: String,
    pub owned_by: Option<String>,
}

/// Fetch available models from an OpenAI-compatible `/models` endpoint.
///
/// Tries `{api_base}/models` first, then `{api_base}/v1/models` as fallback.
/// Uses a 10-second timeout on each request.
pub async fn fetch_models_from_api(
    api_base: &str,
    api_key: Option<&str>,
) -> Result<Vec<ModelInfo>, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {}", e))?;

    let base = api_base.trim_end_matches('/');

    // Candidate URLs to try in order.
    let urls = if base.ends_with("/v1") || base.ends_with("/models") {
        vec![format!("{}/models", base)]
    } else {
        vec![format!("{}/models", base), format!("{}/v1/models", base)]
    };

    let mut last_err = String::new();

    for url in &urls {
        let mut req = client.get(url);
        if let Some(key) = api_key {
            req = req.header("Authorization", format!("Bearer {}", key));
        }

        match req.send().await {
            Ok(resp) if resp.status().is_success() => {
                let json: serde_json::Value = resp
                    .json()
                    .await
                    .map_err(|e| format!("Failed to parse response JSON: {}", e))?;
                return Ok(parse_openai_models(&json));
            }
            Ok(resp) => {
                last_err = format!("HTTP {}", resp.status());
                // Try next URL
            }
            Err(e) => {
                last_err = e.to_string();
                // Try next URL
            }
        }
    }

    Err(format!(
        "Failed to fetch models from all endpoints: {}",
        last_err
    ))
}

/// Fetch available models from an Ollama instance.
///
/// Calls `{api_base}/api/tags`, stripping any `/v1` suffix from `api_base`.
pub async fn fetch_ollama_models(api_base: &str) -> Result<Vec<ModelInfo>, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| format!("Failed to build HTTP client: {}", e))?;

    // Strip /v1 suffix if present
    let base = api_base.trim_end_matches('/').trim_end_matches("/v1");

    let url = format!("{}/api/tags", base);

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("Failed to reach Ollama at {}: {}", url, e))?;

    if !resp.status().is_success() {
        return Err(format!("Ollama returned HTTP {}", resp.status()));
    }

    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("Failed to parse Ollama response: {}", e))?;

    let models = json
        .get("models")
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|entry| {
                    let raw_name = entry.get("name")?.as_str()?.to_string();
                    // Strip the tag suffix (e.g. "llama3.2:latest" -> "llama3.2")
                    let id = raw_name.split(':').next().unwrap_or(&raw_name).to_string();
                    Some(ModelInfo {
                        id,
                        owned_by: Some("ollama".to_string()),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Ok(models)
}

/// Filter a model list to only those that likely support tool/function calling.
pub fn filter_by_tool_support(models: &[ModelInfo]) -> Vec<ModelInfo> {
    models
        .iter()
        .filter(|m| model_supports_tools(&m.id))
        .cloned()
        .collect()
}

/// Static heuristic to determine if a model supports tool/function calling.
///
/// Checks exclusion patterns first (embeddings, images, audio models),
/// then inclusion patterns for known tool-capable model families.
/// Defaults to `true` for unknown models.
pub fn model_supports_tools(model: &str) -> bool {
    let lower = model.to_lowercase();

    // Exclude known non-tool models first
    let exclusions = [
        "embed", "dall-e", "whisper", "tts", "davinci", "babbage", "curie", "ada",
    ];
    for exc in &exclusions {
        if lower.contains(exc) {
            return false;
        }
    }

    // Check known tool-supporting families
    let inclusions = [
        "gpt-4",
        "gpt-3.5-turbo",
        "o1-",
        "o3-",
        "o4-",
        "claude-",
        "gemini-1.5",
        "gemini-2",
        "deepseek-chat",
        "deepseek-v3",
        "qwen-",
        "qwq-",
        "mistral-large",
        "mistral-small",
        "mistral-medium",
        "codestral",
        "glm-4",
        "grok-",
        "kimi-",
        "minimax-",
        "minimlm",
        "ernie-",
    ];
    for inc in &inclusions {
        if lower.contains(inc) {
            return true;
        }
    }

    // Default: assume unknown models support tools
    true
}

/// Return predefined model lists for providers that lack a `/models` endpoint.
pub fn get_predefined_models(provider_id: &str) -> Vec<ModelInfo> {
    match provider_id {
        "ollama" => vec![
            ModelInfo {
                id: "llama3.2".into(),
                owned_by: Some("ollama".into()),
            },
            ModelInfo {
                id: "llama3.1".into(),
                owned_by: Some("ollama".into()),
            },
            ModelInfo {
                id: "qwen2.5".into(),
                owned_by: Some("ollama".into()),
            },
            ModelInfo {
                id: "deepseek-r1".into(),
                owned_by: Some("ollama".into()),
            },
            ModelInfo {
                id: "mistral".into(),
                owned_by: Some("ollama".into()),
            },
            ModelInfo {
                id: "codellama".into(),
                owned_by: Some("ollama".into()),
            },
            ModelInfo {
                id: "gemma2".into(),
                owned_by: Some("ollama".into()),
            },
            ModelInfo {
                id: "phi3".into(),
                owned_by: Some("ollama".into()),
            },
        ],
        "zhipu" | "zai" => vec![
            ModelInfo {
                id: "glm-4-plus".into(),
                owned_by: Some("zhipu".into()),
            },
            ModelInfo {
                id: "glm-4-0520".into(),
                owned_by: Some("zhipu".into()),
            },
            ModelInfo {
                id: "glm-4-flash".into(),
                owned_by: Some("zhipu".into()),
            },
            ModelInfo {
                id: "glm-4".into(),
                owned_by: Some("zhipu".into()),
            },
        ],
        "xai" => vec![
            ModelInfo {
                id: "grok-4".into(),
                owned_by: Some("xai".into()),
            },
            ModelInfo {
                id: "grok-3".into(),
                owned_by: Some("xai".into()),
            },
            ModelInfo {
                id: "grok-3-mini".into(),
                owned_by: Some("xai".into()),
            },
        ],
        "moonshot" => vec![
            ModelInfo {
                id: "kimi-k2.5".into(),
                owned_by: Some("moonshot".into()),
            },
            ModelInfo {
                id: "kimi-k2-turbo-preview".into(),
                owned_by: Some("moonshot".into()),
            },
            ModelInfo {
                id: "moonshot-v1-8k".into(),
                owned_by: Some("moonshot".into()),
            },
            ModelInfo {
                id: "moonshot-v1-32k".into(),
                owned_by: Some("moonshot".into()),
            },
            ModelInfo {
                id: "moonshot-v1-128k".into(),
                owned_by: Some("moonshot".into()),
            },
        ],
        "minimax" => vec![
            ModelInfo {
                id: "MiniMax-M2.5".into(),
                owned_by: Some("minimax".into()),
            },
            ModelInfo {
                id: "MiniMax-M2.5-highspeed".into(),
                owned_by: Some("minimax".into()),
            },
            ModelInfo {
                id: "MiniMax-M2.5-Lightning".into(),
                owned_by: Some("minimax".into()),
            },
        ],
        "qianfan" => vec![
            ModelInfo {
                id: "deepseek-v3.2".into(),
                owned_by: Some("baidu".into()),
            },
            ModelInfo {
                id: "ernie-4.0-8k".into(),
                owned_by: Some("baidu".into()),
            },
            ModelInfo {
                id: "ernie-4.0-turbo-8k".into(),
                owned_by: Some("baidu".into()),
            },
            ModelInfo {
                id: "ernie-3.5-8k".into(),
                owned_by: Some("baidu".into()),
            },
        ],
        "alibaba" => vec![
            ModelInfo {
                id: "qwen-max".into(),
                owned_by: Some("alibaba".into()),
            },
            ModelInfo {
                id: "qwen-plus".into(),
                owned_by: Some("alibaba".into()),
            },
            ModelInfo {
                id: "qwen-turbo".into(),
                owned_by: Some("alibaba".into()),
            },
            ModelInfo {
                id: "qwen-long".into(),
                owned_by: Some("alibaba".into()),
            },
            ModelInfo {
                id: "qwq-32b".into(),
                owned_by: Some("alibaba".into()),
            },
        ],
        "mistral" => vec![
            ModelInfo {
                id: "mistral-large-latest".into(),
                owned_by: Some("mistral".into()),
            },
            ModelInfo {
                id: "mistral-small-latest".into(),
                owned_by: Some("mistral".into()),
            },
            ModelInfo {
                id: "codestral-latest".into(),
                owned_by: Some("mistral".into()),
            },
            ModelInfo {
                id: "open-mistral-nemo".into(),
                owned_by: Some("mistral".into()),
            },
        ],
        _ => vec![],
    }
}

/// Parse an OpenAI-format model list JSON response.
fn parse_openai_models(json: &serde_json::Value) -> Vec<ModelInfo> {
    json.get("data")
        .and_then(|d| d.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|entry| {
                    let id = entry.get("id")?.as_str()?.to_string();
                    let owned_by = entry
                        .get("owned_by")
                        .and_then(|v| v.as_str())
                        .map(String::from);
                    Some(ModelInfo { id, owned_by })
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_supports_tools_positive() {
        let tool_models = [
            "gpt-4",
            "gpt-4o",
            "gpt-3.5-turbo",
            "o1-preview",
            "o3-mini",
            "o4-mini",
            "claude-3-opus",
            "gemini-1.5-pro",
            "gemini-2.0-flash",
            "deepseek-chat",
            "deepseek-v3",
            "qwen-72b",
            "qwq-32b",
            "mistral-large-latest",
            "mistral-small",
            "mistral-medium",
            "codestral-latest",
            "glm-4-plus",
        ];
        for model in &tool_models {
            assert!(
                model_supports_tools(model),
                "Expected {} to support tools",
                model
            );
        }
    }

    #[test]
    fn test_model_supports_tools_negative() {
        let non_tool_models = [
            "text-embedding-ada-002",
            "dall-e-3",
            "whisper-1",
            "tts-1",
            "davinci-002",
            "babbage-002",
            "curie",
            "ada-002",
        ];
        for model in &non_tool_models {
            assert!(
                !model_supports_tools(model),
                "Expected {} to NOT support tools",
                model
            );
        }
    }

    #[test]
    fn test_model_supports_tools_unknown_defaults_true() {
        assert!(model_supports_tools("some-future-model"));
        assert!(model_supports_tools("random-llm-v42"));
    }

    #[test]
    fn test_filter_by_tool_support() {
        let models = vec![
            ModelInfo {
                id: "gpt-4".into(),
                owned_by: None,
            },
            ModelInfo {
                id: "text-embedding-ada-002".into(),
                owned_by: None,
            },
            ModelInfo {
                id: "claude-3-opus".into(),
                owned_by: None,
            },
            ModelInfo {
                id: "dall-e-3".into(),
                owned_by: None,
            },
            ModelInfo {
                id: "unknown-model".into(),
                owned_by: None,
            },
        ];

        let filtered = filter_by_tool_support(&models);
        let ids: Vec<&str> = filtered.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["gpt-4", "claude-3-opus", "unknown-model"]);
    }

    #[test]
    fn test_parse_openai_models() {
        let json = serde_json::json!({
            "data": [
                { "id": "gpt-4", "owned_by": "openai" },
                { "id": "gpt-3.5-turbo", "owned_by": "openai" },
                { "id": "claude-3-opus", "owned_by": "anthropic" },
            ]
        });

        let models = parse_openai_models(&json);
        assert_eq!(models.len(), 3);
        assert_eq!(models[0].id, "gpt-4");
        assert_eq!(models[0].owned_by, Some("openai".to_string()));
        assert_eq!(models[2].id, "claude-3-opus");
        assert_eq!(models[2].owned_by, Some("anthropic".to_string()));
    }

    #[test]
    fn test_parse_openai_models_empty_data() {
        let json = serde_json::json!({ "data": [] });
        let models = parse_openai_models(&json);
        assert!(models.is_empty());
    }

    #[test]
    fn test_parse_openai_models_missing_data() {
        let json = serde_json::json!({ "object": "list" });
        let models = parse_openai_models(&json);
        assert!(models.is_empty());
    }

    #[test]
    fn test_get_predefined_models_ollama() {
        let models = get_predefined_models("ollama");
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert!(ids.contains(&"llama3.2"));
        assert!(ids.contains(&"qwen2.5"));
        assert!(ids.contains(&"deepseek-r1"));
        assert!(ids.contains(&"gemma2"));
        assert!(ids.contains(&"phi3"));
        assert_eq!(models.len(), 8);
        // All owned by ollama
        assert!(models
            .iter()
            .all(|m| m.owned_by.as_deref() == Some("ollama")));
    }

    #[test]
    fn test_get_predefined_models_zhipu() {
        let models = get_predefined_models("zhipu");
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert!(ids.contains(&"glm-4-plus"));
        assert!(ids.contains(&"glm-4-flash"));
        assert!(ids.contains(&"glm-4"));
        assert_eq!(models.len(), 4);
    }

    #[test]
    fn test_get_predefined_models_unknown() {
        let models = get_predefined_models("unknown-provider");
        assert!(models.is_empty());
    }

    #[test]
    fn test_get_predefined_models_xai() {
        let models = get_predefined_models("xai");
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert!(ids.contains(&"grok-4"));
        assert!(ids.contains(&"grok-3"));
        assert_eq!(models.len(), 3);
    }

    #[test]
    fn test_get_predefined_models_moonshot() {
        let models = get_predefined_models("moonshot");
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert!(ids.contains(&"kimi-k2.5"));
        assert!(ids.contains(&"moonshot-v1-8k"));
        assert_eq!(models.len(), 5);
    }

    #[test]
    fn test_get_predefined_models_minimax() {
        let models = get_predefined_models("minimax");
        assert_eq!(models.len(), 3);
        assert_eq!(models[0].id, "MiniMax-M2.5");
    }

    #[test]
    fn test_get_predefined_models_alibaba() {
        let models = get_predefined_models("alibaba");
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert!(ids.contains(&"qwen-max"));
        assert!(ids.contains(&"qwq-32b"));
    }

    #[test]
    fn test_get_predefined_models_zai_alias() {
        // "zai" should return same models as "zhipu"
        let models = get_predefined_models("zai");
        let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        assert!(ids.contains(&"glm-4-plus"));
    }
}
