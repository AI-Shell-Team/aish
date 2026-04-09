"""Event-based command lifecycle tracking for the persistent bash PTY."""

from __future__ import annotations

import os
import re
import shlex
from dataclasses import dataclass

from .control_protocol import BackendControlEvent

_SESSION_COMMANDS = frozenset({
    "ftp",
    "mosh",
    "mosh-client",
    "nc",
    "netcat",
    "sftp",
    "ssh",
    "telnet",
})
_SUDO_SHELL_FLAGS = frozenset({"-i", "-s"})
_SHELL_LAUNCHERS = frozenset({"bash", "fish", "ksh", "sh", "su", "zsh"})
_SSH_OPTIONS_WITH_VALUE = frozenset(
    {
        "-b",
        "-c",
        "-D",
        "-E",
        "-e",
        "-F",
        "-I",
        "-i",
        "-J",
        "-L",
        "-l",
        "-m",
        "-O",
        "-o",
        "-p",
        "-Q",
        "-R",
        "-S",
        "-W",
        "-w",
    }
)


@dataclass(slots=True)
class CommandSubmission:
    """Metadata for a command submitted to the backend shell."""

    command: str
    source: str
    command_seq: int | None = None
    error_correction_dismissed: bool = False
    allow_error_correction: bool = False


@dataclass(slots=True)
class CommandResult:
    """Resolved completion state for a backend command."""

    command: str
    exit_code: int
    source: str
    command_seq: int | None = None
    interrupted: bool = False
    allow_error_correction: bool = False


