"""LLM package entrypoint.

This package is the long-term home for model session, providers, and prompt
integration. The implementation currently re-exports from compatibility
modules while the wider tree is migrated.
"""

from .session import *
from .providers.registry import get_provider_by_id, get_provider_for_model