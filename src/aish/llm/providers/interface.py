from __future__ import annotations

import os
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Awaitable, Callable, Mapping, Protocol

from ...config import ConfigModel


class AuthStateLike(Protocol):
    auth_path: Path


@dataclass(frozen=True)
class ProviderMetadata:
    provider_id: str
    display_name: str
    dashboard_url: str | None = None
    api_key_env_var: str | None = None


@dataclass(frozen=True)
class ProviderUsageStatus:
    summary: str
    style: str = "dim"
    details: tuple[str, ...] = ()


@dataclass(frozen=True)
class ProviderAuthConfig:
    auth_path_config_key: str
    default_model: str
    load_auth_state: Callable[[str | os.PathLike[str] | None], AuthStateLike]
    login_handlers: Mapping[str, Callable[..., AuthStateLike]]

    def get_login_handler(self, flow: str) -> Callable[..., AuthStateLike] | None:
        return self.login_handlers.get(flow)

    @property
    def supported_flows(self) -> tuple[str, ...]:
        return tuple(self.login_handlers.keys())


class ProviderContract(Protocol):
    @property
    def provider_id(self) -> str: ...

    @property
    def model_prefix(self) -> str: ...

    @property
    def display_name(self) -> str: ...

    @property
    def uses_litellm(self) -> bool: ...

    @property
    def supports_streaming(self) -> bool: ...

    @property
    def should_trim_messages(self) -> bool: ...

    @property
    def auth_config(self) -> ProviderAuthConfig | None: ...

    @property
    def metadata(self) -> ProviderMetadata: ...

    def matches_model(self, model: str | None) -> bool: ...

    def get_usage_status(self, config: ConfigModel) -> ProviderUsageStatus | None: ...

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
    ) -> Any: ...

    async def validate_model_switch(
        self,
        *,
        model: str,
        config: ConfigModel,
    ) -> str | None: ...