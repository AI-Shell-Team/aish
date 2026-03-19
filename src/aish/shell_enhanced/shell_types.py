"""Shared shell types and protocol definitions.

These abstractions are intentionally lightweight and internal-facing.
Public compatibility is preserved by re-exporting from aish.shell.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import Any, Protocol


class CommandStatus(Enum):
    """Command execution outcome."""

    SUCCESS = "success"
    CANCELLED = "cancelled"
    ERROR = "error"


@dataclass
class CommandResult:
    """Normalized command result payload."""

    status: CommandStatus
    exit_code: int
    stdout: str
    stderr: str
    offload: dict[str, Any] | None = None

    def to_tuple(self) -> tuple[int, str, str]:
        return (self.exit_code, self.stdout, self.stderr)


class InputIntent(Enum):
    """High-level intent classification for user input."""

    EMPTY = "empty"
    AI = "ai"
    HELP = "help"
    OPERATOR_COMMAND = "operator_command"
    SPECIAL_COMMAND = "special_command"
    BUILTIN_COMMAND = "builtin_command"
    SCRIPT_CALL = "script_call"  # User script invocation
    COMMAND_OR_AI = "command_or_ai"


@dataclass(slots=True)
class ActionContext:
    """Action execution context."""

    raw_input: str
    stripped_input: str
    route_data: dict[str, Any] = field(default_factory=dict)


@dataclass(slots=True)
class ActionOutcome:
    """Action execution outcome."""

    handled: bool


class ShellAction(Protocol):
    """Protocol for shell actions used by strategy-style handlers."""

    async def execute(self, ctx: ActionContext) -> ActionOutcome: ...


class CommandExecutor(Protocol):
    """Protocol for command execution services."""

    async def execute(self, command: str) -> CommandResult: ...


class PromptIO(Protocol):
    """Protocol for interactive prompt IO adapter."""

    async def get_user_input(self, prompt_text: str | None = None) -> str: ...


class EventHandler(Protocol):
    """Protocol for LLM event handlers."""

    def handle(self, event: Any) -> Any: ...
