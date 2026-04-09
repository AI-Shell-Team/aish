from __future__ import annotations

from dataclasses import dataclass
from urllib.parse import urlsplit
from typing import Any, Awaitable, Callable

from ...config import ConfigModel
from ...i18n import t
from ...wizard.constants import _PROVIDER_ALIASES, _PROVIDER_ENV_KEYS, _PROVIDER_LABELS
from .interface import (ProviderAuthConfig, ProviderContract, ProviderMetadata,
                        ProviderUsageStatus)

_PROVIDER_DASHBOARD_URLS: dict[str, str] = {
    "openai-codex": "https://codex.ai/settings",
    "openai": "https://platform.openai.com/usage",
    "anthropic": "https://console.anthropic.com/settings/usage",
    "deepseek": "https://platform.deepseek.com/usage",
    "gemini": "https://aistudio.google.com/app/usage",
    "google": "https://aistudio.google.com/app/usage",
    "minimax": "https://platform.minimaxi.com/user-center/basic-information/interface-key",
    "moonshot": "https://platform.moonshot.ai/console/api-keys",
    "zai": "https://platform.z.ai/usage",
    "openrouter": "https://openrouter.ai/settings/credits",
    "azure": "https://portal.azure.com/",
    "qianfan": "https://console.bce.baidu.com/qianfan/ais/console/applicationConsole/application",
    "mistral": "https://console.mistral.ai/usage",
    "together": "https://api.together.xyz/settings/api-keys",
    "huggingface": "https://huggingface.co/settings/tokens",
    "qwen": "https://dashscope.console.aliyun.com/overview",
    "xai": "https://console.x.ai/",
    "kilocode": "https://dashboard.kilocode.ai/usage",
    "ai_gateway": "https://vercel.com/dashboard/ai-gateway",
    "bedrock": "https://console.aws.amazon.com/bedrock/",
}

_LOCAL_PROVIDER_IDS: frozenset[str] = frozenset({"ollama", "vllm"})

_MODEL_NAME_PROVIDER_HINTS: tuple[tuple[str, str], ...] = (
    ("claude", "anthropic"),
    ("deepseek", "deepseek"),
    ("gemini", "gemini"),
    ("grok", "xai"),
    ("mistral", "mistral"),
    ("moonshot", "moonshot"),
    ("qwen", "qwen"),
    ("glm", "zai"),
    ("openrouter", "openrouter"),
    ("gpt", "openai"),
    ("o1", "openai"),
    ("o3", "openai"),
    ("o4", "openai"),
)

_API_BASE_PROVIDER_HINTS: tuple[tuple[str, str], ...] = (
    ("codex.ai", "openai-codex"),
    ("chatgpt.com", "openai-codex"),
    ("openai.com", "openai"),
    ("anthropic.com", "anthropic"),
    ("deepseek.com", "deepseek"),
    ("googleapis.com", "google"),
    ("google.com", "google"),
    ("openrouter.ai", "openrouter"),
    ("azure.com", "azure"),
    ("baidubce.com", "qianfan"),
    ("mistral.ai", "mistral"),
    ("together.xyz", "together"),
    ("huggingface.co", "huggingface"),
    ("aliyuncs.com", "qwen"),
    ("x.ai", "xai"),
    ("kilocode.ai", "kilocode"),
    ("vercel.ai", "ai_gateway"),
    ("127.0.0.1:11434", "ollama"),
    ("localhost:11434", "ollama"),
    ("127.0.0.1:8000", "vllm"),
    ("localhost:8000", "vllm"),
)


def _canonicalize_provider_id(provider_id: str | None) -> str | None:
    if not provider_id:
        return None
    normalized = provider_id.strip().lower().replace("_", "-")
    return _PROVIDER_ALIASES.get(normalized, normalized)


def _infer_provider_id_from_model(model: str | None) -> str | None:
    trimmed = (model or "").strip().lower()
    if not trimmed:
        return None

    if "/" in trimmed:
        prefix = _canonicalize_provider_id(trimmed.split("/", 1)[0])
        if prefix and (prefix in _PROVIDER_LABELS or prefix == "openai-codex"):
            return prefix

    for needle, provider_id in _MODEL_NAME_PROVIDER_HINTS:
        if needle in trimmed:
            return provider_id
    return None


