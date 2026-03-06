"""Constants used by setup wizard."""

from __future__ import annotations

_PROVIDER_ENV_KEYS: dict[str, str] = {
    "openai": "OPENAI_API_KEY",
    "anthropic": "ANTHROPIC_API_KEY",
    "deepseek": "DEEPSEEK_API_KEY",
    "gemini": "GOOGLE_API_KEY",
    "google": "GOOGLE_API_KEY",
    "minimax": "MINIMAX_API_KEY",
    "moonshot": "MOONSHOT_API_KEY",
    "zai": "ZAI_API_KEY",
    "openrouter": "OPENAI_API_KEY",
    "qianfan": "QIANFAN_API_KEY",
    "mistral": "MISTRAL_API_KEY",
    "together": "TOGETHER_API_KEY",
    "huggingface": "HUGGINGFACE_API_KEY",
    "qwen": "DASHSCOPE_API_KEY",
    "xai": "XAI_API_KEY",
    "kilocode": "KILOCODE_API_KEY",
    "ai_gateway": "AI_GATEWAY_API_KEY",
}

_PROVIDER_LABELS: dict[str, str] = {
    "openai": "OpenAI",
    "anthropic": "Anthropic",
    "deepseek": "DeepSeek",
    "gemini": "Gemini",
    "google": "Google",
    "minimax": "MiniMax",
    "moonshot": "Moonshot AI",
    "zai": "Z.AI",
    "openrouter": "OpenRouter",
    "azure": "Azure",
    "qianfan": "Baidu Qianfan",
    "ollama": "Ollama",
    "vllm": "vLLM",
    "mistral": "Mistral AI",
    "together": "Together AI",
    "huggingface": "HuggingFace",
    "qwen": "Qwen (Alibaba)",
    "xai": "xAI (Grok)",
    "kilocode": "Kilo Gateway",
    "ai_gateway": "Vercel AI Gateway",
}

_PROVIDER_BASES: dict[str, str] = {
    "openrouter": "https://openrouter.ai/api/v1",
    "qianfan": "https://qianfan.baidubce.com/v2",
    "ollama": "http://127.0.0.1:11434/v1",
    "vllm": "http://127.0.0.1:8000/v1",
    "mistral": "https://api.mistral.ai/v1",
    "together": "https://api.together.xyz/v1",
    "huggingface": "https://api-inference.huggingface.co/v1",
    "qwen": "https://dashscope.aliyuncs.com/compatible-mode/v1",
    "xai": "https://api.x.ai/v1",
    "kilocode": "https://api.kilocode.ai/v1",
    "ai_gateway": "https://gateway.vercel.ai/api/v1",
}

_PROVIDER_ALIASES: dict[str, str] = {
    "open_router": "openrouter",
    "openrouter.ai": "openrouter",
}

_PROVIDER_PRIORITY: tuple[str, ...] = (
    "openrouter",
    "openai",
    "anthropic",
    "deepseek",
    "gemini",
    "google",
    "xai",
    "minimax",
    "moonshot",
    "zai",
    "qianfan",
    "mistral",
    "together",
    "huggingface",
    "qwen",
    "kilocode",
    "ollama",
    "vllm",
    "ai_gateway",
    "azure",
    "bedrock",
)

# Z.AI endpoint configurations for second-level selection
_ZAI_ENDPOINTS: dict[str, dict[str, str]] = {
    "zai-global": {
        "label": "Z.AI Global",
        "api_base": "https://api.z.ai/api/paas/v4",
        "hint": "api.z.ai (GLM-5 recommended)",
    },
    "zai-cn": {
        "label": "Z.AI CN",
        "api_base": "https://open.bigmodel.cn/api/paas/v4",
        "hint": "open.bigmodel.cn (GLM-5 recommended)",
    },
    "zai-coding-global": {
        "label": "Z.AI Coding Global",
        "api_base": "https://api.z.ai/api/coding/paas/v4",
        "hint": "Coding Plan endpoint (GLM-4.7)",
    },
    "zai-coding-cn": {
        "label": "Z.AI Coding CN",
        "api_base": "https://open.bigmodel.cn/api/coding/paas/v4",
        "hint": "Coding Plan CN endpoint (GLM-4.7)",
    },
}

# Z.AI model catalog
_ZAI_MODELS: list[str] = [
    "glm-5",
    "glm-4.7",
    "glm-4.7-flash",
    "glm-4.7-flashx",
]

# Z.AI default models per endpoint
_ZAI_DEFAULT_MODELS: dict[str, str] = {
    "zai-global": "glm-5",
    "zai-cn": "glm-5",
    "zai-coding-global": "glm-4.7",
    "zai-coding-cn": "glm-4.7",
}

# MiniMax endpoint configurations for second-level selection
_MINIMAX_ENDPOINTS: dict[str, dict[str, str]] = {
    "minimax-global": {
        "label": "MiniMax Global",
        "api_base": "https://api.minimax.io/anthropic",
        "hint": "api.minimax.io (M2.5 recommended)",
    },
    "minimax-cn": {
        "label": "MiniMax CN",
        "api_base": "https://api.minimaxi.com/anthropic",
        "hint": "api.minimaxi.com (M2.5 recommended)",
    },
}

# MiniMax model catalog
_MINIMAX_MODELS: list[str] = [
    "MiniMax-M2.5",
    "MiniMax-M2.5-highspeed",
    "MiniMax-M2.5-Lightning",
]

# MiniMax default models per endpoint
_MINIMAX_DEFAULT_MODELS: dict[str, str] = {
    "minimax-global": "MiniMax-M2.5",
    "minimax-cn": "MiniMax-M2.5",
}

