from __future__ import annotations

import importlib
import re
from functools import lru_cache
from typing import Optional

# IMPORTANT: Do not import litellm at module import time.
# Importing litellm is expensive (~seconds) and would slow down `import aish.shell`.


@lru_cache(maxsize=1)
def _get_litellm_module() -> object | None:
    """Best-effort lazy import for litellm.

    Cached so we attempt the import at most once per process.
    """

    try:
        return importlib.import_module("litellm")
    except Exception:  # pragma: no cover
        return None


class LiteLLMError(Exception):
    """Base exception for all custom LiteLLM-related errors."""

    def __init__(self, message: str, original_exception: Optional[Exception] = None):
        super().__init__(message)
        self.original_exception = original_exception


class RateLimitError(LiteLLMError):
    """Raised when the API rate limit is exceeded."""

    pass


class InvalidRequestError(LiteLLMError):
    """Raised when the request is invalid (e.g., bad model name, malformed input)."""

    pass


class AuthenticationError(LiteLLMError):
    """Raised when API key or authentication fails."""

    pass


class TimeoutError(LiteLLMError):
    """Raised when the request times out."""

    pass


class ServiceUnavailableError(LiteLLMError):
    """Raised when the service is temporarily unavailable."""

    pass


class ContextWindowExceededError(LiteLLMError):
    """Raised when the input exceeds the model's context window."""

    pass


class UnknownLiteLLMError(LiteLLMError):
    """Fallback for any unhandled LiteLLM exceptions."""

    pass


LITELLM_EXCEPTION_NAMES: set[str] = {
    "RateLimitError",
    "InvalidRequestError",
    "BadRequestError",
    "NotFoundError",
    "AuthenticationError",
    "Timeout",
    "ServiceUnavailableError",
    "ContextWindowExceededError",
    "APIConnectionError",
    "APIError",
    "OpenAIError",
}


_SENSITIVE_PATTERNS: list[re.Pattern[str]] = [
    # OpenAI-like keys and key-like test strings (allow underscore/dash)
    re.compile(r"\bsk-[A-Za-z0-9_-]{10,}\b"),
    re.compile(r"(?i)(api[_-]?key\s*[:=]\s*)([^\s'\"]+)"),
    re.compile(r"(?i)(Authorization\s*[:=]\s*Bearer\s+)([^\s'\"]+)"),
]


def redact_secrets(text: str) -> str:
    if not text:
        return text
    result = text
    for pat in _SENSITIVE_PATTERNS:

        def _repl(m: re.Match[str]) -> str:
            # Patterns that capture a prefix (group 1) keep the prefix.
            if m.lastindex and m.lastindex >= 1:
                return m.group(1) + "***"
            # Full-token matches (e.g., sk-...) get fully masked.
            whole = m.group(0)
            if whole.lower().startswith("sk-"):
                return "sk-***"
            return "***"

        result = pat.sub(_repl, result)
    return result


@lru_cache(maxsize=None)
def _get_litellm_exception_type(name: str) -> type[BaseException] | None:
    litellm = _get_litellm_module()
    if litellm is None:
        return None

    exc_type = getattr(litellm, name, None)
    if isinstance(exc_type, type) and issubclass(exc_type, BaseException):
        return exc_type

    exc_mod = getattr(litellm, "exceptions", None)
    if exc_mod is None:
        return None
    exc_type = getattr(exc_mod, name, None)
    if isinstance(exc_type, type) and issubclass(exc_type, BaseException):
        return exc_type
    return None


def is_litellm_exception(e: Exception) -> bool:
    name = type(e).__name__
    mod = type(e).__module__

    if isinstance(mod, str) and mod.startswith("litellm"):
        return True

    # Best-effort fallback (useful across litellm versions and for tests)
    if name in LITELLM_EXCEPTION_NAMES:
        return True

    return False


def handle_litellm_exception(e: Exception) -> LiteLLMError:
    """Maps a raw litellm exception to a custom LiteLLMError subclass."""

    # First, prefer mapping by exception type name (version-robust).
    name = type(e).__name__
    msg = str(e)
    msg_lower = msg.lower()
    if name == "RateLimitError":
        return RateLimitError(msg, e)
    if name in {"InvalidRequestError", "BadRequestError", "NotFoundError"}:
        return InvalidRequestError(msg, e)
    if name == "AuthenticationError":
        return AuthenticationError(msg, e)
    if name in {"Timeout", "TimeoutError"}:
        return TimeoutError(msg, e)
    if name == "ServiceUnavailableError":
        return ServiceUnavailableError(msg, e)
    if name == "ContextWindowExceededError":
        return ContextWindowExceededError(msg, e)

    # Message heuristics for context window exceeded (provider-specific strings)
    if "context_length_exceeded" in msg or "maximum context length" in msg_lower:
        return ContextWindowExceededError(msg, e)

    # Then, if litellm is available, map by actual exception classes.
    if _get_litellm_module() is not None:
        mappings: tuple[tuple[str, type[LiteLLMError]], ...] = (
            ("RateLimitError", RateLimitError),
            ("InvalidRequestError", InvalidRequestError),
            ("BadRequestError", InvalidRequestError),
            ("NotFoundError", InvalidRequestError),
            ("AuthenticationError", AuthenticationError),
            ("Timeout", TimeoutError),
            ("TimeoutError", TimeoutError),
            ("ServiceUnavailableError", ServiceUnavailableError),
            ("ContextWindowExceededError", ContextWindowExceededError),
        )

        for litellm_name, mapped_exc in mappings:
            exc_type = _get_litellm_exception_type(litellm_name)
            if exc_type is not None and isinstance(e, exc_type):
                return mapped_exc(msg, e)

    return UnknownLiteLLMError(f"Unexpected LiteLLM error: {msg}", e)
