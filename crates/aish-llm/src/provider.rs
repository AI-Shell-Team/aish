//! Simple provider detection from model names and API base URLs.

/// Metadata about an LLM provider.
#[derive(Debug, Clone)]
pub struct ProviderInfo {
    /// Provider identifier (e.g. "openai", "anthropic", "ollama").
    pub id: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Dashboard/web UI URL (if available).
    pub dashboard_url: Option<String>,
    /// Whether this provider supports streaming.
    pub supports_streaming: bool,
    /// Whether this provider supports tool/function calling.
    pub supports_tools: bool,
}

/// Detect the provider from a model name.
pub fn detect_provider_from_model(model: &str) -> ProviderInfo {
    let model_lower = model.to_lowercase();

    let (id, display) = if model_lower.starts_with("claude") || model_lower.starts_with("anthropic")
    {
        ("anthropic", "Anthropic")
    } else if model_lower.starts_with("gpt")
        || model_lower.starts_with("o1")
        || model_lower.starts_with("o3")
        || model_lower.starts_with("o4")
        || model_lower.starts_with("chatgpt")
    {
        ("openai", "OpenAI")
    } else if model_lower.starts_with("gemini") || model_lower.starts_with("gemma") {
        ("google", "Google AI")
    } else if model_lower.starts_with("deepseek") {
        ("deepseek", "DeepSeek")
    } else if model_lower.starts_with("qwen") || model_lower.starts_with("qwq") {
        ("alibaba", "Alibaba Cloud")
    } else if model_lower.starts_with("llama")
        || model_lower.starts_with("mistral")
        || model_lower.starts_with("mixtral")
        || model_lower.starts_with("codestral")
    {
        ("mistral", "Mistral AI")
    } else if model_lower.starts_with("glm") {
        ("zhipu", "Zhipu AI")
    } else if model_lower.starts_with("grok") {
        ("xai", "xAI (Grok)")
    } else if model_lower.starts_with("kimi")
        || model_lower.starts_with("k2p")
        || model_lower.starts_with("moonshot")
    {
        ("moonshot", "Moonshot AI")
    } else if model_lower.starts_with("minimax") {
        ("minimax", "MiniMax")
    } else if model_lower.contains("ernie") {
        ("qianfan", "Baidu Qianfan")
    } else {
        ("unknown", "Unknown")
    };

    ProviderInfo {
        id: id.to_string(),
        display_name: display.to_string(),
        dashboard_url: dashboard_url_for(id),
        supports_streaming: true,
        supports_tools: supports_tools_for(id),
    }
}

