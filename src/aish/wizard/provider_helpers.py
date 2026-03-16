"""Provider endpoint helpers for multi-endpoint providers."""

from __future__ import annotations

from dataclasses import dataclass

from .constants import (
    _AI_GATEWAY_DEFAULT_MODEL,
    _AI_GATEWAY_MODELS,
    _HUGGINGFACE_DEFAULT_MODEL,
    _HUGGINGFACE_MODELS,
    _KILOCODE_DEFAULT_MODEL,
    _KILOCODE_MODELS,
    _MINIMAX_DEFAULT_MODELS,
    _MINIMAX_ENDPOINTS,
    _MINIMAX_MODELS,
    _MISTRAL_DEFAULT_MODEL,
    _MISTRAL_MODELS,
    _MOONSHOT_DEFAULT_MODELS,
    _MOONSHOT_ENDPOINTS,
    _MOONSHOT_MODELS,
    _OLLAMA_DEFAULT_MODEL,
    _OLLAMA_MODELS,
    _QIANFAN_MODELS,
    _QWEN_DEFAULT_MODEL,
    _QWEN_MODELS,
    _TOGETHER_DEFAULT_MODEL,
    _TOGETHER_MODELS,
    _VLLM_DEFAULT_MODEL,
    _VLLM_MODELS,
    _XAI_DEFAULT_MODEL,
    _XAI_MODELS,
    _ZAI_DEFAULT_MODELS,
    _ZAI_ENDPOINTS,
    _ZAI_MODELS,
)


@dataclass
class ProviderEndpointInfo:
    """Provider endpoint information."""

    key: str
    label: str
    api_base: str
    hint: str
    default_model: str


def get_zai_endpoints() -> list[ProviderEndpointInfo]:
    """Get all available Z.AI endpoints."""
    return [
        ProviderEndpointInfo(
            key=key,
            label=value["label"],
            api_base=value["api_base"],
            hint=value["hint"],
            default_model=_ZAI_DEFAULT_MODELS.get(key, "glm-5"),
        )
        for key, value in _ZAI_ENDPOINTS.items()
    ]


def get_zai_models() -> list[str]:
    """Get all available Z.AI models."""
    return list(_ZAI_MODELS)


def get_default_model_for_zai_endpoint(endpoint_key: str) -> str:
    """Get the default model for a specific Z.AI endpoint."""
    return _ZAI_DEFAULT_MODELS.get(endpoint_key, "glm-5")


def get_minimax_endpoints() -> list[ProviderEndpointInfo]:
    """Get all available MiniMax endpoints."""
    return [
        ProviderEndpointInfo(
            key=key,
            label=value["label"],
            api_base=value["api_base"],
            hint=value["hint"],
            default_model=_MINIMAX_DEFAULT_MODELS.get(key, "MiniMax-M2.5"),
        )
        for key, value in _MINIMAX_ENDPOINTS.items()
    ]


def get_minimax_models() -> list[str]:
    """Get all available MiniMax models."""
    return list(_MINIMAX_MODELS)


def get_default_model_for_minimax_endpoint(endpoint_key: str) -> str:
    """Get the default model for a specific MiniMax endpoint."""
    return _MINIMAX_DEFAULT_MODELS.get(endpoint_key, "MiniMax-M2.5")


def get_moonshot_endpoints() -> list[ProviderEndpointInfo]:
    """Get all available Moonshot endpoints."""
    return [
        ProviderEndpointInfo(
            key=key,
            label=value["label"],
            api_base=value["api_base"],
            hint=value["hint"],
            default_model=_MOONSHOT_DEFAULT_MODELS.get(key, "kimi-k2.5"),
        )
        for key, value in _MOONSHOT_ENDPOINTS.items()
    ]


def get_moonshot_models() -> list[str]:
    """Get all available Moonshot models."""
    return list(_MOONSHOT_MODELS)


def get_default_model_for_moonshot_endpoint(endpoint_key: str) -> str:
    """Get the default model for a specific Moonshot endpoint."""
    return _MOONSHOT_DEFAULT_MODELS.get(endpoint_key, "kimi-k2.5")


def get_provider_endpoints(provider_key: str) -> list[ProviderEndpointInfo]:
    """Get endpoints for a specific provider."""
    endpoints_map = {
        "zai": get_zai_endpoints,
        "minimax": get_minimax_endpoints,
        "moonshot": get_moonshot_endpoints,
    }
    getter = endpoints_map.get(provider_key)
    if getter:
        return getter()
    return []


