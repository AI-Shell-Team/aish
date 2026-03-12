from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Awaitable, Callable

from ..config import ConfigModel
from ..i18n import t
from .interface import ProviderAuthConfig, ProviderContract


@dataclass(frozen=True)
class LiteLLMProviderAdapter:
    provider_id: str = "litellm"
    model_prefix: str = ""
    display_name: str = "LiteLLM"
    uses_litellm: bool = True
    supports_streaming: bool = True
    should_trim_messages: bool = True
    auth_config: ProviderAuthConfig | None = None

    def matches_model(self, model: str | None) -> bool:
        return True

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
    normalized = provider_id.strip().lower().replace("_", "-")
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