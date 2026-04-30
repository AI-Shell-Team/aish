"""prompt_toolkit completers for the shell editing UI."""

from __future__ import annotations

import os
import shlex
from contextlib import contextmanager
from typing import TYPE_CHECKING, Callable, Iterable, Iterator, Optional

from prompt_toolkit.completion import (
    CompleteEvent,
    Completer,
    Completion,
    PathCompleter,
)

from ..commands.registry import (
    PTY_REQUIRING_COMMANDS,
    REJECTED_COMMANDS,
    STATE_MODIFYING_COMMANDS,
)

if TYPE_CHECKING:
    from ...terminal.pty import PTYManager

SPECIAL_SHELL_COMMANDS = frozenset({"/model", "/setup", "/plan"})
DIRECTORY_COMMANDS = frozenset({"cd", "pushd", "popd"})
AI_PREFIXES = (";", "；")


def _iter_executables_in_path(path_value: str) -> Iterator[str]:
    seen: set[str] = set()
    for raw_dir in path_value.split(os.pathsep):
        directory = raw_dir.strip()
        if not directory:
            continue

        try:
            entries = os.scandir(directory)
        except OSError:
            continue

        with entries:
            for entry in entries:
                name = entry.name
                if not name or name in seen:
                    continue
                try:
                    if not entry.is_file():
                        continue
                    if not os.access(entry.path, os.X_OK):
                        continue
                except OSError:
                    continue

                seen.add(name)
                yield name


def _looks_like_path_token(token: str) -> bool:
    token = str(token or "")
    if not token:
        return False

    return token.startswith(("./", "../", "/", "~/")) or "/" in token


