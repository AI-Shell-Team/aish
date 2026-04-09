"""LLM provider adapters namespace."""

from .registry import get_provider_by_id, get_provider_for_model

__all__ = ["get_provider_by_id", "get_provider_for_model"]