# Moonshot endpoint configurations for second-level selection
_MOONSHOT_ENDPOINTS: dict[str, dict[str, str]] = {
    "moonshot-international": {
        "label": "Moonshot International",
        "api_base": "https://api.moonshot.ai/v1",
        "hint": "api.moonshot.ai (Kimi K2.5)",
    },
    "moonshot-cn": {
        "label": "Moonshot CN",
        "api_base": "https://api.moonshot.cn/v1",
        "hint": "api.moonshot.cn (Kimi K2.5)",
    },
}

# Moonshot model catalog
_MOONSHOT_MODELS: list[str] = [
    "kimi-k2.5",
    "kimi-k2-turbo-preview",
    "k2p5",  # Kimi Coding
]

# Moonshot default models per endpoint
_MOONSHOT_DEFAULT_MODELS: dict[str, str] = {
    "moonshot-international": "kimi-k2.5",
    "moonshot-cn": "kimi-k2.5",
}

_STATIC_FILTER_SKIP_PROVIDERS: set[str] = {
    "aiohttp_openai",
    "azure",
    "azure_ai",
    "azure_text",
    "custom",
    "custom_openai",
    "databricks",
    "litellm_proxy",
    "llamafile",
    "lm_studio",
    "ollama_chat",
    "openai_like",
    "openrouter",
    "oobabooga",
    "predibase",
    "sagemaker",
    "sagemaker_chat",
    "snowflake",
    "hosted_vllm",
    "xinference",
}

# Qianfan (Baidu) model catalog
_QIANFAN_MODELS: list[str] = [
    "deepseek-v3.2",
    "ernie-5.0-thinking-preview",
    "ernie-4.0-8k",
    "ernie-4.0-turbo-8k",
    "ernie-3.5-8k",
]

# Ollama default configuration
# Note: Ollama models are auto-discovered via /api/tags endpoint
_OLLAMA_DEFAULT_MODEL: str = "llama3.2"

# Ollama is OpenAI-compatible, uses local server
_OLLAMA_MODELS: list[str] = [
    "llama3.2",
    "llama3.1",
    "qwen2.5",
    "deepseek-r1",
    "mistral",
    "codellama",
]

# vLLM default configuration
# Note: vLLM models are auto-discovered via /models endpoint
_VLLM_DEFAULT_MODEL: str = "meta-llama/Llama-3.2-3B-Instruct"

# vLLM is OpenAI-compatible, uses local server
_VLLM_MODELS: list[str] = [
    "meta-llama/Llama-3.2-3B-Instruct",
    "meta-llama/Llama-3.1-8B-Instruct",
    "Qwen/Qwen2.5-7B-Instruct",
    "mistralai/Mistral-7B-Instruct-v0.3",
]

# Mistral AI model catalog
_MISTRAL_MODELS: list[str] = [
    "mistral-large-latest",
    "mistral-large-2411",
    "pixtral-12b-2409",
    "mistral-nemo",
    "open-mistral-7b",
    "open-mixtral-8x7b",
    "open-mixtral-8x22b",
]

_MISTRAL_DEFAULT_MODEL: str = "mistral-large-latest"

# Together AI model catalog
_TOGETHER_MODELS: list[str] = [
    "meta-llama/Meta-Llama-3.1-70B-Instruct-Turbo",
    "meta-llama/Meta-Llama-3.1-8B-Instruct-Turbo",
    "Qwen/Qwen2.5-72B-Instruct-Turbo",
    "mistralai/Mixtral-8x22B-Instruct-v0.1",
    "deepseek-ai/DeepSeek-V3",
    "google/gemma-2-27b-it",
]

_TOGETHER_DEFAULT_MODEL: str = "meta-llama/Meta-Llama-3.1-70B-Instruct-Turbo"

# HuggingFace model catalog (popular models for inference)
_HUGGINGFACE_MODELS: list[str] = [
    "meta-llama/Llama-3.1-70B-Instruct",
    "meta-llama/Llama-3.1-8B-Instruct",
    "Qwen/Qwen2.5-72B-Instruct",
    "mistralai/Mistral-7B-Instruct-v0.3",
    "bigcode/starcoder2-15b",
]

_HUGGINGFACE_DEFAULT_MODEL: str = "meta-llama/Llama-3.1-70B-Instruct"

# Qwen (Alibaba DashScope) model catalog
_QWEN_MODELS: list[str] = [
    "qwen-max",
    "qwen-plus",
    "qwen-turbo",
    "qwen-long",
    "qwen-vl-max",
    "qwen-vl-plus",
]

_QWEN_DEFAULT_MODEL: str = "qwen-max"

# xAI (Grok) model catalog
_XAI_MODELS: list[str] = [
    "grok-4",
]

_XAI_DEFAULT_MODEL: str = "grok-4"

# Kilo Gateway model catalog (OpenRouter-compatible)
_KILOCODE_MODELS: list[str] = [
    "openai/gpt-4o",
    "openai/gpt-4o-mini",
    "openai/gpt-4-turbo",
    "anthropic/claude-3-5-sonnet-20241022",
    "anthropic/claude-3-5-haiku-20241022",
    "google/gemini-2.0-flash-exp",
    "meta-llama/llama-3.1-405b-instruct",
]

_KILOCODE_DEFAULT_MODEL: str = "openai/gpt-4o"

# Vercel AI Gateway (proxy, no predefined models)
_AI_GATEWAY_MODELS: list[str] = []

_AI_GATEWAY_DEFAULT_MODEL: str = ""
