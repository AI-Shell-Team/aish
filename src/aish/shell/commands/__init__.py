"""Shell command handlers and registry.

This package is the canonical home for shell built-in command handling.
The legacy ``aish.builtin`` package remains as a compatibility shim.
"""

from .handlers import BuiltinHandlers, BuiltinResult, DirectoryStack
from .registry import (
    ALL_BUILTIN_COMMANDS,
    COMMAND_DESCRIPTIONS,
    PTY_REQUIRING_COMMANDS,
    REJECTED_COMMANDS,
    STATE_MODIFYING_COMMANDS,
    BuiltinRegistry,
)

__all__ = [
    "BuiltinHandlers",
    "BuiltinResult",
    "DirectoryStack",
    "BuiltinRegistry",
    "STATE_MODIFYING_COMMANDS",
    "PTY_REQUIRING_COMMANDS",
    "REJECTED_COMMANDS",
    "ALL_BUILTIN_COMMANDS",
    "COMMAND_DESCRIPTIONS",
]