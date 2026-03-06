"""Shared helper utilities for wizard flows."""

from __future__ import annotations

import getpass
from typing import Optional
from urllib.parse import urlparse

from rich.console import Console

console = Console()


def _is_blank(value: object) -> bool:
    if value is None:
        return True
    if isinstance(value, str):
        return not value.strip()
    return False


def _ask_value(label: str) -> str:
    """Read a single-line value from terminal."""

    prompt_label = label
    try:
        value = console.input(
            f"{prompt_label}: ",
            markup=False,
        )
    except (KeyboardInterrupt, EOFError):
        return ""
    return value


def _mask_secret(value: str, keep: int = 4) -> str:
    value = value.strip()
    if not value:
        return ""
    if len(value) <= keep:
        return "*" * len(value)
    return "*" * (len(value) - keep) + value[-keep:]


def _is_valid_url(value: str) -> bool:
    try:
        parsed = urlparse(value)
    except Exception:
        return False
    return bool(parsed.scheme and parsed.netloc)


def _looks_like_api_base(value: str) -> bool:
    lowered = value.lower()
    if "generatecontent" in lowered:
        return False
    if "/chat/completions" in lowered or "/completions" in lowered:
        return False
    if "/responses" in lowered:
        return False
    return True


def _prompt_secret_with_mask(prompt_text: str) -> Optional[str]:
    try:
        from prompt_toolkit import prompt as pt_prompt

        return pt_prompt(prompt_text, is_password=True)
    except Exception:
        pass

    try:
        return getpass.getpass(prompt_text)
    except (KeyboardInterrupt, EOFError):
        return None


def _normalize_filter_tokens(query: str) -> list[str]:
    normalized = []
    for chunk in query.lower().split():
        token = "".join(
            char for char in chunk if char.isalnum() or char in {"/", "_", "-", "."}
        )
        if token:
            normalized.append(token)
    return normalized


def _sanitize_filter_input(value: str) -> str:
    return "".join(
        char
        for char in value
        if char.isprintable() and char not in {"\r", "\n", "\u2028", "\u2029"}
    )


def _matches_filter_query(query: str, candidates: list[str]) -> bool:
    tokens = _normalize_filter_tokens(_sanitize_filter_input(query))
    if not tokens:
        return True

    haystack = " ".join(str(item).lower() for item in candidates if str(item).strip())
    if not haystack:
        return False
    return all(token in haystack for token in tokens)


def _display_width(text: str) -> int:
    import unicodedata

    width = 0
    for char in text:
        width += 2 if unicodedata.east_asian_width(char) in ("W", "F") else 1
    return width