def get_provider_models(provider_key: str) -> list[str]:
    """Get models for a specific provider."""
    models_map = {
        "zai": get_zai_models,
        "minimax": get_minimax_models,
        "moonshot": get_moonshot_models,
        "qianfan": get_qianfan_models,
        "ollama": get_ollama_models,
        "vllm": get_vllm_models,
        "mistral": get_mistral_models,
        "together": get_together_models,
        "huggingface": get_huggingface_models,
        "qwen": get_qwen_models,
        "xai": get_xai_models,
        "kilocode": get_kilocode_models,
        "ai_gateway": get_ai_gateway_models,
    }
    getter = models_map.get(provider_key)
    if getter:
        return getter()
    return []


def get_default_model_for_endpoint(provider_key: str, endpoint_key: str) -> str:
    """Get the default model for a specific provider endpoint."""
    defaults_map = {
        "zai": get_default_model_for_zai_endpoint,
        "minimax": get_default_model_for_minimax_endpoint,
        "moonshot": get_default_model_for_moonshot_endpoint,
    }
    getter = defaults_map.get(provider_key)
    if getter:
        return getter(endpoint_key)
    return ""


def has_multi_endpoints(provider_key: str) -> bool:
    """Check if a provider has multiple endpoints."""
    return provider_key in {"zai", "minimax", "moonshot"}


def get_qianfan_models() -> list[str]:
    """Get all available Qianfan models."""
    return list(_QIANFAN_MODELS)


def get_ollama_models() -> list[str]:
    """Get all available Ollama models."""
    return list(_OLLAMA_MODELS)


def get_vllm_models() -> list[str]:
    """Get all available vLLM models."""
    return list(_VLLM_MODELS)


def get_mistral_models() -> list[str]:
    """Get all available Mistral AI models."""
    return list(_MISTRAL_MODELS)


def get_together_models() -> list[str]:
    """Get all available Together AI models."""
    return list(_TOGETHER_MODELS)


def get_huggingface_models() -> list[str]:
    """Get all available HuggingFace models."""
    return list(_HUGGINGFACE_MODELS)


def get_qwen_models() -> list[str]:
    """Get all available Qwen models."""
    return list(_QWEN_MODELS)


def get_xai_models() -> list[str]:
    """Get all available xAI models."""
    return list(_XAI_MODELS)


def get_kilocode_models() -> list[str]:
    """Get all available Kilo Gateway models."""
    return list(_KILOCODE_MODELS)


def get_ai_gateway_models() -> list[str]:
    """Get all available Vercel AI Gateway models (proxy, no predefined)."""
    return list(_AI_GATEWAY_MODELS)


def get_default_model_for_qianfan() -> str:
    """Get the default model for Qianfan."""
    return _QIANFAN_MODELS[0] if _QIANFAN_MODELS else "deepseek-v3.2"


def get_default_model_for_ollama() -> str:
    """Get the default model for Ollama."""
    return _OLLAMA_DEFAULT_MODEL


def get_default_model_for_vllm() -> str:
    """Get the default model for vLLM."""
    return _VLLM_DEFAULT_MODEL


def get_default_model_for_mistral() -> str:
    """Get the default model for Mistral AI."""
    return _MISTRAL_DEFAULT_MODEL


def get_default_model_for_together() -> str:
    """Get the default model for Together AI."""
    return _TOGETHER_DEFAULT_MODEL


def get_default_model_for_huggingface() -> str:
    """Get the default model for HuggingFace."""
    return _HUGGINGFACE_DEFAULT_MODEL


def get_default_model_for_qwen() -> str:
    """Get the default model for Qwen."""
    return _QWEN_DEFAULT_MODEL


def get_default_model_for_xai() -> str:
    """Get the default model for xAI."""
    return _XAI_DEFAULT_MODEL


def get_default_model_for_kilocode() -> str:
    """Get the default model for Kilo Gateway."""
    return _KILOCODE_DEFAULT_MODEL


def get_default_model_for_ai_gateway() -> str:
    """Get the default model for Vercel AI Gateway (proxy, no default)."""
    return _AI_GATEWAY_DEFAULT_MODEL