/// Refine provider detection using the API base URL.
pub fn refine_provider_from_api_base(provider: &mut ProviderInfo, api_base: &str) {
    let base_lower = api_base.to_lowercase();

    if base_lower.contains("anthropic.com") {
        provider.id = "anthropic".into();
        provider.display_name = "Anthropic".into();
    } else if base_lower.contains("openai.com") && !base_lower.contains("codex") {
        provider.id = "openai".into();
        provider.display_name = "OpenAI".into();
    } else if base_lower.contains("codex.ai") || base_lower.contains("codex.openai") {
        provider.id = "openai-codex".into();
        provider.display_name = "OpenAI Codex".into();
    } else if base_lower.contains("generativelanguage.googleapis") {
        provider.id = "google".into();
        provider.display_name = "Google AI".into();
    } else if base_lower.contains("deepseek.com") {
        provider.id = "deepseek".into();
        provider.display_name = "DeepSeek".into();
    } else if base_lower.contains("dashscope") || base_lower.contains("aliyuncs") {
        provider.id = "alibaba".into();
        provider.display_name = "Alibaba Cloud".into();
    } else if base_lower.contains("mistral.ai") {
        provider.id = "mistral".into();
        provider.display_name = "Mistral AI".into();
    } else if base_lower.contains("localhost")
        || base_lower.contains("127.0.0.1")
        || base_lower.contains("ollama")
    {
        // Distinguish vLLM (:8000) from Ollama (:11434)
        if base_lower.contains(":8000") {
            provider.id = "vllm".into();
            provider.display_name = "vLLM (Local)".into();
        } else {
            provider.id = "ollama".into();
            provider.display_name = "Ollama (Local)".into();
            provider.supports_tools = true;
        }
    } else if base_lower.contains("zhipu") || base_lower.contains("bigmodel") {
        provider.id = "zhipu".into();
        provider.display_name = "Zhipu AI".into();
    } else if base_lower.contains("x.ai") {
        provider.id = "xai".into();
        provider.display_name = "xAI (Grok)".into();
    } else if base_lower.contains("moonshot") {
        provider.id = "moonshot".into();
        provider.display_name = "Moonshot AI".into();
    } else if base_lower.contains("minimax") {
        provider.id = "minimax".into();
        provider.display_name = "MiniMax".into();
    } else if base_lower.contains("openrouter.ai") {
        provider.id = "openrouter".into();
        provider.display_name = "OpenRouter".into();
    } else if base_lower.contains("together.xyz") {
        provider.id = "together".into();
        provider.display_name = "Together AI".into();
    } else if base_lower.contains("huggingface") {
        provider.id = "huggingface".into();
        provider.display_name = "HuggingFace".into();
    } else if base_lower.contains("baidubce") || base_lower.contains("qianfan") {
        provider.id = "qianfan".into();
        provider.display_name = "Baidu Qianfan".into();
    } else if base_lower.contains("kilocode") {
        provider.id = "kilocode".into();
        provider.display_name = "Kilo Gateway".into();
    } else if base_lower.contains("vercel.ai") || base_lower.contains("gateway.vercel") {
        provider.id = "ai_gateway".into();
        provider.display_name = "Vercel AI Gateway".into();
    } else if base_lower.contains("z.ai") {
        provider.id = "zai".into();
        provider.display_name = "Z.AI".into();
    }

    if provider.dashboard_url.is_none() {
        provider.dashboard_url = dashboard_url_for(&provider.id);
    }
}

/// Detect provider from both model name and API base URL.
pub fn detect_provider(model: &str, api_base: &str) -> ProviderInfo {
    let mut provider = detect_provider_from_model(model);
    if !api_base.is_empty() {
        refine_provider_from_api_base(&mut provider, api_base);
    }
    provider
}

fn dashboard_url_for(provider_id: &str) -> Option<String> {
    match provider_id {
        "openai" => Some("https://platform.openai.com/usage".into()),
        "anthropic" => Some("https://console.anthropic.com/".into()),
        "google" => Some("https://aistudio.google.com/".into()),
        "deepseek" => Some("https://platform.deepseek.com/usage".into()),
        "mistral" => Some("https://console.mistral.ai/".into()),
        "openai-codex" => Some("https://codex.ai/".into()),
        "xai" => Some("https://console.x.ai/".into()),
        "moonshot" => Some("https://platform.moonshot.ai/console/api-keys".into()),
        "minimax" => {
            Some("https://platform.minimaxi.com/user-center/basic-information/interface-key".into())
        }
        "alibaba" => Some("https://dashscope.console.aliyun.com/overview".into()),
        "zhipu" | "zai" => Some("https://platform.z.ai/usage".into()),
        "openrouter" => Some("https://openrouter.ai/settings/credits".into()),
        "together" => Some("https://api.together.xyz/settings/api-keys".into()),
        "huggingface" => Some("https://huggingface.co/settings/tokens".into()),
        "qianfan" => Some("https://console.bce.baidu.com/qianfan/".into()),
        "ollama" => None,
        "vllm" => None,
        _ => None,
    }
}

