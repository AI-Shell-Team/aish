"""prompt_toolkit-backed editing session for the shell frontend."""

from __future__ import annotations

import importlib.resources
import os
import subprocess
import time
from html import escape
from typing import TYPE_CHECKING, Callable, Optional

from prompt_toolkit import PromptSession
from prompt_toolkit.auto_suggest import AutoSuggestFromHistory
from prompt_toolkit.formatted_text import ANSI, HTML
from prompt_toolkit.history import FileHistory
from prompt_toolkit.key_binding import KeyBindings, merge_key_bindings
from prompt_toolkit.key_binding.bindings.auto_suggest import (
    load_auto_suggest_bindings,
)
from prompt_toolkit.shortcuts import CompleteStyle

from .completion import ShellCompleter

if TYPE_CHECKING:
    from ...state import HistoryManager
    from ..interruption import InterruptionManager

# Cache TTL for theme rendering (seconds). Avoids re-running git commands
# on every prompt refresh when cwd and exit_code haven't changed.
_THEME_CACHE_TTL = 2.0


class ShellPromptController:
    """Own the editing-mode PromptSession and lightweight UI state."""

    def __init__(
        self,
        history_manager: Optional["HistoryManager"] = None,
        interruption_manager: Optional["InterruptionManager"] = None,
        on_buffer_change: Optional[Callable[[str], None]] = None,
        cwd_provider: Optional[Callable[[], str]] = None,
        completer: Optional[ShellCompleter] = None,
        prompt_theme: str = "",
        exit_code_provider: Optional[Callable[[], int]] = None,
    ):
        self._history_manager = history_manager
        self._interruption_manager = interruption_manager
        self._on_buffer_change = on_buffer_change
        self._cwd_provider = cwd_provider
        self._prompt_theme = prompt_theme
        self._exit_code_provider = exit_code_provider
        # Theme render cache: avoids repeated git subprocess calls
        self._theme_cache_key: tuple[str, int] = ("", 0)
        self._theme_cache_output: str = ""
        self._theme_cache_time: float = 0.0
        self._history = FileHistory(os.path.expanduser("~/.aish_history"))
        self._completer = completer or ShellCompleter(cwd_provider=cwd_provider)
        self._session = PromptSession(
            history=self._history,
            auto_suggest=AutoSuggestFromHistory(),
            completer=self._completer,
            complete_while_typing=False,
            complete_style=CompleteStyle.READLINE_LIKE,
            key_bindings=self._build_key_bindings(),
            mouse_support=False,
            reserve_space_for_menu=0,
        )
        if hasattr(self._session, "app") and hasattr(self._session.app, "output"):
            output = self._session.app.output
            if hasattr(output, "enable_cpr"):
                setattr(output, "enable_cpr", False)

    def prompt(self, prompt_message=None) -> str:
        """Read one editing-mode line from the terminal."""

        def _pre_run() -> None:
            app = self._session.app
            buffer = app.current_buffer
            self._notify_buffer_change(buffer.text)

            def _handle_change(_buffer) -> None:
                self._notify_buffer_change(buffer.text)

            buffer.on_text_changed += _handle_change

        return self._session.prompt(
            self._build_prompt_message() if prompt_message is None else prompt_message,
            pre_run=_pre_run,
        )

    def remember_command(self, command: str) -> None:
        """Append a submitted command to prompt-toolkit's local history."""
        command = str(command or "").strip()
        if not command:
            return
        self._history.append_string(command)

    def _build_key_bindings(self):
        bindings = KeyBindings()
        return merge_key_bindings([bindings, load_auto_suggest_bindings()])

    def _notify_buffer_change(self, text: str) -> None:
        if self._on_buffer_change is not None:
            self._on_buffer_change(text)

    def _get_prompt_text(self) -> str:
        cwd = None
        if self._cwd_provider is not None:
            try:
                cwd = self._cwd_provider()
            except Exception:
                cwd = None

        cwd_text = str(cwd or os.getcwd())
        home = os.path.expanduser("~")
        if cwd_text == home:
            return "~"
        if cwd_text.startswith(home + os.sep):
            return "~" + cwd_text[len(home) :]
        return cwd_text

    def _build_prompt_message(self) -> ANSI | HTML:
        if self._prompt_theme:
            theme_output = self._render_theme()
            if theme_output:
                return ANSI(theme_output)
        # Default prompt: blue path + cyan >
        prompt_text = escape(self._get_prompt_text())
        return HTML(f"<ansiblue>{prompt_text}</ansiblue> <ansicyan>&gt;</ansicyan> ")

    def _render_theme(self) -> str:
        """Execute theme script and return ANSI prompt string (cached)."""
        theme = self._prompt_theme
        if not theme or theme == "default":
            return ""

        # Validate theme name
        if not all(c.isalnum() or c in "_-" for c in theme):
            return ""

        cwd = os.getcwd()
        exit_code = 0
        if self._exit_code_provider:
            try:
                exit_code = self._exit_code_provider()
            except Exception:
                pass

        # Return cached result if cwd+exit_code unchanged and TTL not expired
        cache_key = (cwd, exit_code)
        now = time.monotonic()
        if cache_key == self._theme_cache_key and (now - self._theme_cache_time) < _THEME_CACHE_TTL:
            return self._theme_cache_output

        # Find theme script
        theme_script = self._find_theme_script(theme)
        if not theme_script:
            return ""

        env = self._build_theme_env(cwd, exit_code)

        try:
            result = subprocess.run(
                ["bash", "-c", 'source "$1"', "_", theme_script],
                capture_output=True,
                text=True,
                env=env,
                cwd=cwd,
                timeout=1,
            )
            output = result.stdout.rstrip("\r\n")
            if result.returncode == 0 and output:
                self._theme_cache_key = cache_key
                self._theme_cache_time = now
                self._theme_cache_output = output
                return output
        except (OSError, subprocess.TimeoutExpired):
            pass
        return ""

    @staticmethod
    def _find_theme_script(theme: str) -> str:
        """Locate theme script: user dir first, then built-in."""
        user_path = os.path.expanduser(f"~/.config/aish/scripts/themes/{theme}.aish")
        if os.path.isfile(user_path):
            return user_path
        # Built-in theme via importlib.resources (packaging-friendly)
        try:
            themes_pkg = importlib.resources.files("aish.scripts.themes")
            candidate = themes_pkg.joinpath(f"{theme}.aish")
            if hasattr(candidate, "is_file") and candidate.is_file():
                return str(candidate)
        except (ModuleNotFoundError, TypeError):
            pass
        return ""

    @staticmethod
    def _build_theme_env(cwd: str, exit_code: int) -> dict[str, str]:
        """Build environment variables for theme script execution."""
        env = dict(os.environ)
        env["AISH_CWD"] = cwd
        env["AISH_EXIT_CODE"] = str(exit_code)

        # Git status
        try:
            r = subprocess.run(
                ["git", "rev-parse", "--is-inside-work-tree"],
                capture_output=True, text=True, cwd=cwd, timeout=0.5,
            )
            if r.returncode == 0 and r.stdout.strip() == "true":
                env["AISH_GIT_REPO"] = "1"
                r = subprocess.run(
                    ["git", "branch", "--show-current"],
                    capture_output=True, text=True, cwd=cwd, timeout=0.5,
                )
                env["AISH_GIT_BRANCH"] = r.stdout.strip() or "HEAD" if r.returncode == 0 else "HEAD"

                r = subprocess.run(
                    ["git", "status", "--porcelain"],
                    capture_output=True, text=True, cwd=cwd, timeout=1,
                )
                if r.returncode == 0:
                    lines = r.stdout.strip().split("\n") if r.stdout.strip() else []
                    staged = sum(1 for ln in lines if ln and ln[0] in "MADRC")
                    modified = sum(1 for ln in lines if ln and ln[1] in "MD")
                    untracked = sum(1 for ln in lines if ln.startswith("??"))
                    env["AISH_GIT_STAGED"] = str(staged)
                    env["AISH_GIT_MODIFIED"] = str(modified)
                    env["AISH_GIT_UNTRACKED"] = str(untracked)
                    if staged > 0:
                        env["AISH_GIT_STATUS"] = "staged"
                    elif modified > 0 or untracked > 0:
                        env["AISH_GIT_STATUS"] = "dirty"
                    else:
                        env["AISH_GIT_STATUS"] = "clean"

                r = subprocess.run(
                    ["git", "rev-list", "--left-right", "--count", "@{upstream}...HEAD"],
                    capture_output=True, text=True, cwd=cwd, timeout=0.5,
                )
                if r.returncode == 0:
                    parts = r.stdout.strip().split()
                    if len(parts) == 2:
                        env["AISH_GIT_BEHIND"] = parts[0]
                        env["AISH_GIT_AHEAD"] = parts[1]
            else:
                env["AISH_GIT_REPO"] = "0"
        except (OSError, subprocess.TimeoutExpired):
            env["AISH_GIT_REPO"] = "0"

        # Virtual environment
        venv = os.environ.get("VIRTUAL_ENV", "")
        if venv and not venv.endswith("/aish/.venv") and "/aish/.venv/" not in venv:
            env["AISH_VIRTUAL_ENV"] = os.path.basename(venv)
        elif conda := os.environ.get("CONDA_DEFAULT_ENV", ""):
            env["AISH_VIRTUAL_ENV"] = conda

        return env


