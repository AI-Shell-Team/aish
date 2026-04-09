"""Provider-related setup wizard helpers."""

from __future__ import annotations

from typing import Optional

from ..i18n import t
from ..llm.loader import load_litellm
from .constants import (
    _PROVIDER_ALIASES,
    _PROVIDER_BASES,
    _PROVIDER_ENV_KEYS,
    _PROVIDER_LABELS,
    _PROVIDER_PRIORITY,
)
from .helpers import _is_valid_url, _looks_like_api_base, _matches_filter_query
from .types import ProviderOption


def _with_api_base(provider: ProviderOption, api_base: Optional[str]) -> ProviderOption:
    if not api_base or api_base == provider.api_base:
        return provider
    return ProviderOption(
        key=provider.key,
        label=provider.label,
        api_base=api_base,
        env_key=provider.env_key,
        allow_custom_model=provider.allow_custom_model,
        requires_api_base=provider.requires_api_base,
    )


def _probe_model_for_provider(
    provider: ProviderOption, model_hint: Optional[str]
) -> str:
    if model_hint:
        if "/" in model_hint:
            return model_hint
        return f"{provider.key}/{model_hint}"
    return f"{provider.key}/_"


def _infer_api_base_from_litellm(
    litellm,
    *,
    provider: ProviderOption,
    api_key: Optional[str],
    model_hint: Optional[str],
) -> Optional[str]:
    if provider.requires_api_base or provider.key == "custom":
        return None
    probe_model = _probe_model_for_provider(provider, model_hint)
    optional_params = {"api_key": api_key} if api_key else {}

    api_base = None
    try:
        api_base = litellm.get_api_base(
            model=probe_model,
            optional_params=optional_params,
        )
    except Exception:
        api_base = None

    if isinstance(api_base, str):
        api_base = api_base.strip()
        if _is_valid_url(api_base) and _looks_like_api_base(api_base):
            return api_base

    try:
        _, _, _, candidate = litellm.get_llm_provider(
            model=probe_model,
            api_key=api_key,
        )
    except Exception:
        candidate = None

    if isinstance(candidate, str):
        candidate = candidate.strip()
        if _is_valid_url(candidate) and _looks_like_api_base(candidate):
            return candidate
    return None


def _maybe_resolve_api_base(
    provider: ProviderOption,
    *,
    api_key: Optional[str],
    model_hint: Optional[str] = None,
) -> ProviderOption:
    if provider.api_base or provider.requires_api_base or provider.key == "custom":
        return provider
    litellm = load_litellm()
    if litellm is None:
        return provider
    api_base = _infer_api_base_from_litellm(
        litellm,
        provider=provider,
        api_key=api_key,
        model_hint=model_hint,
    )
    return _with_api_base(provider, api_base)


def _get_provider_options() -> list[ProviderOption]:
    provider_names = list(_PROVIDER_PRIORITY)

    options: list[ProviderOption] = []
    seen: set[str] = set()
    for name in provider_names:
        key = name.lower()
        canonical = _PROVIDER_ALIASES.get(key, key)
        if canonical in seen:
            continue
        seen.add(canonical)
        label = _PROVIDER_LABELS.get(canonical)
        if not label:
            label = name.replace("_", " ").replace("-", " ").title()
        options.append(
            ProviderOption(
                key=canonical,
                label=label,
                api_base=_PROVIDER_BASES.get(canonical),
                env_key=_PROVIDER_ENV_KEYS.get(canonical),
                allow_custom_model=True,
                requires_api_base=False,
            )
        )

    options.append(
        ProviderOption(
            key="custom",
            label=t("cli.setup.provider_custom_default"),
            api_base=None,
            env_key=None,
            allow_custom_model=True,
            requires_api_base=True,
        )
    )
    return options


def _provider_note(provider: ProviderOption) -> str:
    if provider.requires_api_base:
        return t("cli.setup.provider_custom_note")
    if provider.api_base:
        return t("cli.setup.provider_preset_base")
    return ""


def _filter_provider_options(
    options: list[ProviderOption], query: str
) -> list[ProviderOption]:
    if not query.strip():
        return options
    return [
        option
        for option in options
        if _matches_filter_query(query, [option.label, option.key])
    ]