fn supports_tools_for(provider_id: &str) -> bool {
    match provider_id {
        "openai" | "anthropic" | "google" | "deepseek" | "mistral" | "ollama" | "xai"
        | "moonshot" | "minimax" | "openrouter" | "together" | "zhipu" | "zai" | "alibaba"
        | "qianfan" | "vllm" => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_openai() {
        let p = detect_provider_from_model("gpt-4o");
        assert_eq!(p.id, "openai");
        assert_eq!(p.display_name, "OpenAI");
        assert_eq!(
            p.dashboard_url.as_deref(),
            Some("https://platform.openai.com/usage")
        );
        assert!(p.supports_streaming);
        assert!(p.supports_tools);

        let p2 = detect_provider_from_model("o1-preview");
        assert_eq!(p2.id, "openai");

        let p3 = detect_provider_from_model("o3-mini");
        assert_eq!(p3.id, "openai");

        let p4 = detect_provider_from_model("o4-mini");
        assert_eq!(p4.id, "openai");

        let p5 = detect_provider_from_model("chatgpt-4o-latest");
        assert_eq!(p5.id, "openai");
    }

    #[test]
    fn test_detect_anthropic() {
        let p = detect_provider_from_model("claude-sonnet-4-20250514");
        assert_eq!(p.id, "anthropic");
        assert_eq!(p.display_name, "Anthropic");
        assert_eq!(
            p.dashboard_url.as_deref(),
            Some("https://console.anthropic.com/")
        );
        assert!(p.supports_streaming);
        assert!(p.supports_tools);

        let p2 = detect_provider_from_model("anthropic-model");
        assert_eq!(p2.id, "anthropic");
    }

    #[test]
    fn test_detect_deepseek() {
        let p = detect_provider_from_model("deepseek-chat");
        assert_eq!(p.id, "deepseek");
        assert_eq!(p.display_name, "DeepSeek");
        assert_eq!(
            p.dashboard_url.as_deref(),
            Some("https://platform.deepseek.com/usage")
        );
        assert!(p.supports_tools);
    }

    #[test]
    fn test_detect_from_api_base_ollama() {
        let mut p = ProviderInfo {
            id: "unknown".into(),
            display_name: "Unknown".into(),
            dashboard_url: None,
            supports_streaming: true,
            supports_tools: false,
        };
        refine_provider_from_api_base(&mut p, "http://localhost:11434/v1");
        assert_eq!(p.id, "ollama");
        assert_eq!(p.display_name, "Ollama (Local)");
        assert!(p.supports_tools);

        let mut p2 = ProviderInfo {
            id: "unknown".into(),
            display_name: "Unknown".into(),
            dashboard_url: None,
            supports_streaming: true,
            supports_tools: false,
        };
        refine_provider_from_api_base(&mut p2, "http://127.0.0.1:11434/v1");
        assert_eq!(p2.id, "ollama");

        let mut p3 = ProviderInfo {
            id: "unknown".into(),
            display_name: "Unknown".into(),
            dashboard_url: None,
            supports_streaming: true,
            supports_tools: false,
        };
        refine_provider_from_api_base(&mut p3, "http://ollama.local:11434/v1");
        assert_eq!(p3.id, "ollama");
    }

    #[test]
    fn test_detect_combined() {
        // Model says OpenAI, API base says Ollama — API base wins
        let p = detect_provider("gpt-4", "http://localhost:11434/v1");
        assert_eq!(p.id, "ollama");
        assert_eq!(p.display_name, "Ollama (Local)");

        // Model says Anthropic, empty API base — model wins
        let p2 = detect_provider("claude-3-opus", "");
        assert_eq!(p2.id, "anthropic");
    }

    #[test]
    fn test_detect_unknown() {
        let p = detect_provider_from_model("some-random-model");
        assert_eq!(p.id, "unknown");
        assert_eq!(p.display_name, "Unknown");
        assert!(p.dashboard_url.is_none());
        assert!(!p.supports_tools);
    }

    #[test]
    fn test_refine_overrides_unknown() {
        let p = detect_provider("mymodel", "https://api.openai.com/v1");
        assert_eq!(p.id, "openai");
        assert_eq!(p.display_name, "OpenAI");
        assert_eq!(
            p.dashboard_url.as_deref(),
            Some("https://platform.openai.com/usage")
        );
    }

    #[test]
    fn test_zhipu_detection() {
        let p = detect_provider_from_model("glm-4-flash");
        assert_eq!(p.id, "zhipu");
        assert_eq!(p.display_name, "Zhipu AI");
        assert!(p.supports_tools);

        let p2 = detect_provider("some-model", "https://open.bigmodel.cn/api/paas/v4");
        assert_eq!(p2.id, "zhipu");
    }

    #[test]
    fn test_qwen_detection() {
        let p = detect_provider_from_model("qwen-max");
        assert_eq!(p.id, "alibaba");
        assert_eq!(p.display_name, "Alibaba Cloud");

        let p2 = detect_provider_from_model("qwq-32b");
        assert_eq!(p2.id, "alibaba");

        let p3 = detect_provider(
            "qwen-turbo",
            "https://dashscope.aliyuncs.com/compatible-mode/v1",
        );
        assert_eq!(p3.id, "alibaba");
    }

    #[test]
    fn test_xai_detection() {
        let p = detect_provider_from_model("grok-4");
        assert_eq!(p.id, "xai");
        assert_eq!(p.display_name, "xAI (Grok)");
        assert!(p.supports_tools);

        let p2 = detect_provider("grok-3", "https://api.x.ai/v1");
        assert_eq!(p2.id, "xai");
    }

    #[test]
    fn test_moonshot_detection() {
        let p = detect_provider_from_model("kimi-k2.5");
        assert_eq!(p.id, "moonshot");
        assert_eq!(p.display_name, "Moonshot AI");

        let p2 = detect_provider_from_model("moonshot-v1");
        assert_eq!(p2.id, "moonshot");

        let p3 = detect_provider("model", "https://api.moonshot.ai/v1");
        assert_eq!(p3.id, "moonshot");

        let p4 = detect_provider_from_model("k2p5");
        assert_eq!(p4.id, "moonshot");
    }

    #[test]
    fn test_minimax_detection() {
        let p = detect_provider_from_model("MiniMax-M2.5");
        assert_eq!(p.id, "minimax");
        assert_eq!(p.display_name, "MiniMax");

        let p2 = detect_provider("model", "https://api.minimaxi.com/anthropic");
        assert_eq!(p2.id, "minimax");
    }

    #[test]
    fn test_openrouter_detection() {
        let p = detect_provider("gpt-4o", "https://openrouter.ai/api/v1");
        assert_eq!(p.id, "openrouter");
        assert_eq!(p.display_name, "OpenRouter");
        assert!(p.supports_tools);
    }

    #[test]
    fn test_together_detection() {
        let p = detect_provider("meta-llama/model", "https://api.together.xyz/v1");
        assert_eq!(p.id, "together");
        assert_eq!(p.display_name, "Together AI");
    }

    #[test]
    fn test_zai_detection() {
        let p = detect_provider("glm-4", "https://api.z.ai/api/paas/v4");
        assert_eq!(p.id, "zai");
        assert_eq!(p.display_name, "Z.AI");
    }

    #[test]
    fn test_vllm_detection() {
        let p = detect_provider("model", "http://localhost:8000/v1");
        assert_eq!(p.id, "vllm");
        assert_eq!(p.display_name, "vLLM (Local)");
    }

    #[test]
    fn test_kilocode_detection() {
        let p = detect_provider("model", "https://api.kilocode.ai/v1");
        assert_eq!(p.id, "kilocode");
        assert_eq!(p.display_name, "Kilo Gateway");
    }

    #[test]
    fn test_qianfan_detection() {
        let p = detect_provider_from_model("ernie-4.0-8k");
        assert_eq!(p.id, "qianfan");
        assert_eq!(p.display_name, "Baidu Qianfan");

        let p2 = detect_provider("model", "https://qianfan.baidubce.com/v2");
        assert_eq!(p2.id, "qianfan");
    }
}
