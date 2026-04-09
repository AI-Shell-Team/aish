"""Shell package.

Keep top-level exports lazy so importing subpackages like ``aish.shell.commands``
does not eagerly import the full shell runtime.
"""

from importlib import import_module

_EXPORT_MAP = {
    "run_shell": ("aish.shell.entry", "run_shell"),
    "execute_command_with_pty": ("aish.shell.pty.executor", "execute_command_with_pty"),
    "AIHandler": ("aish.shell.runtime.ai", "AIHandler"),
    "PTYAIShell": ("aish.shell.runtime.app", "PTYAIShell"),
    "LLMEventRouter": ("aish.shell.runtime.events", "LLMEventRouter"),
    "OutputProcessor": ("aish.shell.runtime.output", "OutputProcessor"),
    "ActionContext": ("aish.shell.types", "ActionContext"),
    "ActionOutcome": ("aish.shell.types", "ActionOutcome"),
    "CommandResult": ("aish.shell.types", "CommandResult"),
    "CommandStatus": ("aish.shell.types", "CommandStatus"),
    "InputIntent": ("aish.shell.types", "InputIntent"),
    "PTYUserInteraction": ("aish.shell.ui.interaction", "PTYUserInteraction"),
}


def __getattr__(name: str):
    if name not in _EXPORT_MAP:
        raise AttributeError(f"module 'aish.shell' has no attribute {name!r}")
    module_name, attr_name = _EXPORT_MAP[name]
    module = import_module(module_name)
    return getattr(module, attr_name)


def __dir__() -> list[str]:
    return sorted(list(globals().keys()) + list(_EXPORT_MAP.keys()))

__all__ = [
    "AIHandler",
    "ActionContext",
    "ActionOutcome",
    "CommandResult",
    "CommandStatus",
    "InputIntent",
    "LLMEventRouter",
    "OutputProcessor",
    "PTYUserInteraction",
    "PTYAIShell",
    "execute_command_with_pty",
    "run_shell",
]