class ShellCompleter(Completer):
    """Provide tab completion by delegating to PTY bash.

    Falls back to PATH scanning + PathCompleter when PTY is unavailable.
    """

    def __init__(
        self,
        cwd_provider: Optional[Callable[[], str]] = None,
        command_provider: Optional[Callable[[], Iterable[str]]] = None,
        ai_prefixes: tuple[str, ...] = AI_PREFIXES,
        pty_provider: Optional[Callable[[], Optional["PTYManager"]]] = None,
    ) -> None:
        self._cwd_provider = cwd_provider
        self._command_provider = command_provider or self._default_command_provider
        self._ai_prefixes = ai_prefixes
        self._pty_provider = pty_provider
        self._path_completer = PathCompleter(expanduser=True)
        self._dir_completer = PathCompleter(only_directories=True, expanduser=True)
        self._cached_path_value: Optional[str] = None
        self._cached_commands: tuple[str, ...] = ()

    def get_completions(self, document, complete_event: CompleteEvent):
        text_before_cursor = document.text_before_cursor
        if not text_before_cursor.strip():
            return

        stripped = text_before_cursor.lstrip()
        if stripped.startswith(self._ai_prefixes):
            return

        tokens, current_token, trailing_space = self._split_tokens(
            text_before_cursor
        )
        if not tokens and not current_token:
            return

        # Special aish commands always handled in Python
        if (not tokens and current_token.startswith("/")) or (
            tokens and tokens[0] in SPECIAL_SHELL_COMMANDS
        ):
            yield from self._complete_special_commands(current_token)
            return

        # Attempt PTY bash completion
        cursor_pos = len(text_before_cursor)
        pty_results = self._query_pty_completions(text_before_cursor, cursor_pos)

        if pty_results is not None:
            prefix = current_token
            for candidate in pty_results:
                if prefix and not candidate.startswith(prefix):
                    continue
                yield Completion(
                    candidate,
                    start_position=-len(prefix),
                    display=candidate,
                )
            return

        # Fallback: PATH scanning + PathCompleter
        yield from self._fallback_completions(
            tokens, current_token, trailing_space, complete_event
        )

    def _query_pty_completions(
        self, line: str, cursor_pos: int
    ) -> Optional[list[str]]:
        """Query the PTY bash for completions. Returns None on failure."""
        pty_manager = self._pty_provider() if self._pty_provider else None
        if pty_manager is None or not pty_manager.is_running:
            return None

        escaped_line = shlex.quote(line)
        cmd = f"__aish_query_completions {escaped_line} {cursor_pos}"

        try:
            output, exit_code = pty_manager.execute_command(cmd, timeout=0.5)
        except Exception:
            return None

        if exit_code != 0 or not output:
            return None

        candidates = [c for c in output.splitlines() if c]
        return candidates or None

    def _fallback_completions(
        self,
        tokens: list[str],
        current_token: str,
        trailing_space: bool,
        complete_event: CompleteEvent,
    ) -> Iterator[Completion]:
        """Fallback completion using PATH scanning and PathCompleter."""
        if not tokens:
            if _looks_like_path_token(current_token):
                yield from self._complete_paths(
                    current_token,
                    directory_only=False,
                    complete_event=complete_event,
                )
                return
            yield from self._complete_commands(current_token)
            return

        command = tokens[0]
        if current_token.startswith("-"):
            return

        if trailing_space or current_token:
            if command in DIRECTORY_COMMANDS:
                yield from self._complete_paths(
                    current_token,
                    directory_only=True,
                    complete_event=complete_event,
                )
                return

            yield from self._complete_paths(
                current_token,
                directory_only=False,
                complete_event=complete_event,
            )

    def _complete_commands(self, prefix: str) -> Iterator[Completion]:
        prefix = str(prefix or "")
        for candidate in self._command_provider():
            if prefix and not candidate.startswith(prefix):
                continue
            yield Completion(candidate, start_position=-len(prefix), display=candidate)

    def _complete_special_commands(self, prefix: str) -> Iterator[Completion]:
        prefix = str(prefix or "")
        for command in sorted(SPECIAL_SHELL_COMMANDS):
            if prefix and not command.startswith(prefix):
                continue
            yield Completion(command, start_position=-len(prefix), display=command)

    def _complete_paths(
        self,
        prefix: str,
        *,
        directory_only: bool,
        complete_event: CompleteEvent,
    ) -> Iterator[Completion]:
        completer = self._dir_completer if directory_only else self._path_completer
        with self._use_completion_cwd():
            from prompt_toolkit.document import Document

            path_document = Document(text=prefix, cursor_position=len(prefix))
            yield from completer.get_completions(path_document, complete_event)

    def _default_command_provider(self) -> Iterable[str]:
        path_value = os.environ.get("PATH", "")
        if path_value != self._cached_path_value:
            commands = set()
            commands.update(STATE_MODIFYING_COMMANDS)
            commands.update(PTY_REQUIRING_COMMANDS)
            commands.update(REJECTED_COMMANDS)
            commands.update(_iter_executables_in_path(path_value))
            self._cached_path_value = path_value
            self._cached_commands = tuple(sorted(commands))
        return self._cached_commands

    def _resolve_cwd(self) -> Optional[str]:
        if self._cwd_provider is None:
            return None
        try:
            cwd = self._cwd_provider()
        except Exception:
            return None
        if not isinstance(cwd, str) or not cwd:
            return None
        return cwd

    @contextmanager
    def _use_completion_cwd(self) -> Iterator[None]:
        target_cwd = self._resolve_cwd()
        if not target_cwd:
            yield
            return

        try:
            original_cwd = os.getcwd()
        except OSError:
            original_cwd = None

        changed = False
        try:
            if original_cwd != target_cwd:
                os.chdir(target_cwd)
                changed = True
            yield
        except OSError:
            yield
        finally:
            if changed and original_cwd:
                try:
                    os.chdir(original_cwd)
                except OSError:
                    pass

    @staticmethod
    def _split_tokens(text: str) -> tuple[list[str], str, bool]:
        trailing_space = bool(text) and text[-1].isspace()
        parts = text.split()
        if trailing_space:
            return parts, "", True
        if not parts:
            return [], "", False
        return parts[:-1], parts[-1], False
