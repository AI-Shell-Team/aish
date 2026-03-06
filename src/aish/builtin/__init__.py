"""Built-in command handlers for aish shell.

This module provides stateless command processing logic for shell built-in commands
like cd, pushd, popd, export, unset, etc.

The handlers are designed to be used by both:
- AIShell (for user commands)
- BashTool (for AI-generated commands)
"""

from aish.builtin.handlers import (BuiltinHandlers, BuiltinResult,
                                   DirectoryStack)
from aish.builtin.registry import (ALL_BUILTIN_COMMANDS, COMMAND_DESCRIPTIONS,
                                   PTY_REQUIRING_COMMANDS, REJECTED_COMMANDS,
                                   STATE_MODIFYING_COMMANDS, BuiltinRegistry)

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
