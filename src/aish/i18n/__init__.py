from __future__ import annotations

import os
from dataclasses import dataclass
from typing import Any

import yaml

try:
    from importlib import resources as importlib_resources
except Exception:  # pragma: no cover
    import importlib_resources  # type: ignore


_UI_LOCALE: str | None = None
_MESSAGES: dict[str, Any] | None = None
_MESSAGES_EN: dict[str, Any] | None = None

_LANG_PREFIX_TO_UI_LOCALE = {
    "zh": "zh-CN",
    "de": "de-DE",
    "es": "es-ES",
    "fr": "fr-FR",
    "ja": "ja-JP",
    "en": "en-US",
}


def _normalize_lang_to_ui_locale(lang_value: str | None) -> str:
    if not lang_value:
        return "en-US"

    raw = str(lang_value).strip()
    if not raw or raw.upper() in {"C", "POSIX"}:
        return "en-US"

    # Common formats: zh_CN.UTF-8, en_US.UTF-8, zh_CN, en_US
    main = raw.split(".", 1)[0]
    main = main.split("@", 1)[0]
    normalized = main.replace("_", "-").lower()
    lang_prefix = normalized.split("-", 1)[0]

    return _LANG_PREFIX_TO_UI_LOCALE.get(lang_prefix, "en-US")


def get_ui_locale() -> str:
    """Return the UI locale (fixed for the process).

    Requirement: only read $LANG and fix it at startup.
    """

    global _UI_LOCALE
    if _UI_LOCALE is not None:
        return _UI_LOCALE

    _UI_LOCALE = _normalize_lang_to_ui_locale(os.getenv("LANG"))
    return _UI_LOCALE


def _load_yaml_resource(filename: str) -> dict[str, Any]:
    try:
        package = __name__  # aish.i18n
        text = (
            importlib_resources.files(package)
            .joinpath(filename)
            .read_text(encoding="utf-8")
        )
    except Exception:
        # Fallback: empty dict rather than crashing UI
        return {}

    try:
        data = yaml.safe_load(text) or {}
        return data if isinstance(data, dict) else {}
    except Exception:
        return {}


def _ensure_messages_loaded() -> None:
    global _MESSAGES, _MESSAGES_EN
    if _MESSAGES is not None and _MESSAGES_EN is not None:
        return

    _MESSAGES_EN = _load_yaml_resource("en-US.yaml")
    ui_locale = get_ui_locale()
    if ui_locale == "en-US":
        _MESSAGES = _MESSAGES_EN
        return

    localized_messages = _load_yaml_resource(f"{ui_locale}.yaml")
    _MESSAGES = localized_messages or _MESSAGES_EN


def _lookup(messages: dict[str, Any], dotted_key: str) -> str | None:
    current: Any = messages
    for part in dotted_key.split("."):
        if not isinstance(current, dict) or part not in current:
            return None
        current = current[part]

    if isinstance(current, str):
        return current
    return None


def _lookup_value(messages: dict[str, Any], dotted_key: str) -> Any | None:
    current: Any = messages
    for part in dotted_key.split("."):
        if not isinstance(current, dict) or part not in current:
            return None
        current = current[part]
    return current


def get_value(key: str) -> Any | None:
    """Get a raw (possibly non-string) i18n value.

    Useful for structured resources (lists/dicts) such as built-in help definitions.
    Falls back to en-US.
    """

    _ensure_messages_loaded()
    return _lookup_value(_MESSAGES or {}, key) or _lookup_value(_MESSAGES_EN or {}, key)


def t(key: str, **kwargs: Any) -> str:
    """Translate a dotted key to a localized string.

    - Uses YAML resources under aish/i18n/
    - Falls back to en-US, then to the key itself
    """

    _ensure_messages_loaded()

    msg = _lookup(_MESSAGES or {}, key) or _lookup(_MESSAGES_EN or {}, key)
    if msg is None:
        return key

    if kwargs:
        try:
            return msg.format(**kwargs)
        except Exception:
            return msg

    return msg


@dataclass(frozen=True)
class I18nStr:
    """Lazy translatable string for Typer/Click help text."""

    key: str

    def __str__(self) -> str:
        return t(self.key)


def reset_i18n_for_tests() -> None:  # pragma: no cover
    """Reset cached locale/messages for tests."""

    global _UI_LOCALE, _MESSAGES, _MESSAGES_EN
    _UI_LOCALE = None
    _MESSAGES = None
    _MESSAGES_EN = None