class CommandState:
    """Track submitted commands and completed results using control events."""

    def __init__(self) -> None:
        self._active_submission: CommandSubmission | None = None
        self._submitted_by_seq: dict[int, CommandSubmission] = {}
        self._last_command: str = ""
        self._last_exit_code: int = 0
        self._last_result: CommandResult | None = None
        self._pending_error: CommandResult | None = None

    @property
    def last_command(self) -> str:
        return self._last_command

    @property
    def last_exit_code(self) -> int:
        return self._last_exit_code

    @property
    def last_result(self) -> CommandResult | None:
        return self._last_result

    @property
    def can_correct_last_error(self) -> bool:
        result = self._last_result
        return bool(
            result is not None
            and result.allow_error_correction
            and result.exit_code != 0
            and not result.interrupted
        )

    def register_user_command(self, command: str) -> None:
        self.register_command(command, source="user", command_seq=None)

    def register_backend_command(
        self, command: str, command_seq: int | None = None
    ) -> None:
        self.register_command(command, source="backend", command_seq=command_seq)

    def register_command(
        self,
        command: str,
        source: str,
        command_seq: int | None = None,
    ) -> None:
        command = command.strip()
        if not command:
            return
        self._store_submission(
            command=command,
            source=source,
            command_seq=command_seq,
        )

    def clear_error_correction(self) -> None:
        self._pending_error = None

    def consume_error(self) -> tuple[str, int] | None:
        if self._pending_error is None:
            return None
        result = self._pending_error
        self._pending_error = None
        return result.command, result.exit_code

    def handle_backend_event(
        self, event: BackendControlEvent
    ) -> CommandResult | None:
        if event.type == "command_started":
            command = str(event.payload.get("command") or "").strip()
            command_seq = self._coerce_command_seq(event.payload.get("command_seq"))
            submission = self._resolve_submission(
                command_seq=command_seq,
                command=command,
                create_if_missing=True,
            )
            if submission is not None and command and (
                not submission.command or submission.source != "user"
            ):
                submission.command = command
            return None

        if event.type != "prompt_ready":
            return None

        command_seq = self._coerce_command_seq(event.payload.get("command_seq"))
        submission = self._resolve_submission(
            command_seq=command_seq,
            command=None,
            create_if_missing=False,
        )
        if submission is None or not submission.command:
            return None

        exit_code = self._coerce_exit_code(event.payload.get("exit_code"))
        interrupted = bool(event.payload.get("interrupted")) or exit_code == 130
        result = CommandResult(
            command=submission.command,
            exit_code=exit_code,
            source=submission.source,
            command_seq=command_seq,
            interrupted=interrupted,
            allow_error_correction=submission.allow_error_correction,
        )

        self._last_command = result.command
        self._last_exit_code = result.exit_code
        self._last_result = result

        if result.allow_error_correction and result.exit_code != 0 and not result.interrupted:
            self._pending_error = result
        else:
            self._pending_error = None

        if command_seq is not None:
            self._submitted_by_seq.pop(command_seq, None)
        if self._active_submission is submission:
            self._active_submission = None
        return result

    def reset(self) -> None:
        self._active_submission = None
        self._submitted_by_seq.clear()
        self._last_command = ""
        self._last_exit_code = 0
        self._last_result = None
        self._pending_error = None

    def _store_submission(
        self,
        *,
        command: str,
        source: str,
        command_seq: int | None,
    ) -> CommandSubmission:
        submission = CommandSubmission(
            command=command,
            source=source,
            command_seq=command_seq,
            allow_error_correction=self._should_offer_error_correction(
                command=command,
                source=source,
            ),
        )
        self._active_submission = submission
        if command_seq is not None:
            self._submitted_by_seq[command_seq] = submission
        return submission

    @classmethod
    def _should_offer_error_correction(cls, *, command: str, source: str) -> bool:
        if source != "user":
            return False

        command = str(command or "").strip()
        if not command:
            return False

        return not cls._is_interactive_session_command(command)

    @classmethod
    def _is_interactive_session_command(cls, command: str) -> bool:
        words = cls._extract_command_words(command)
        if not words:
            return False

        executable = os.path.basename(words[0]).lower()
        if executable == "ssh":
            return cls._is_interactive_ssh_invocation(words)

        if executable in _SESSION_COMMANDS:
            return True

        if executable == "su":
            return True

        if executable != "sudo":
            return False

        remaining = words[1:]
        if not remaining:
            return False

        for token in remaining:
            if token == "--":
                continue
            if token in _SUDO_SHELL_FLAGS:
                return True
            if token.startswith("-"):
                continue
            lowered = os.path.basename(token).lower()
            return lowered in _SHELL_LAUNCHERS

        return False

    @classmethod
    def _extract_command_words(cls, command: str) -> list[str]:
        parts = cls._split_compound_command(command)
        if not parts:
            return []

        try:
            tokens = shlex.split(parts[-1])
        except ValueError:
            tokens = parts[-1].split()

        words: list[str] = []
        for token in tokens:
            if not words and cls._is_env_assignment(token):
                continue
            words.append(token)

        return words

    @staticmethod
    def _split_compound_command(command: str) -> list[str]:
        parts = re.split(r"\s*(?:\|\||&&|[;|&])\s*", command)
        return [part.strip() for part in parts if part.strip()]

    @staticmethod
    def _is_env_assignment(token: str) -> bool:
        if not token or token.startswith("="):
            return False
        name, _, value = token.partition("=")
        return bool(name) and bool(value or token.endswith("=")) and name.replace("_", "a").isalnum() and not name[0].isdigit()

    @classmethod
    def _is_interactive_ssh_invocation(cls, words: list[str]) -> bool:
        index = 1
        while index < len(words):
            token = words[index]
            if token == "--":
                index += 1
                break
            if not token.startswith("-") or token == "-":
                break
            if cls._ssh_option_takes_value(token):
                index += 2
            else:
                index += 1

        remaining = words[index:]
        return len(remaining) == 1

    @staticmethod
    def _ssh_option_takes_value(token: str) -> bool:
        if token in _SSH_OPTIONS_WITH_VALUE:
            return True
        for option in _SSH_OPTIONS_WITH_VALUE:
            if token.startswith(option) and token != option:
                return True
        return False

    def _resolve_submission(
        self,
        *,
        command_seq: int | None,
        command: str | None,
        create_if_missing: bool,
    ) -> CommandSubmission | None:
        if command_seq is not None:
            submission = self._submitted_by_seq.get(command_seq)
            if submission is not None:
                self._active_submission = submission
                return submission

        if self._active_submission is not None:
            active_seq = self._active_submission.command_seq
            if command_seq is None or active_seq is None or active_seq == command_seq:
                return self._active_submission

        if not create_if_missing:
            return None

        source = "backend" if command_seq is not None else "user"
        return self._store_submission(
            command=(command or self._last_command).strip(),
            source=source,
            command_seq=command_seq,
        )

    @staticmethod
    def _coerce_command_seq(value: object) -> int | None:
        if value is None or value == "":
            return None
        if isinstance(value, bool):
            return None
        if isinstance(value, int):
            return value
        if not isinstance(value, (str, bytes, bytearray)):
            return None
        try:
            return int(value)
        except (TypeError, ValueError):
            return None

    @staticmethod
    def _coerce_exit_code(value: object) -> int:
        if isinstance(value, int):
            return value
        if not isinstance(value, (str, bytes, bytearray)):
            return 0
        try:
            return int(value)
        except (TypeError, ValueError):
            return 0