"""Script system for aish shell.

This module provides a fish-shell-like script system that allows users to:
- Define custom commands as .aish script files
- Execute scripts directly by name
- Capture and apply shell state changes (cwd, env)
- Define hook scripts for customization (aish_prompt, etc.)
"""

from .executor import ScriptExecutionResult, ScriptExecutor
from .hooks import HookManager
from .hotreload import ScriptHotReloadService
from .loader import ScriptLoader
from .models import Script, ScriptArgument, ScriptMetadata
from .registry import ScriptRegistry

__all__ = [
    "Script",
    "ScriptArgument",
    "ScriptMetadata",
    "ScriptLoader",
    "ScriptRegistry",
    "ScriptExecutor",
    "ScriptExecutionResult",
    "HookManager",
    "ScriptHotReloadService",
]
