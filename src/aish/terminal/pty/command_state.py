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

    submission_id: str
    command: str
    source: str
    command_seq: int | None = None
    observed_command: str = ""
    error_correction_dismissed: bool = False
    allow_error_correction: bool = False
    status: str = "submitted"


@dataclass(slots=True)
class CommandResult:
    """Resolved completion state for a backend command."""

    command: str
    exit_code: int
    source: str
    command_seq: int | None = None
    interrupted: bool = False
    allow_error_correction: bool = False
    submission_id: str | None = None


class CommandState:
    """Track submitted commands and completed results using control events."""

    def __init__(self) -> None:
        self._active_submission: CommandSubmission | None = None
        self._active_started_submission: CommandSubmission | None = None
        self._submitted_by_seq: dict[int, CommandSubmission] = {}
        self._submitted_by_id: dict[str, CommandSubmission] = {}
        self._last_command: str = ""
        self._last_exit_code: int = 0
        self._last_result: CommandResult | None = None
        self._pending_error: CommandResult | None = None
        self._protocol_issues: list[str] = []
        self._next_submission_id: int = 1

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
    def pending_submission(self) -> CommandSubmission | None:
        """Return the currently active pending submission, if any."""
        if self._active_started_submission is not None:
            return self._active_started_submission
        return self._active_submission

    @property
    def pending_submissions(self) -> tuple[CommandSubmission, ...]:
        """Return the known pending submissions without duplicates."""
        submissions: list[CommandSubmission] = []
        seen: set[int] = set()

        if self._active_submission is not None:
            submissions.append(self._active_submission)
            seen.add(id(self._active_submission))

        if self._active_started_submission is not None:
            started_id = id(self._active_started_submission)
            if started_id not in seen:
                submissions.append(self._active_started_submission)
                seen.add(started_id)

        for submission in self._submitted_by_seq.values():
            submission_id = id(submission)
            if submission_id in seen:
                continue
            submissions.append(submission)
            seen.add(submission_id)

        for submission in self._submitted_by_id.values():
            submission_obj_id = id(submission)
            if submission_obj_id in seen:
                continue
            submissions.append(submission)
            seen.add(submission_obj_id)

        return tuple(submissions)

    @property
    def pending_submission_count(self) -> int:
        """Return the number of tracked pending submissions."""
        return len(self.pending_submissions)

    @property
    def can_correct_last_error(self) -> bool:
        result = self._last_result
        return bool(
            result is not None
            and result.allow_error_correction
            and result.exit_code != 0
            and not result.interrupted
        )

    @property
    def protocol_issues(self) -> tuple[str, ...]:
        """Return collected protocol/state issues for diagnostics."""
        return tuple(self._protocol_issues)

    def register_user_command(self, command: str) -> CommandSubmission | None:
        return self.register_command(command, source="user", command_seq=None)

    def register_backend_command(
        self, command: str, command_seq: int | None = None
    ) -> CommandSubmission | None:
        return self.register_command(command, source="backend", command_seq=command_seq)

    def register_command(
        self,
        command: str,
        source: str,
        command_seq: int | None = None,
    ) -> CommandSubmission | None:
        command = command.strip()
        if not command:
            return None
        return self._store_submission(
            command=command,
            source=source,
            command_seq=command_seq,
        )

    def get_pending_submission(
        self, command_seq: int | None = None
    ) -> CommandSubmission | None:
        """Return a tracked pending submission by sequence or active state."""
        if command_seq is None:
            return self.pending_submission
        return self._submitted_by_seq.get(command_seq)

    def has_pending_submission(self, command_seq: int | None = None) -> bool:
        """Return whether a pending submission is currently tracked."""
        return self.get_pending_submission(command_seq) is not None

    def resolve_command_seq(
        self,
        command_seq: object,
        submission_id: object | None = None,
    ) -> int | None:
        """Resolve an event command sequence using pending submission state."""
        resolved_seq = self._coerce_command_seq(command_seq)
        resolved_submission_id = self._coerce_submission_id(submission_id)

        if resolved_seq is not None and resolved_submission_id is not None:
            seq_submission = self._submitted_by_seq.get(resolved_seq)
            id_submission = self._submitted_by_id.get(resolved_submission_id)
            if (
                seq_submission is not None
                and id_submission is not None
                and seq_submission is not id_submission
            ):
                self._record_protocol_issue(
                    "conflicting backend identifiers for "
                    f"submission_id={resolved_submission_id} and command_seq={resolved_seq}"
                )
                return None

        if resolved_seq is not None:
            return resolved_seq

        if resolved_submission_id is not None:
            submission = self._submitted_by_id.get(resolved_submission_id)
            if submission is not None:
                return submission.command_seq
            return None

        if (
            self._active_started_submission is not None
            and self._active_submission in (None, self._active_started_submission)
        ):
            return self._active_started_submission.command_seq

        unique_submission = self._unique_pending_submission()
        if unique_submission is None:
            return None
        return unique_submission.command_seq

    def next_submission_id(self) -> str:
        submission_id = f"subm_{self._next_submission_id}"
        self._next_submission_id += 1
        return submission_id

    def clear_error_correction(self) -> None:
        self._pending_error = None

    def consume_error(self) -> tuple[str, int] | None:
        if self._pending_error is None:
            return None
        result = self._pending_error
        self._pending_error = None
        return result.command, result.exit_code

    def consume_protocol_issues(self) -> tuple[str, ...]:
        """Consume and clear collected protocol/state issues."""
        issues = tuple(self._protocol_issues)
        self._protocol_issues.clear()
        return issues

    def handle_backend_event(
        self, event: BackendControlEvent
    ) -> CommandResult | None:
        if event.type == "command_started":
            command = str(event.payload.get("command") or "").strip()
            submission_id = self._coerce_submission_id(event.payload.get("submission_id"))
            raw_command_seq = self._coerce_command_seq(event.payload.get("command_seq"))
            command_seq = self._coerce_command_seq(event.payload.get("command_seq"))

            if (
                raw_command_seq is not None
                and raw_command_seq not in self._submitted_by_seq
                and self._active_submission is None
            ):
                self._record_protocol_issue(
                    f"command_started missing matching submission for command_seq={raw_command_seq}"
                )

            submission = self._resolve_submission(
                submission_id=submission_id,
                command_seq=command_seq,
                command=command,
                create_if_missing=True,
            )
            if submission is not None:
                if not self._bind_submission_seq(submission, command_seq):
                    return None
                if command:
                    submission.observed_command = command
                if not submission.command and command:
                    submission.command = command
                submission.status = "started"
                self._active_started_submission = submission
            return None

        if event.type != "prompt_ready":
            return None

        submission_id = self._coerce_submission_id(event.payload.get("submission_id"))
        raw_command_seq = self._coerce_command_seq(event.payload.get("command_seq"))
        command_seq = self._coerce_command_seq(event.payload.get("command_seq"))
        submission = self._resolve_submission(
            submission_id=submission_id,
            command_seq=command_seq,
            command=None,
            create_if_missing=False,
        )
        if submission is None or not submission.command:
            if submission_id is not None and raw_command_seq is not None:
                self._record_protocol_issue(
                    "conflicting backend identifiers for "
                    f"submission_id={submission_id} and command_seq={raw_command_seq}"
                )
            elif raw_command_seq is not None:
                self._record_protocol_issue(
                    f"prompt_ready missing matching submission for command_seq={raw_command_seq}"
                )
            else:
                self._record_protocol_issue(
                    "prompt_ready received without matching active submission"
                )
            return None

        if not self._bind_submission_seq(submission, command_seq):
            return None
        resolved_command_seq = submission.command_seq if submission.command_seq is not None else command_seq

        exit_code = self._coerce_exit_code(event.payload.get("exit_code"))
        interrupted = bool(event.payload.get("interrupted")) or exit_code == 130
        result = CommandResult(
            command=submission.command,
            exit_code=exit_code,
            source=submission.source,
            command_seq=resolved_command_seq,
            interrupted=interrupted,
            allow_error_correction=submission.allow_error_correction,
            submission_id=submission.submission_id,
        )

        self._last_command = result.command
        self._last_exit_code = result.exit_code
        self._last_result = result

        if result.allow_error_correction and result.exit_code != 0 and not result.interrupted:
            self._pending_error = result
        else:
            self._pending_error = None

        submission.status = "completed"

        if resolved_command_seq is not None:
            self._submitted_by_seq.pop(resolved_command_seq, None)
        self._submitted_by_id.pop(submission.submission_id, None)
        if self._active_submission is submission:
            self._active_submission = None
        if self._active_started_submission is submission:
            self._active_started_submission = None
        return result

    def reset(self) -> None:
        self._active_submission = None
        self._active_started_submission = None
        self._submitted_by_seq.clear()
        self._submitted_by_id.clear()
        self._last_command = ""
        self._last_exit_code = 0
        self._last_result = None
        self._pending_error = None
        self._protocol_issues.clear()
        self._next_submission_id = 1

    def _record_protocol_issue(self, issue: str) -> None:
        self._protocol_issues.append(issue)
        self._protocol_issues = self._protocol_issues[-50:]

    def _store_submission(
        self,
        *,
        command: str,
        source: str,
        command_seq: int | None,
        submission_id: str | None = None,
    ) -> CommandSubmission:
        submission = CommandSubmission(
            submission_id=submission_id or self.next_submission_id(),
            command=command,
            source=source,
            command_seq=command_seq,
            allow_error_correction=self._should_offer_error_correction(
                command=command,
                source=source,
            ),
        )
        self._active_submission = submission
        self._submitted_by_id[submission.submission_id] = submission
        if command_seq is not None:
            self._submitted_by_seq[command_seq] = submission
        return submission

    def _bind_submission_seq(
        self,
        submission: CommandSubmission,
        command_seq: int | None,
    ) -> bool:
        if command_seq is None:
            return True

        existing_seq = submission.command_seq
        if existing_seq == command_seq:
            self._submitted_by_seq[command_seq] = submission
            return True

        mapped_submission = self._submitted_by_seq.get(command_seq)
        if mapped_submission is not None and mapped_submission is not submission:
            self._record_protocol_issue(
                "conflicting backend identifiers for "
                f"submission_id={submission.submission_id} and command_seq={command_seq}"
            )
            return False

        if existing_seq is not None:
            self._record_protocol_issue(
                "refusing to rebind submission_id="
                f"{submission.submission_id} from command_seq={existing_seq} to {command_seq}"
            )
            return False

        submission.command_seq = command_seq
        self._submitted_by_seq[command_seq] = submission
        return True

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
        submission_id: str | None,
        command_seq: int | None,
        command: str | None,
        create_if_missing: bool,
    ) -> CommandSubmission | None:
        id_submission = None
        if submission_id is not None:
            id_submission = self._submitted_by_id.get(submission_id)

        seq_submission = None
        if command_seq is not None:
            seq_submission = self._submitted_by_seq.get(command_seq)

        if (
            id_submission is not None
            and seq_submission is not None
            and id_submission is not seq_submission
        ):
            self._record_protocol_issue(
                "conflicting backend identifiers for "
                f"submission_id={submission_id} and command_seq={command_seq}"
            )
            return None

        if id_submission is not None:
            self._active_submission = id_submission
            return id_submission

        if seq_submission is not None:
            self._active_submission = seq_submission
            return seq_submission

        if self._active_started_submission is not None:
            if self._submission_matches(
                self._active_started_submission,
                submission_id=submission_id,
                command_seq=command_seq,
            ):
                return self._active_started_submission

        if self._active_submission is not None:
            if self._submission_matches(
                self._active_submission,
                submission_id=submission_id,
                command_seq=command_seq,
            ):
                return self._active_submission

        if submission_id is None and command_seq is None:
            unique_submission = self._unique_pending_submission()
            if unique_submission is not None:
                return unique_submission

        if not create_if_missing:
            return None

        if submission_id is not None and command_seq is None:
            self._record_protocol_issue(
                f"backend event missing matching submission for submission_id={submission_id}"
            )

        source = "backend" if (submission_id is not None or command_seq is not None) else "user"
        return self._store_submission(
            command=(command or self._last_command).strip(),
            source=source,
            command_seq=command_seq,
            submission_id=submission_id,
        )

    def _unique_pending_submission(self) -> CommandSubmission | None:
        pending = self.pending_submissions
        if len(pending) == 1:
            return pending[0]
        return None

    @staticmethod
    def _submission_matches(
        submission: CommandSubmission,
        *,
        submission_id: str | None,
        command_seq: int | None,
    ) -> bool:
        if submission_id is not None:
            return submission.submission_id == submission_id

        if command_seq is not None:
            submission_seq = submission.command_seq
            return submission_seq is None or submission_seq == command_seq

        return False

    @staticmethod
    def _coerce_submission_id(value: object) -> str | None:
        if value is None:
            return None
        if isinstance(value, bytes):
            try:
                value = value.decode("utf-8")
            except UnicodeDecodeError:
                return None
        if not isinstance(value, str):
            return None

        submission_id = value.strip()
        if not submission_id or submission_id == "null":
            return None
        return submission_id

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