def _infer_provider_id_from_api_base(api_base: str | None) -> str | None:
    trimmed = (api_base or "").strip()
    if not trimmed:
        return None

    try:
        parsed = urlsplit(trimmed)
    except Exception:
        return None

    host = (parsed.netloc or "").lower()
    path = (parsed.path or "").lower()
    candidate = host + path
    if not candidate:
        return None

    for needle, provider_id in _API_BASE_PROVIDER_HINTS:
        if needle in candidate:
            return provider_id
    return None


def _build_provider_metadata(provider_id: str, *, display_name: str | None = None) -> ProviderMetadata:
    canonical = _canonicalize_provider_id(provider_id) or provider_id
    return ProviderMetadata(
        provider_id=canonical,
        display_name=display_name or _PROVIDER_LABELS.get(canonical, canonical.replace("-", " ").title()),
        dashboard_url=_PROVIDER_DASHBOARD_URLS.get(canonical),
        api_key_env_var=_PROVIDER_ENV_KEYS.get(canonical),
    )


@dataclass(frozen=True)
class LiteLLMProviderAdapter:
    provider_id: str = "litellm"
    model_prefix: str = ""
    display_name: str = "LiteLLM"
    uses_litellm: bool = True
    supports_streaming: bool = True
    should_trim_messages: bool = True
    auth_config: ProviderAuthConfig | None = None
    metadata: ProviderMetadata = ProviderMetadata(
        provider_id="litellm",
        display_name="LiteLLM",
    )

    def matches_model(self, model: str | None) -> bool:
        return True

    def get_usage_status(self, config: ConfigModel) -> ProviderUsageStatus | None:
        return None

    async def create_completion(
        self,
        *,
        model: str,
        config: ConfigModel,
        api_base: str | None,
        api_key: str | None,
        messages: list[dict[str, Any]],
        stream: bool,
        tools: list[dict[str, Any]] | None = None,
        tool_choice: str = "auto",
        fallback_completion: Callable[..., Awaitable[Any]] | None = None,
        **kwargs: Any,
    ) -> Any:
        if fallback_completion is None:
            raise RuntimeError("LiteLLM provider requires a fallback completion callable.")

        return await fallback_completion(
            model=model,
            api_base=api_base,
            api_key=api_key,
            messages=messages,
            stream=stream,
            tools=tools,
            tool_choice=tool_choice,
            **kwargs,
        )

    async def validate_model_switch(
        self,
        *,
        model: str,
        config: ConfigModel,
    ) -> str | None:
        from aish.wizard.verification import build_failure_reason, run_verification_async

        connectivity, tool_support = await run_verification_async(
            model=model,
            api_base=config.api_base,
            api_key=config.api_key,
        )
        if connectivity.ok and tool_support.supports is True:
            return None

        reason = build_failure_reason(connectivity, tool_support)
        return t("shell.model.verify_failed", reason=reason)


DEFAULT_PROVIDER = LiteLLMProviderAdapter()


def _registered_providers() -> tuple[ProviderContract, ...]:
    from .openai_codex import OPENAI_CODEX_PROVIDER_ADAPTER

    return (OPENAI_CODEX_PROVIDER_ADAPTER, DEFAULT_PROVIDER)


def get_provider_for_model(model: str | None) -> ProviderContract:
    for provider in _registered_providers():
        if provider.matches_model(model):
            return provider
    return DEFAULT_PROVIDER


def get_provider_by_id(provider_id: str) -> ProviderContract | None:
    normalized = _canonicalize_provider_id(provider_id)
    if normalized is None:
        return None
    for provider in _registered_providers():
        if provider.provider_id == normalized:
            return provider
    return None


def list_auth_capable_provider_ids() -> tuple[str, ...]:
    return tuple(
        provider.provider_id
        for provider in _registered_providers()
        if provider.auth_config is not None
    )


def resolve_provider_metadata(
    model: str | None,
    api_base: str | None = None,
) -> ProviderMetadata:
    registered_provider = get_provider_for_model(model)
    if registered_provider.provider_id != DEFAULT_PROVIDER.provider_id:
        return registered_provider.metadata

    provider_id = _infer_provider_id_from_api_base(api_base) or _infer_provider_id_from_model(
        model
    )
    if provider_id is None:
        return _build_provider_metadata(
            "litellm",
            display_name=t("cli.models_usage.unknown_provider"),
        )

    metadata = _build_provider_metadata(provider_id)
    if metadata.provider_id in _LOCAL_PROVIDER_IDS and api_base:
        return ProviderMetadata(
            provider_id=metadata.provider_id,
            display_name=metadata.display_name,
            dashboard_url=api_base,
            api_key_env_var=metadata.api_key_env_var,
        )

    return metadata