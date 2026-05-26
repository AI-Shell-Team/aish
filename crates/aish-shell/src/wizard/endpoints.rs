//! Multi-region endpoint selection for LLM providers.

/// A provider endpoint with a label, API base URL, hint, and default model.
#[derive(Debug, Clone)]
pub struct EndpointInfo {
    pub label: String,
    pub api_base: String,
    pub region: String,
    pub hint: String,
    pub default_model: String,
}

/// Get alternative endpoints for a provider.
pub fn get_provider_endpoints(provider_key: &str) -> Vec<EndpointInfo> {
    match provider_key {
        "zai" => vec![
            EndpointInfo {
                label: "Z.AI Global".to_string(),
                api_base: "https://api.z.ai/api/paas/v4".to_string(),
                region: "zai-global".to_string(),
                hint: "api.z.ai (GLM-5 recommended)".to_string(),
                default_model: "glm-5".to_string(),
            },
            EndpointInfo {
                label: "Z.AI CN".to_string(),
                api_base: "https://open.bigmodel.cn/api/paas/v4".to_string(),
                region: "zai-cn".to_string(),
                hint: "open.bigmodel.cn (GLM-5 recommended)".to_string(),
                default_model: "glm-5".to_string(),
            },
            EndpointInfo {
                label: "Z.AI Coding Global".to_string(),
                api_base: "https://api.z.ai/api/coding/paas/v4".to_string(),
                region: "zai-coding-global".to_string(),
                hint: "Coding Plan endpoint (GLM-4.7)".to_string(),
                default_model: "glm-4.7".to_string(),
            },
            EndpointInfo {
                label: "Z.AI Coding CN".to_string(),
                api_base: "https://open.bigmodel.cn/api/coding/paas/v4".to_string(),
                region: "zai-coding-cn".to_string(),
                hint: "Coding Plan CN endpoint (GLM-4.7)".to_string(),
                default_model: "glm-4.7".to_string(),
            },
        ],
        "minimax" => vec![
            EndpointInfo {
                label: "MiniMax Global".to_string(),
                api_base: "https://api.minimax.io/anthropic".to_string(),
                region: "minimax-global".to_string(),
                hint: "api.minimax.io (M2.5 recommended)".to_string(),
                default_model: "MiniMax-M2.5".to_string(),
            },
            EndpointInfo {
                label: "MiniMax CN".to_string(),
                api_base: "https://api.minimaxi.com/anthropic".to_string(),
                region: "minimax-cn".to_string(),
                hint: "api.minimaxi.com (M2.5 recommended)".to_string(),
                default_model: "MiniMax-M2.5".to_string(),
            },
        ],
        "moonshot" => vec![
            EndpointInfo {
                label: "Moonshot International".to_string(),
                api_base: "https://api.moonshot.ai/v1".to_string(),
                region: "moonshot-international".to_string(),
                hint: "api.moonshot.ai (Kimi K2.5)".to_string(),
                default_model: "kimi-k2.5".to_string(),
            },
            EndpointInfo {
                label: "Moonshot CN".to_string(),
                api_base: "https://api.moonshot.cn/v1".to_string(),
                region: "moonshot-cn".to_string(),
                hint: "api.moonshot.cn (Kimi K2.5)".to_string(),
                default_model: "kimi-k2.5".to_string(),
            },
        ],
        "deepseek" => vec![
            EndpointInfo {
                label: "DeepSeek (Global)".to_string(),
                api_base: "https://api.deepseek.com/v1".to_string(),
                region: "global".to_string(),
                hint: "api.deepseek.com".to_string(),
                default_model: "deepseek-chat".to_string(),
            },
            EndpointInfo {
                label: "DeepSeek (China)".to_string(),
                api_base: "https://api.deepseek.com/v1".to_string(),
                region: "cn".to_string(),
                hint: "api.deepseek.com".to_string(),
                default_model: "deepseek-chat".to_string(),
            },
        ],
        "openrouter" => vec![EndpointInfo {
            label: "OpenRouter (Global)".to_string(),
            api_base: "https://openrouter.ai/api/v1".to_string(),
            region: "global".to_string(),
            hint: "openrouter.ai".to_string(),
            default_model: String::new(),
        }],
        _ => vec![],
    }
}

/// Check if a provider has multiple endpoint choices.
pub fn has_multi_endpoints(provider_key: &str) -> bool {
    matches!(provider_key, "zai" | "minimax" | "moonshot" | "deepseek")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deepseek_endpoints() {
        let endpoints = get_provider_endpoints("deepseek");
        assert!(!endpoints.is_empty());
        assert!(endpoints.iter().any(|e| e.region == "global"));
    }

    #[test]
    fn test_zai_endpoints() {
        let endpoints = get_provider_endpoints("zai");
        assert_eq!(endpoints.len(), 4);
        assert!(endpoints.iter().any(|e| e.region == "zai-global"));
        assert!(endpoints.iter().any(|e| e.region == "zai-coding-cn"));
    }

    #[test]
    fn test_minimax_endpoints() {
        let endpoints = get_provider_endpoints("minimax");
        assert_eq!(endpoints.len(), 2);
        assert!(endpoints.iter().any(|e| e.region == "minimax-global"));
    }

    #[test]
    fn test_moonshot_endpoints() {
        let endpoints = get_provider_endpoints("moonshot");
        assert_eq!(endpoints.len(), 2);
        assert!(endpoints.iter().any(|e| e.default_model == "kimi-k2.5"));
    }

    #[test]
    fn test_no_endpoints_for_unknown() {
        let endpoints = get_provider_endpoints("nonexistent");
        assert!(endpoints.is_empty());
    }

    #[test]
    fn test_has_multi_endpoints() {
        assert!(has_multi_endpoints("zai"));
        assert!(has_multi_endpoints("minimax"));
        assert!(has_multi_endpoints("moonshot"));
        assert!(has_multi_endpoints("deepseek"));
        assert!(!has_multi_endpoints("openrouter"));
        assert!(!has_multi_endpoints("openai"));
    }
}
