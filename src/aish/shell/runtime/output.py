"""PTY output processing for the shell runtime."""

from __future__ import annotations

import re
import sys
from typing import TYPE_CHECKING, Optional

from ...i18n import t
from ..commands import SHELL_EXIT_COMMANDS
from ...terminal.pty.command_state import CommandResult
from ...terminal.pty.control_protocol import BackendControlEvent

if TYPE_CHECKING:
    from ...terminal.pty import PTYManager
    from .app import PTYAIShell


_ANSI_CSI_RE = re.compile(rb"\x1b\[[0-9;?]*[ -/]*[@-~]")
_ANSI_OSC_RE = re.compile(rb"\x1b\].*?(?:\x07|\x1b\\)")


class OutputProcessor:
    """Process PTY output. detect errors. show hints."""

    def __init__(
        self,
        pty_manager: "PTYManager",
        shell: Optional["PTYAIShell"] = None,
    ):
        self.pty_manager = pty_manager
        self._filter_exit_echo = False
        self.shell = shell
        self._current_command: str = ""
        self._pending_user_echo: bytes | None = None
        self._pending_user_echo_buffer = bytearray()
        # Two-layer error suppression:
        # Layer 1 (here): _suppress_error_hint — UI-layer skip for one cycle
        #   (e.g., after Ctrl+C for exit, suppress the spurious hint).
        # Layer 2 (CommandState): explicit control events distinguish
        #   user-typed vs backend commands and only expose failures once.
        self._suppress_error_hint: bool = False

    def suppress_next_error_hint(self) -> None:
        """Suppress the next error correction hint (e.g., after Ctrl+C for exit)."""
        self._suppress_error_hint = True

    def prepare_user_command_echo(self, command: str, command_seq: int | None) -> None:
        """Suppress the first bash echo for a user-submitted command."""
        command = str(command or "").strip()
        if not command or command_seq is None:
            self._clear_pending_user_echo()
            return
        self._pending_user_echo = command.encode("utf-8")
        self._pending_user_echo_buffer.clear()

    def _clear_pending_user_echo(self) -> None:
        self._pending_user_echo = None
        self._pending_user_echo_buffer.clear()

    @staticmethod
    def _strip_terminal_control(data: bytes) -> bytes:
        data = _ANSI_CSI_RE.sub(b"", data)
        data = _ANSI_OSC_RE.sub(b"", data)
        return data

    def _line_matches_pending_user_echo(self, line: bytes) -> bool:
        if self._pending_user_echo is None:
            return False

        normalized = self._strip_terminal_control(line).strip(b"\r\n")
        normalized = normalized.lstrip(b"\r")
        return normalized == self._pending_user_echo

    def _buffer_might_be_pending_user_echo(self, buffer: bytes) -> bool:
        if self._pending_user_echo is None:
            return False

        normalized = self._strip_terminal_control(buffer)
        normalized = normalized.lstrip(b"\r")
        return self._pending_user_echo.startswith(normalized)

    def _consume_pending_user_echo(self, data: bytes) -> bytes:
        if self._pending_user_echo is None:
            return data

        self._pending_user_echo_buffer.extend(data)
        rendered = bytearray()

        while self._pending_user_echo_buffer:
            buffered = bytes(self._pending_user_echo_buffer)
            newline_index = buffered.find(b"\n")

            if newline_index == -1:
                if self._buffer_might_be_pending_user_echo(buffered):
                    break
                rendered.extend(self._pending_user_echo_buffer)
                self._pending_user_echo_buffer.clear()
                break

            line_end = newline_index + 1
            line = buffered[:line_end]
            del self._pending_user_echo_buffer[:line_end]

            if self._line_matches_pending_user_echo(line):
                remainder = bytes(self._pending_user_echo_buffer)
                self._clear_pending_user_echo()
                rendered.extend(remainder)
                break

            rendered.extend(line)

        return bytes(rendered)

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

        self._clear_pending_user_echo()
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
