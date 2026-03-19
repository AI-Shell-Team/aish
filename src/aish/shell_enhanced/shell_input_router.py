"""Input intent router for AIShell.

This module classifies user input without performing side effects.
"""

from __future__ import annotations

import shlex
from dataclasses import dataclass, field
from typing import TYPE_CHECKING

from .shell_types import InputIntent

if TYPE_CHECKING:
    from ..shell import AIShell


@dataclass(slots=True)
class InputRoute:
    intent: InputIntent
    command_name: str | None = None
    help_command: str | None = None
    cmd_parts: list[str] = field(default_factory=list)
    parse_error: bool = False

    def as_dict(self) -> dict[str, object]:
        return {
            "command_name": self.command_name,
            "help_command": self.help_command,
            "cmd_parts": list(self.cmd_parts),
            "parse_error": self.parse_error,
        }


class ShellInputRouter:
    """Classify user input into stable intent buckets."""

    QUICK_BUILTIN_PREFIXES = ("cd", "pushd", "popd", "dirs", "export", "unset", "pwd")

    def __init__(self, shell: "AIShell") -> None:
        self.shell = shell

    def route(self, user_input: str) -> InputRoute:
        text = user_input.strip()
        if not text:
            return InputRoute(intent=InputIntent.EMPTY)

        if self.shell.starts_with_question_mark(text):
            return InputRoute(intent=InputIntent.AI)

        command, _remaining_input = self.shell.help_manager.parse_help_request(text)
        if command and self.shell.help_manager.has_help(command):
            return InputRoute(intent=InputIntent.HELP, help_command=command)

        if self.shell._has_shell_operators(text):
            return InputRoute(intent=InputIntent.OPERATOR_COMMAND)

        if text in {"exit", "quit", "help", "clear"}:
            return InputRoute(intent=InputIntent.SPECIAL_COMMAND, command_name=text)

        if text == "history" or text.startswith("history "):
            return InputRoute(
                intent=InputIntent.BUILTIN_COMMAND, command_name="history"
            )

        first = text.split()[0] if text.split() else ""
        if first and any(
            text == name or text.startswith(name + " ")
            for name in self.QUICK_BUILTIN_PREFIXES
        ):
            return InputRoute(intent=InputIntent.BUILTIN_COMMAND, command_name=first)

        # Check for script call before COMMAND_OR_AI
        if first and self._is_script_call(first):
            try:
                cmd_parts = shlex.split(text)
            except ValueError:
                cmd_parts = text.split()
            return InputRoute(
                intent=InputIntent.SCRIPT_CALL,
                command_name=first,
                cmd_parts=cmd_parts,
            )

        try:
            cmd_parts = shlex.split(text)
        except ValueError:
            return InputRoute(intent=InputIntent.COMMAND_OR_AI, parse_error=True)

        return InputRoute(intent=InputIntent.COMMAND_OR_AI, cmd_parts=cmd_parts)

    def _is_script_call(self, name: str) -> bool:
        """Check if the name matches a registered script.

        Args:
            name: First word of user input.

        Returns:
            True if it matches a script name.
        """
        # Check if script_registry exists and has the script
        if hasattr(self.shell, "script_registry") and self.shell.script_registry:
            return self.shell.script_registry.has_script(name)
        return False
