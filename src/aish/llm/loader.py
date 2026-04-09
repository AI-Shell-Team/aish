"""Shared LiteLLM loading helpers.

Centralizes lazy import, quiet flags, and optional background preload so
different modules don't duplicate import/cache logic.
"""

from __future__ import annotations

import logging
import os
import threading
from typing import Optional

_SENTINEL = object()
_cached_litellm: object = _SENTINEL
_preload_thread: Optional[threading.Thread] = None

# Proxy environment variables that may cause issues with invalid URLs
PROXY_ENV_VARS = (
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "http_proxy",
    "https_proxy",
    "ALL_PROXY",
    "all_proxy",
    "NO_PROXY",
    "no_proxy",
)


def _sanitize_url_value(value: str) -> str:
    """Sanitize URL value by taking only the first line and trimming whitespace.

    If the value contains newlines or other control characters, only the first
    line is kept to prevent URL parsing errors from malformed values.
    """
    # Split on first newline and take only the first part
    first_line = value.split("\n")[0].split("\r")[0]
    return first_line.strip()


def _sanitize_proxy_env() -> None:
    """Sanitize proxy environment variables to remove invalid characters."""
    for var in PROXY_ENV_VARS:
        value = os.environ.get(var)
        if value:
            sanitized = _sanitize_url_value(value)
            if sanitized != value:
                os.environ[var] = sanitized


def _configure_litellm(litellm: object) -> None:
    """Best-effort reduce LiteLLM log/debug noise."""
    litellm_logger = logging.getLogger("litellm")
    if litellm_logger.level < logging.WARNING:
        litellm_logger.setLevel(logging.WARNING)

    for attr in ("suppress_debug_info", "disable_debug_info"):
        if hasattr(litellm, attr):
            try:
                setattr(litellm, attr, True)
            except Exception:
                pass

    if hasattr(litellm, "set_verbose"):
        try:
            setattr(litellm, "set_verbose", False)
        except Exception:
            pass


def load_litellm() -> object | None:
    """Import LiteLLM once and return cached module (or None if unavailable)."""
    global _cached_litellm

    if _cached_litellm is not _SENTINEL:
        return _cached_litellm

    t = _preload_thread
    if t is not None and t.is_alive() and t is not threading.current_thread():
        t.join()
        if _cached_litellm is not _SENTINEL:
            return _cached_litellm

    # Sanitize proxy env vars to remove invalid characters before import
    _sanitize_proxy_env()

    try:
        import litellm
    except ImportError:
        _cached_litellm = None
        return None

    _configure_litellm(litellm)
    _cached_litellm = litellm
    return litellm


def preload_litellm() -> None:
    """Start daemon preload thread once; no-op if already loaded/preloading."""
    global _preload_thread

    if _preload_thread is not None:
        return
    if _cached_litellm is not _SENTINEL:
        return
    _preload_thread = threading.Thread(target=load_litellm, daemon=True)
    _preload_thread.start()
