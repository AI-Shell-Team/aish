"""PTY output processing for the shell runtime."""

from __future__ import annotations

import shlex
import sys
from typing import TYPE_CHECKING, Optional

from ...i18n import t
from ..commands import SHELL_EXIT_COMMANDS
from ...terminal.pty.command_state import CommandResult
from ...terminal.pty.control_protocol import BackendControlEvent

if TYPE_CHECKING:
    from ...terminal.pty import PTYManager
    from .app import PTYAIShell


class OutputProcessor:
    """Process PTY output. detect errors. show hints."""

    def __init__(
        self,
        pty_manager: "PTYManager",
        shell: Optional["PTYAIShell"] = None,
    ):
        self.pty_manager = pty_manager
        self._waiting_for_result = False
        self._filter_exit_echo = False
        self.shell = shell
        self._current_command: str = ""
        self._pending_user_echo: bytes | None = None
        # Two-layer error suppression:
        # Layer 1 (here): _suppress_error_hint — UI-layer skip for one cycle
        #   (e.g., after Ctrl+C for exit, suppress the spurious hint).
        # Layer 2 (CommandState): explicit control events distinguish
        #   user-typed vs backend commands and only expose failures once.
        self._suppress_error_hint: bool = False

    def set_waiting_for_result(self, waiting: bool, command: str = "") -> None:
        """Set whether we're waiting for a command result."""
        self._waiting_for_result = waiting
        self._suppress_error_hint = False
        if waiting:
            self._current_command = command

    def suppress_next_error_hint(self) -> None:
        """Suppress the next error correction hint (e.g., after Ctrl+C for exit)."""
        self._suppress_error_hint = True

    def set_current_command(self, command: str) -> None:
        """Set the current command being executed."""
        self._current_command = command

    def prepare_user_command_echo(self, command: str, command_seq: int | None) -> None:
        """Suppress the first bash echo for a user-submitted command."""
        command = str(command or "").strip()
        if not command or command_seq is None:
            self._pending_user_echo = None
            return
        quoted_command = shlex.quote(command)
        self._pending_user_echo = (
            (
                f" __AISH_ACTIVE_COMMAND_SEQ={command_seq}; "
                f"__AISH_ACTIVE_COMMAND_TEXT={quoted_command}; {command}"
            ).encode("utf-8")
        )

    def _consume_pending_user_echo(self, data: bytes) -> bytes:
        if self._pending_user_echo is None:
            return data

        echoed = self._pending_user_echo
        patterns = (
            b"\r" + echoed + b"\r\n",
            echoed + b"\r\n",
            b"\r" + echoed + b"\n",
            echoed + b"\n",
            b"\r" + echoed,
            echoed,
        )

        for pattern in patterns:
            if data.startswith(pattern):
                self._pending_user_echo = None
                return data[len(pattern) :]

        index = data.find(echoed)
        if index == -1:
            return data

        start = index
        end = index + len(echoed)
        if start > 0 and data[start - 1 : start] == b"\r":
            start -= 1

        if data[end : end + 2] == b"\r\n":
            end += 2
        elif data[end : end + 1] in (b"\r", b"\n"):
            end += 1

        self._pending_user_echo = None
        return data[:start] + data[end:]

    def set_filter_exit_echo(self, filter_exit: bool) -> None:
        """Set whether to filter exit command echo."""
        self._filter_exit_echo = filter_exit

    def handle_backend_event(
        self,
        event: BackendControlEvent,
        result: CommandResult | None = None,
    ) -> None:
        """Update output state from explicit backend lifecycle events."""
        if event.type == "command_started":
            command = event.payload.get("command")
            if isinstance(command, str) and command.strip():
                self._current_command = command.strip()
            return

        if event.type != "prompt_ready":
            return

        self._waiting_for_result = False
        if result is None:
            return

        command = result.command or self._current_command
        if self.shell and command:
            self.shell.add_shell_history(
                command=command,
                returncode=result.exit_code,
                stdout="",
                stderr="",
                offload={"status": "inline", "source": "pty"},
            )

        error_info = self.pty_manager.consume_error()
        if error_info is not None:
            if self._suppress_error_hint:
                self._suppress_error_hint = False
            else:
                hint = t("shell.error_correction.press_semicolon_hint")
                sys.stdout.write(f"\033[2m\033[37m<{hint}>\033[0m\r\n")
                sys.stdout.flush()

        self._current_command = ""

    def process(self, data: bytes) -> bytes:
        """Process PTY output, return cleaned output."""
        data = self._consume_pending_user_echo(data)

        if self._filter_exit_echo:
            stripped = data.strip(b"\r\n")
            for exit_command in SHELL_EXIT_COMMANDS:
                exit_bytes = exit_command.encode("utf-8")
                if stripped == exit_bytes:
                    self._filter_exit_echo = False
                    return b""
                for pattern in (
                    b"\r" + exit_bytes + b"\r\n",
                    b"\n" + exit_bytes + b"\r\n",
                    b"\r" + exit_bytes + b"\n",
                ):
                    if data.endswith(pattern):
                        data = data[: -len(pattern)]
                        self._filter_exit_echo = False
                        break
                if not self._filter_exit_echo:
                    break

        return data
