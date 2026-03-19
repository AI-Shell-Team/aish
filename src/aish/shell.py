"""AI Shell - A shell with built-in LLM capabilities."""

from __future__ import annotations

import datetime as dt
import fcntl
import getpass
import json
import logging
import os
import re
import shlex
import signal
import subprocess
import sys
import termios
import threading
import time
import uuid
from contextlib import contextmanager
from pathlib import Path
from textwrap import dedent
from typing import Any, Optional

import anyio
from anyio import to_thread
from prompt_toolkit import PromptSession
from prompt_toolkit.auto_suggest import AutoSuggestFromHistory
from prompt_toolkit.history import FileHistory
from rich.console import Console
from rich.live import Live
from rich.markdown import Markdown
from rich.panel import Panel
from rich.text import Text

from .builtin import (BuiltinHandlers, DirectoryStack)
from .cancellation import CancellationReason
from .command import CommandDispatcher
from .config import (Config, ConfigModel, ToolArgPreviewSettings,
                     get_default_session_db_path)
from .context_manager import ContextManager, MemoryType
from .env_manager import EnvironmentManager
from .help_manager import HelpManager
from .history_manager import HistoryManager
from .i18n import t
from .interruption import (InterruptionManager, PromptConfig,
                           ShellState)
from .llm import LLMCallbackResult, LLMEvent, LLMEventType, LLMSession
from .logging_utils import set_session_uuid
from .providers.registry import get_provider_for_model
from .prompts import PromptManager
from .security.security_manager import SimpleSecurityManager
from .session_store import SessionRecord, SessionStore
from .shell_enhanced.shell_actions import build_default_actions
from .shell_enhanced.shell_command_service import ShellCommandService
from .shell_enhanced.shell_completion import (_SKILL_REF_EXTRACT_RE,
                                              ModeAwareCompleter,
                                              SkillReferenceCompleter,
                                              make_shell_completer)
from .shell_enhanced.shell_input_router import ShellInputRouter
from .shell_enhanced.shell_llm_events import LLMEventRouter
from .shell_enhanced.shell_prompt_io import \
    display_security_panel as _prompt_display_security_panel
from .shell_enhanced.shell_prompt_io import \
    get_user_confirmation as _prompt_get_user_confirmation
from .shell_enhanced.shell_prompt_io import \
    get_user_input as _prompt_get_user_input
from .shell_enhanced.shell_prompt_io import \
    handle_ask_user_required as _prompt_handle_ask_user_required
from .shell_enhanced.shell_prompt_io import \
    handle_ask_user_required_inline as _prompt_handle_ask_user_required_inline
from .shell_enhanced.shell_prompt_io import \
    handle_tool_confirmation_required as \
    _prompt_handle_tool_confirmation_required
from .shell_enhanced.shell_pty_executor import \
    execute_command_with_pty as _execute_command_with_pty_impl
from .shell_enhanced.shell_types import (ActionContext, CommandResult,
                                         CommandStatus, InputIntent)
from .skills import SkillManager
from .skills.hotreload import SkillHotReloadService
from .utils import (get_current_env_info, get_or_fetch_static_env_info,
                    get_output_language)
from .welcome_screen import build_welcome_renderable


def _build_passthrough_stdin_termios(settings: list[Any]) -> list[Any]:
    """Build a raw-like stdin mode that preserves input bytes for PTY forwarding.

    We intentionally disable CR/LF translations on stdin so Enter remains '\r'.
    Child programs then decide newline semantics using their own PTY termios
    configuration (ICRNL on/off), which avoids command-specific heuristics.
    """
    new_settings = list(settings)
    if len(new_settings) >= 7 and isinstance(new_settings[6], list):
        new_settings[6] = list(new_settings[6])

    iflag_mask = (
        termios.BRKINT
        | termios.ICRNL
        | termios.INPCK
        | termios.ISTRIP
        | termios.IXON
        | getattr(termios, "INLCR", 0)
        | getattr(termios, "IGNCR", 0)
    )
    new_settings[0] &= ~iflag_mask
    new_settings[1] &= ~termios.OPOST
    new_settings[2] |= termios.CS8
    new_settings[3] &= ~(termios.ECHO | termios.ICANON | termios.IEXTEN | termios.ISIG)
    new_settings[6][termios.VMIN] = 1
    new_settings[6][termios.VTIME] = 0
    return new_settings


class AIShell:
    """AI-enhanced shell with LLM integration and PTY support"""

    # Define comprehensive question mark characters from various languages
    # QUESTION_MARKS = {
    #     "?",  # Latin Question Mark (U+003F)
    #     "？",  # Fullwidth Question Mark (U+FF1F) - Chinese, Japanese
    #     "؟",  # Arabic Question Mark (U+061F)
    #     "՞",  # Armenian Question Mark (U+055E)
    #     "¿",  # Inverted Question Mark (U+00BF) - Spanish
    #     "⸮",  # Reversed Question Mark (U+2E2E) - Historical
    #     "？",  # Greek Question Mark (same as fullwidth)
    #     "¿",  # Used in Spanish (opening)
    #     "？",  # Used in Japanese/Chinese contexts
    #     "՝",  # Armenian Apostrophe/Question Mark (U+055D)
    #     "؟",  # Urdu Question Mark (same as Arabic)
    # }
    SEMICOLON_MARKS = {
        ";",  # Latin Semicolon (U+003B)
        "；",  # Fullwidth Semicolon (U+FF1B) - Chinese, Japanese, Korean
    }

    def __init__(
        self,
        config: ConfigModel,
        skill_manager: SkillManager,
        config_manager: Optional[Config] = None,
    ):
        self.config = config
        self.config_manager = config_manager
        self._approved_ai_commands: set[str] = set()
        self.skill_manager = skill_manager
        self.console = Console()
        self.logger = logging.getLogger("aish.shell")
        # Use a smaller debounce (200ms) to avoid long cleanup delays on exit
        # caused by watchfiles.awatch taking time to cancel during debounce sleep.
        self._skill_hotreload_service = SkillHotReloadService(
            skill_manager=skill_manager, debounce_ms=200
        )

        # Create a new persisted session record for each shell start.
        self.session_record: SessionRecord | None = None
        self._create_new_session_record()
        if self.session_record is None:
            raise RuntimeError("Failed to create session record")
        if self.session_record:
            set_session_uuid(self.session_record.session_uuid)

        # Initialize history and environment managers before LLMSession
        # Use same db_path as SessionStore, default to XDG data directory.
        db_path_str = getattr(self.config, "session_db_path", "")
        if not str(db_path_str).strip():
            db_path_str = get_default_session_db_path()

        self.history_manager = HistoryManager(
            db_path=Path(db_path_str).expanduser(),
            session_uuid=self.session_record.session_uuid,
        )
        self.env_manager = EnvironmentManager()
        # Initialize directory stack and share it with env_manager
        # This allows BashTool to access the same directory_stack instance
        self.directory_stack = DirectoryStack()
        self.env_manager.directory_stack = self.directory_stack

        # Initialize interruption manager for Ctrl+C and Esc handling
        # Must be initialized before LLMSession
        self.interruption_manager = InterruptionManager()
        self.interruption_manager.set_interrupt_callback(self._on_interrupt_requested)

        self.llm_session = LLMSession(
            config=config,
            skill_manager=skill_manager,
            event_callback=self.handle_llm_event,
            env_manager=self.env_manager,
            interruption_manager=self.interruption_manager,
            is_command_approved=self._is_command_approved,
            history_manager=self.history_manager,
        )

        # Event handling infrastructure
        self.current_live = None
        self.event_handlers = {
            LLMEventType.OP_START: self.handle_operation_start,
            LLMEventType.GENERATION_START: self.handle_generation_start,
            LLMEventType.GENERATION_END: self.handle_generation_end,
            LLMEventType.REASONING_START: self.handle_reasoning_start,
            LLMEventType.REASONING_END: self.handle_reasoning_end,
            LLMEventType.REASONING_DELTA: self.handle_reasoning_delta,
            LLMEventType.CONTENT_DELTA: self.handle_content_delta,
            LLMEventType.OP_END: self.handle_operation_end,
            LLMEventType.TOOL_EXECUTION_START: self.handle_tool_execution_start,
            LLMEventType.TOOL_EXECUTION_END: self.handle_tool_execution_end,
            LLMEventType.ERROR: self.handle_error_event,
            LLMEventType.TOOL_CONFIRMATION_REQUIRED: self.handle_tool_confirmation_required,
            LLMEventType.ASK_USER_REQUIRED: self.handle_ask_user_required,
            LLMEventType.CANCELLED: self.handle_processing_cancelled,
        }
        self._llm_event_router = LLMEventRouter(self.event_handlers)
        self._shell_completer = make_shell_completer()
        self._ai_completer = SkillReferenceCompleter(self.skill_manager)
        self._mode_aware_completer = ModeAwareCompleter(
            self._ai_completer,
            self._shell_completer,
            self.SEMICOLON_MARKS,
        )

        self.session = PromptSession(
            history=FileHistory(os.path.expanduser("~/.aish_history")),
            auto_suggest=AutoSuggestFromHistory(),
            completer=self._mode_aware_completer,
            complete_while_typing=False,
            # Bash-style completion: don't reserve space for menu
            reserve_space_for_menu=0,
        )
        self.running = True
        self.prompt_manager = PromptManager()
        self.command_dispatcher = CommandDispatcher()
        self.context_manager = ContextManager(
            max_llm_messages=getattr(config, "max_llm_messages", 50),
            max_shell_messages=getattr(config, "max_shell_messages", 20),
            token_budget=getattr(config, "context_token_budget", None),
            model=config.model,
            enable_token_estimation=getattr(config, "enable_token_estimation", True),
        )

        # TUI mode support (set externally when --tui is enabled)
        self._tui_app: Optional[Any] = None

        # Flag to track if we're currently processing input
        self.processing_input = False

        # LLM call context tracking (used to avoid duplicate LLM calls after an LLM failure).
        # - When we call LLM just to "guess_command", a failure should NOT trigger a second
        #   LLM call via handle_ai_command.
        self._llm_call_context: str | None = None
        self._command_detection_llm_failed: bool = False

        # Animation state management
        self.animation_active = False
        self.animation_thread: Optional[threading.Thread] = None
        self.animation_counter = 0
        self.animation_lock = threading.Lock()
        self._animation_base_text = t("shell.status.thinking")
        self._animation_pattern = "braille"
        self._animation_update_text = None
        self._reasoning_display_enabled = False
        self._reasoning_active = False
        self._reasoning_partial = ""
        self._reasoning_lines: list[str] = []
        self._reasoning_max_lines = 2
        self._content_preview_active = False
        # Track whether the cursor is at the start of a new line.
        self._at_line_start = True

        # Inline selection UI state (for ask_user in main prompt)
        self._inline_selection: dict | None = None  # Active selection UI state

        # 仅保留沙箱安全体系：repo_root 默认绑定到当前工作目录。
        self.security_manager = SimpleSecurityManager(
            console=self.console,
            repo_root=Path(os.getcwd()).resolve(),
        )

        # Flag to track user-requested exit (via Ctrl+C twice)
        self._user_requested_exit = False

        # Pending error correction info (set when command fails)
        self._pending_error_correction: Optional[dict] = None

        # Last command exit code for prompt hooks
        self._last_exit_code: int = 0

        # Pre-load system info (static info from cache if available)
        self.uname_info, self.os_info, self.basic_env_info = (
            get_or_fetch_static_env_info()
        )
        self.output_language = get_output_language(config)

        # Dynamic environment info - fetch and add to context on startup
        self.current_env_info = get_current_env_info()
        self._last_current_env_info = self.current_env_info

        # Add initial current_env_info to context (like command execution result)
        env_entry = f"[system information] {self.current_env_info}"
        self.context_manager.add_memory(
            MemoryType.LLM, {"role": "user", "content": env_entry}
        )

        # Cache system knowledge
        self.context_manager.add_memory(
            MemoryType.KNOWLEDGE, {"key": "system_info", "value": self.uname_info}
        )
        self.context_manager.add_memory(
            MemoryType.KNOWLEDGE, {"key": "os_info", "value": self.os_info}
        )
        self.context_manager.add_memory(
            MemoryType.KNOWLEDGE,
            {"key": "output_language", "value": self.output_language},
        )
        self.context_manager.add_memory(
            MemoryType.KNOWLEDGE,
            {"key": "basic_env_info", "value": self.basic_env_info},
        )
        self.context_manager.add_memory(
            MemoryType.KNOWLEDGE,
            {"key": "current_env_info", "value": self.current_env_info},
        )

        # Initialize LiteLLM
        self.setup_llm()

        self.help_manager = HelpManager(self.console)

        # Plan mode components
        from .plans.manager import PlanManager
        from .plans.plan_agent import PlanAgent

        self.plan_manager = PlanManager()
        self.plan_agent = PlanAgent(
            config=config,
            model_id=config.model,
            skill_manager=skill_manager,
            plan_manager=self.plan_manager,
            shell=self,  # Pass shell reference for TUI updates
            api_base=config.api_base,
            api_key=config.api_key,
            parent_event_callback=self.handle_llm_event,
            cancellation_token=self.llm_session.cancellation_token,
            history_manager=self.history_manager,
        )

        # Register plan agent to LLM session for tool access
        self.llm_session.tools[self.plan_agent.name] = self.plan_agent

        self.task_counter = 0
        # Per-operation cancel scope (reset for each user request)
        self._current_op_scope: anyio.CancelScope | None = None
        # Track whether an operation is in progress (LLM call or command exec)
        self.operation_in_progress: bool = False
        self._terminal_resize_mode = self._resolve_terminal_resize_mode()
        self._last_terminal_size: tuple[int, int] | None = self._read_terminal_size()
        self._last_reasoning_render_lines: list[str] = []
        self._last_streaming_accumulated: str = ""
        self._input_router = ShellInputRouter(self)
        self._command_service = ShellCommandService(self)
        self._actions = build_default_actions(self, self._command_service)

        # Initialize script system (only if enabled)
        if self.config.enable_scripts:
            from .scripts import ScriptExecutor, ScriptHotReloadService, ScriptRegistry
            from .scripts.hooks import HookManager

            self.logger.debug("Initializing script system")

            self.script_registry = ScriptRegistry()
            self.script_executor = ScriptExecutor(
                llm_session=self.llm_session,
                env_manager=self.env_manager,
            )
            self.hook_manager = HookManager(self.script_registry, self.script_executor)

            # Initialize script hot reload service
            self._script_hotreload_service = ScriptHotReloadService(
                script_registry=self.script_registry, debounce_ms=200
            )
            # Start the hot reload service
            self._script_hotreload_service.start()

            # Load scripts on startup
            try:
                self.script_registry.load_all_scripts()
                script_count = len(self.script_registry.list_scripts())
                if script_count > 0:
                    self.logger.debug("Loaded %d scripts", script_count)
            except Exception as e:
                self.logger.warning("Failed to load scripts: %s", e)

            # Initialize default prompt template
            self._init_prompt_template()

    def _init_prompt_template(self) -> None:
        """Initialize default prompt template if not exists."""
        import shutil

        # Skip for custom config or pytest
        if getattr(self.config, "is_custom_config", False):
            return

        is_pytest = any(
            name == "pytest" or name.startswith("pytest.") or name.startswith("_pytest")
            for name in sys.modules
        )
        if is_pytest:
            return

        # User hooks directory - use config_dir if available, fallback to default
        if self.config_manager and hasattr(self.config_manager, "config_dir"):
            base_config_dir = self.config_manager.config_dir
        else:
            # Fallback: check XDG_CONFIG_HOME or use default
            xdg_config = os.environ.get("XDG_CONFIG_HOME")
            if xdg_config:
                base_config_dir = Path(xdg_config) / "aish"
            else:
                base_config_dir = Path.home() / ".config" / "aish"

        user_hooks_dir = base_config_dir / "scripts" / "hooks"
        user_prompt = user_hooks_dir / "aish_prompt.aish"

        # Skip if user already has a prompt
        if user_prompt.exists():
            return

        # Find template in package
        template_path = None

        # Check for development location
        dev_template = Path(__file__).parent / "scripts" / "templates" / "aish_prompt.aish"
        if dev_template.exists():
            template_path = dev_template

        # Check for PyInstaller bundle
        if not template_path and getattr(sys, "frozen", False):
            if hasattr(sys, "_MEIPASS"):
                bundle_template = Path(sys._MEIPASS) / "aish" / "scripts" / "templates" / "aish_prompt.aish"
                if bundle_template.exists():
                    template_path = bundle_template

        # Check for system install locations
        if not template_path:
            for sys_path in [
                Path("/usr/share/aish/scripts/templates/aish_prompt.aish"),
                Path("/usr/local/share/aish/scripts/templates/aish_prompt.aish"),
            ]:
                if sys_path.exists():
                    template_path = sys_path
                    break

        # Copy template if found
        if template_path:
            try:
                user_hooks_dir.mkdir(parents=True, exist_ok=True)
                shutil.copy2(template_path, user_prompt)
                self.logger.debug("Installed default prompt template to %s", user_prompt)
                # Reload scripts so the new prompt is immediately available
                if hasattr(self, "script_registry"):
                    self.script_registry.load_all_scripts()
            except (OSError, IOError) as e:
                self.logger.debug("Could not install prompt template: %s", e)

    def _resolve_terminal_resize_mode(self) -> str:
        mode = str(getattr(self.config, "terminal_resize_mode", "full")).strip().lower()
        if mode in {"full", "pty_only", "off"}:
            return mode
        return "full"

    def _is_pty_resize_enabled(self) -> bool:
        return self._terminal_resize_mode in {"full", "pty_only"}

    def _is_ui_resize_enabled(self) -> bool:
        return self._terminal_resize_mode == "full"

    def _read_terminal_size(self) -> tuple[int, int] | None:
        fds: list[int] = []
        for stream in (sys.stdin, sys.stdout, sys.stderr):
            try:
                fds.append(stream.fileno())
            except (AttributeError, OSError, ValueError):
                continue

        for fd in fds:
            try:
                size = os.get_terminal_size(fd)
                rows = int(size.lines)
                cols = int(size.columns)
                if rows > 0 and cols > 0:
                    return rows, cols
            except (OSError, ValueError):
                continue

        try:
            size = os.get_terminal_size()
            rows = int(size.lines)
            cols = int(size.columns)
            if rows > 0 and cols > 0:
                return rows, cols
        except (OSError, ValueError):
            pass
        return None

    def _set_pty_winsize(self, fd: int, rows: int, cols: int) -> bool:
        try:
            import struct

            winsize = struct.pack("HHHH", rows, cols, 0, 0)
            fcntl.ioctl(fd, termios.TIOCSWINSZ, winsize)
            return True
        except (OSError, ValueError):
            return False

    def _sync_pty_resize(
        self,
        master_fd: int,
        process: subprocess.Popen[Any] | None,
        last_size: tuple[int, int] | None,
    ) -> tuple[int, int] | None:
        if not self._is_pty_resize_enabled():
            return last_size

        current_size = self._read_terminal_size()
        if current_size is None or current_size == last_size:
            return last_size

        rows, cols = current_size
        winsize_updated = self._set_pty_winsize(master_fd, rows, cols)
        if not winsize_updated:
            return last_size

        try:
            if winsize_updated and process is not None and process.poll() is None:
                os.killpg(process.pid, signal.SIGWINCH)
        except (OSError, ProcessLookupError, AttributeError):
            pass

        return current_size

    def _refresh_live_for_resize(self) -> None:
        if not self._is_ui_resize_enabled() or not self.current_live:
            return

        if self._reasoning_active or self._last_reasoning_render_lines:
            self._update_reasoning_live(self._last_reasoning_render_lines)
            return

        if self._last_streaming_accumulated:
            self._render_streaming_chunk(self._last_streaming_accumulated)

    def _check_terminal_resize(self) -> None:
        current_size = self._read_terminal_size()
        if current_size is None:
            return
        if current_size == self._last_terminal_size:
            return
        self._last_terminal_size = current_size
        self._refresh_live_for_resize()

    @staticmethod
    def _compute_ask_user_max_visible(
        total_options: int,
        term_rows: int,
        allow_custom_input: bool,
        max_visible_cap: int = 12,
    ) -> int:
        reserved_rows = 8 if allow_custom_input else 6
        visible = max(3, term_rows - reserved_rows)
        visible = min(visible, max_visible_cap)
        return min(total_options, visible)

    @contextmanager
    def _safe_cancel_scope(self):
        """Provide a CancelScope when running under anyio, fallback otherwise."""
        scope = anyio.CancelScope()
        try:
            entered = scope.__enter__()
        except AssertionError:
            # pytest-asyncio context: no anyio task state available.
            yield None
            return
        try:
            yield entered
        finally:
            scope.__exit__(None, None, None)

    def _is_command_approved(self, command: str) -> bool:
        command = str(command)
        return bool(command) and command in self._approved_ai_commands

    def _remember_approved_command(self, command: str) -> None:
        command = str(command)
        if not command:
            return

        self._approved_ai_commands.add(command)

    def _create_new_session_record(self) -> None:
        db_path_value = getattr(self.config, "session_db_path", "")
        if not str(db_path_value).strip():
            db_path_value = get_default_session_db_path()
        db_path = Path(db_path_value).expanduser()

        store: SessionStore | None = None
        try:
            store = SessionStore(db_path)
            self.session_record = store.create_session(
                model=self.config.model,
                api_base=getattr(self.config, "api_base", None),
                run_user=getpass.getuser(),
                state={
                    "temperature": getattr(self.config, "temperature", None),
                    "max_tokens": getattr(self.config, "max_tokens", None),
                },
            )
        except Exception:
            # Session persistence should not prevent the shell from running.
            # Fallback to an in-memory session record to keep a single session UUID.
            self.session_record = SessionRecord(
                session_uuid=str(uuid.uuid4()),
                created_at=dt.datetime.utcnow(),
                model=self.config.model,
                api_base=getattr(self.config, "api_base", None),
                run_user=getpass.getuser(),
                state={
                    "temperature": getattr(self.config, "temperature", None),
                    "max_tokens": getattr(self.config, "max_tokens", None),
                },
            )
        finally:
            if store is not None:
                try:
                    store.close()
                except Exception:
                    pass

    def get_prompt(self) -> str:
        """Return the current prompt string."""
        try:
            cwd = os.getcwd()
        except (FileNotFoundError, OSError):
            # Current directory was deleted, try to recover
            home_dir = os.path.expanduser("~")
            try:
                os.chdir(home_dir)
                os.environ["PWD"] = home_dir
                cwd = home_dir
            except Exception:
                cwd = "<deleted>"

        # Update TUI status bar with current directory
        if self._tui_app is not None:
            self._tui_app.set_cwd(cwd)

        # Legacy mode: simple prompt when scripts disabled
        if not getattr(self.config, "enable_scripts", True):
            prompt_style = getattr(self.config, "prompt_style", "🚀")
            return f"{prompt_style} {os.path.basename(cwd)} > "

        # Try custom prompt hook first
        if hasattr(self, "hook_manager") and self.hook_manager:
            custom_prompt = self.hook_manager.run_prompt_hook(
                last_exit_code=self._last_exit_code
            )
            if custom_prompt:
                return custom_prompt

        # Fallback: enhanced default prompt with git info
        return self._build_default_prompt(cwd)

    def _build_default_prompt(self, cwd: str) -> str:
        """Build default prompt with compact colorful style.

        Args:
            cwd: Current working directory.

        Returns:
            Default prompt string with path, git status, and colors.
        """
        import subprocess

        # ANSI colors
        R, D = "\033[0m", "\033[2m"
        G, Y, RD, BL, M, C = "\033[32m", "\033[33m", "\033[31m", "\033[34m", "\033[35m", "\033[36m"

        # Abbreviate path
        home = os.path.expanduser("~")
        display_path = cwd
        if cwd.startswith(home):
            display_path = "~" + cwd[len(home) :]

        # Shorten middle directories
        parts = display_path.split("/")
        if len(parts) > 1:
            abbreviated = parts[0]  # ~
            for i, part in enumerate(parts[1:-1], 1):
                if part:
                    abbreviated += "/" + part[0]
            abbreviated += "/" + parts[-1]
            display_path = abbreviated

        p = f":{BL}{display_path}{R}"

        # Git status
        try:
            result = subprocess.run(
                ["git", "rev-parse", "--is-inside-work-tree"],
                capture_output=True,
                text=True,
                cwd=cwd,
                timeout=0.3,
            )
            if result.returncode == 0 and result.stdout.strip() == "true":
                # Branch
                result = subprocess.run(
                    ["git", "branch", "--show-current"],
                    capture_output=True,
                    text=True,
                    cwd=cwd,
                    timeout=0.3,
                )
                branch = result.stdout.strip() or "HEAD"
                p += f"|{M}{branch}{R}"

                # Status
                result = subprocess.run(
                    ["git", "status", "--porcelain"],
                    capture_output=True,
                    text=True,
                    cwd=cwd,
                    timeout=0.5,
                )
                if result.returncode == 0:
                    lines = result.stdout.strip().split("\n") if result.stdout.strip() else []
                    staged = sum(1 for line in lines if line and line[0] in "MADRC")
                    modified = sum(1 for line in lines if line and line[1] in "MD")
                    untracked = sum(1 for line in lines if line.startswith("??"))

                    # Status indicator color
                    if staged > 0:
                        p += f"{Y}●{R} {Y}+{staged}{R}"
                    elif modified > 0:
                        p += f"{RD}●{R} {RD}~{modified}{R}"
                    elif untracked > 0:
                        p += f"{C}●{R} {D}?{untracked}{R}"
                    else:
                        p += f"{G}●{R}"

                # Ahead/behind
                result = subprocess.run(
                    ["git", "rev-list", "--left-right", "--count", "@{upstream}...HEAD"],
                    capture_output=True,
                    text=True,
                    cwd=cwd,
                    timeout=0.3,
                )
                if result.returncode == 0:
                    parts = result.stdout.strip().split()
                    if len(parts) == 2:
                        behind, ahead = parts
                        if ahead != "0":
                            p += f" {C}↑{ahead}{R}"
                        if behind != "0":
                            p += f" {C}↓{behind}{R}"

        except (OSError, subprocess.SubprocessError, subprocess.TimeoutExpired):
            pass

        # Virtual env
        venv = os.environ.get("VIRTUAL_ENV")
        if venv and not venv.endswith("/aish/.venv") and "/aish/.venv/" not in venv:
            p += f" {C}🐍{R}"
        elif os.environ.get("CONDA_DEFAULT_ENV"):
            p += f" {C}🐍{R}"

        # Prompt symbol - use internal exit code for reliability
        if self._last_exit_code != 0:
            p += f" {RD}➜➜{R} "
        else:
            p += f" {G}➜{R} "

        return p

    async def get_user_input(
        self, prompt_text: Optional[str] = None, _recursion_depth: int = 0
    ) -> str:
        """Get user input with the configured prompt.

        Args:
            prompt_text: Optional custom prompt text
            _recursion_depth: Internal recursion depth counter (do not set externally)
        """
        return await _prompt_get_user_input(self, prompt_text, _recursion_depth)

    def _get_prompt_style(self):
        """获取 prompt 样式配置（缓存）- 已废弃，使用 message 参数"""
        # 保留此方法以避免兼容性问题，但不再使用
        return None

    def stop(self) -> None:
        """Stop the shell loop."""
        self.running = False

    def _exit_shell(self) -> None:
        """Exit entrypoint: finalize UI and print goodbye on a clean line."""
        # Stop script hot reload service
        if hasattr(self, "_script_hotreload_service"):
            self._script_hotreload_service.stop()

        # Ensure any live/streaming UI is finalized before exit message.
        self._stop_animation()
        self._reset_reasoning_state()
        self._finalize_content_preview()
        if self.current_live:
            try:
                self.current_live.update("", refresh=True)
                self.current_live.stop()
            finally:
                self.current_live = None

        # Move to a fresh line only when we're mid-line.
        if not self._at_line_start:
            self.console.print()
            self._at_line_start = True
        self.console.print(t("cli.startup.goodbye"), style="green")
        self._at_line_start = True

    def starts_with_question_mark(self, text: str) -> bool:
        """Check if text starts with the AI prefix mark (semicolon).

        NOTE: Historical name kept for backward compatibility.
        """
        if not text:
            return False
        return text[0] in self.SEMICOLON_MARKS

    def strip_leading_question_mark(self, text: str) -> str:
        """Remove leading AI prefix mark (semicolon) from text.

        NOTE: Historical name kept for backward compatibility.
        """
        if not text:
            return text
        if text[0] in self.SEMICOLON_MARKS:
            return text[1:].strip()
        return text

    def _extract_skill_refs(self, text: str) -> list[str]:
        available = {skill.metadata.name for skill in self.skill_manager.list_skills()}
        if not available:
            return []
        refs: list[str] = []
        seen: set[str] = set()
        for match in _SKILL_REF_EXTRACT_RE.findall(text):
            name = match.lower()
            if name in available and name not in seen:
                refs.append(name)
                seen.add(name)
        return refs

    def _inject_skill_prefix(self, text: str) -> str:
        refs = self._extract_skill_refs(text)
        if not refs:
            return text
        prefix = " ".join([f"use {name} skill to do this." for name in refs])
        return f"{prefix}\n\n{text}"

    async def _get_multiline_input(
        self, initial_prompt: str, first_line: str, ai_mode: bool = True
    ) -> str:
        """Get multiline input with backslash continuation support.

        Args:
            initial_prompt: The initial prompt text (e.g., "🚀 ~ → ")
            first_line: The first line of input already received
            ai_mode: If True, merge with newlines (for AI prompts). If False, merge with spaces (for commands).

        Returns:
            The complete multiline input merged into a single string
        """
        if not first_line:
            return first_line

        lines = [first_line]

        # Check if first line ends with backslash for continuation
        while lines[-1].endswith("\\"):
            # Remove the trailing backslash
            lines[-1] = lines[-1][:-1].rstrip()

            # Show continuation prompt and get next line
            continuation_line = await self.session.prompt_async(
                "... ", handle_sigint=False
            )
            if not continuation_line:
                # Empty line on continuation still adds to lines
                lines.append("")
            else:
                lines.append(continuation_line)

        # Merge lines based on mode
        # AI mode: use newlines to preserve prompt structure
        # Command mode: use spaces like fish-shell
        if ai_mode:
            return "\n".join(lines)
        else:
            return " ".join(line for line in lines if line)

    def setup_llm(self):
        """Setup LiteLLM configuration"""
        pass

    def print_welcome(self):
        """Print welcome message"""
        self.console.print(build_welcome_renderable(self.config))

    async def execute_command_with_pty(self, command: str) -> CommandResult:
        """Execute a command with PTY support and enhanced error handling."""
        return await _execute_command_with_pty_impl(self, command)

    async def execute_command_with_security(
        self, command: str, record_history: bool = True
    ) -> CommandResult:
        """Execute a shell command with security checks (AI commands only)."""
        try:
            # Check for state-modifying built-in commands first
            # These need special handling and cannot be executed with subprocess
            from aish.builtin import BuiltinRegistry

            if BuiltinRegistry.is_state_modifying_command(command):
                # Handle built-in commands directly
                return await self._execute_builtin_command(
                    command, record_history=record_history
                )

            # Exact-match allowlist: skip confirmation for previously approved commands.
            if self._is_command_approved(command):
                return await self.execute_command(command)

            # 统一走安全决策：
            # - allow=False: 直接阻断（不进入确认流程）
            # - allow=True & require_confirmation=True: 进入确认
            # - allow=True & require_confirmation=False: 直接执行

            decision = self.security_manager.decide(
                command,
                is_ai_command=True,
                cwd=Path(os.getcwd()).resolve(),
            )
            analysis_data = decision.analysis or {}

            if not decision.allow:
                # 直接阻断：复用统一的安全面板渲染，确保样式与字段一致
                self._display_security_panel(
                    {
                        "tool_name": "shell_command",
                        "command": command,
                        "security_analysis": analysis_data,
                    },
                    panel_mode="blocked",
                )
                return CommandResult(
                    CommandStatus.CANCELLED,
                    1,
                    "",
                    "Blocked by security policy",
                    offload={"status": "inline", "reason": "security_blocked"},
                )

            if decision.require_confirmation:
                confirmation_data = {
                    "tool_name": "shell_command",
                    "command": command,
                    "security_analysis": analysis_data,
                }
                try:
                    user_response = await to_thread.run_sync(
                        self._get_shell_command_confirmation,
                        confirmation_data,
                    )
                except AssertionError:
                    # Fallback for non-anyio runtimes (e.g. pytest-asyncio).
                    user_response = self._get_shell_command_confirmation(
                        confirmation_data
                    )
                if user_response != LLMCallbackResult.APPROVE:
                    return CommandResult(
                        CommandStatus.CANCELLED,
                        1,
                        "",
                        "User cancelled",
                        offload={"status": "inline", "reason": "user_cancelled"},
                    )

            return await self.execute_command(command)

        except anyio.get_cancelled_exc_class():
            # Handle explicit anyio cancellation
            return CommandResult(
                CommandStatus.ERROR,
                1,
                "",
                "Execution cancelled by system",
                offload={"status": "inline", "reason": "execution_cancelled"},
            )
        except Exception as e:
            return CommandResult(
                CommandStatus.ERROR,
                1,
                "",
                f"Execution error: {e}",
                offload={"status": "inline", "reason": "execution_error"},
            )

    async def execute_command(self, command: str) -> CommandResult:
        """Execute a shell command asynchronously using PTY"""
        try:
            return await self.execute_command_with_pty(command)

        except Exception as e:
            print(f"Error executing command: {e}")
            return CommandResult(
                CommandStatus.ERROR,
                1,
                "",
                str(e),
                offload={"status": "inline", "reason": "execute_command_exception"},
            )

    async def _execute_builtin_command(
        self, command: str, record_history: bool = True
    ) -> CommandResult:
        """Execute a built-in command (cd, pushd, popd, export, unset, dirs, pwd, history)."""
        from aish.builtin import BuiltinHandlers
        from aish.tools.bash_executor import UnifiedBashExecutor

        cmd_parts = command.strip().split()
        if not cmd_parts:
            return CommandResult(
                CommandStatus.ERROR,
                1,
                "",
                "Empty command",
                offload={"status": "inline", "reason": "builtin_command"},
            )

        cmd_name = cmd_parts[0].lower()

        # history command has its own handler with special logic
        if cmd_name == "history":
            result_returncode, result_stderr = await self.handle_history_command(
                command,
                record_history=record_history,
            )
            status = (
                CommandStatus.SUCCESS if result_returncode == 0 else CommandStatus.ERROR
            )
            return CommandResult(
                status=status,
                exit_code=result_returncode,
                stdout="",
                stderr=result_stderr,
                offload={"status": "inline", "reason": "builtin_command"},
            )

        # pushd/popd/dirs 需要特殊处理，因为它们依赖 Python 目录栈对象
        if cmd_name in ("pushd", "popd", "dirs"):
            cwd = os.getcwd()
            if cmd_name == "pushd":
                result = BuiltinHandlers.handle_pushd(
                    command, cwd, self.directory_stack
                )
            elif cmd_name == "popd":
                result = BuiltinHandlers.handle_popd(command, cwd, self.directory_stack)
            else:  # dirs
                result = BuiltinHandlers.handle_dirs(command, cwd, self.directory_stack)

            # 应用状态变化
            if result.new_cwd:
                os.chdir(result.new_cwd)
            if result.directory_stack_push:
                self.directory_stack.push(result.directory_stack_push)
            for key, value in result.env_vars_to_set.items():
                os.environ[key] = value

            # 添加历史记录
            if record_history:
                await self.history_manager.add_entry(
                    command=command,
                    source="user",
                    returncode=result.returncode,
                    stdout=result.output if result.success else "",
                    stderr=result.error if not result.success else "",
                )

            # 转换为 CommandResult
            status = CommandStatus.SUCCESS if result.success else CommandStatus.ERROR
            return CommandResult(
                status=status,
                exit_code=result.returncode,
                stdout=result.output if result.success else "",
                stderr=result.error if not result.success else "",
                offload={"status": "inline", "reason": "builtin_command"},
            )

        # cd, export, unset, pwd 使用 UnifiedBashExecutor
        executor = UnifiedBashExecutor(
            env_manager=self.env_manager,
            # 不传递 history_manager，避免重复记录历史
            history_manager=None,
        )

        # 执行命令（自动捕获状态）- 同步方法，不需要 await
        success, stdout, stderr, returncode, changes = executor.execute(
            command, source="user"
        )

        # 手动添加历史记录
        if record_history:
            await self.history_manager.add_entry(
                command=command,
                source="user",
                returncode=returncode,
                stdout=stdout,
                stderr=stderr,
            )

        # 转换为 CommandResult
        status = CommandStatus.SUCCESS if success else CommandStatus.ERROR

        return CommandResult(
            status=status,
            exit_code=returncode,
            stdout=stdout,
            stderr=stderr,
            offload={"status": "inline", "reason": "builtin_command"},
        )

    async def ask_oracle_fast(
        self, prompt: str, system_message: Optional[str] = None, **kwargs
    ) -> str:
        output = await self.llm_session.completion(
            prompt, system_message=system_message, emit_events=True, stream=True
        )
        return output

    # FIXME: tool call history is not added to memory, wrong ?
    async def ask_oracle(
        self, prompt: str, system_message: Optional[str] = None, **kwargs
    ) -> str:
        """Ask the LLM a question"""
        try:
            output = await self.llm_session.process_input(
                prompt,
                context_manager=self.context_manager,
                system_message=system_message,
                stream=True,
                **kwargs,
            )
            return output
        except json.JSONDecodeError:
            raise
        except Exception as e:
            self.console.print(
                t("shell.error.execution_error", error=str(e)),
                style="red",
            )
            return ""

    def _show_tool_args_json_error_hint(self) -> None:
        self.console.print(t("shell.error.tool_args_json_invalid_hint"), style="red")

    async def handle_ai_command(self, question: str):
        """Handle AI question command"""
        if not question:
            return

        # Record user input to history file
        await self.history_manager.add_entry(
            command=question,
            source="user",
            returncode=None,  # AI requests don't have return codes
            stdout=None,
            stderr=None,
        )

        try:
            # 保存原始输入（带 AI 前缀符）以便中断后恢复
            original_input = question

            # Remove leading AI prefix mark
            question = self.strip_leading_question_mark(question)
            question = self._inject_skill_prefix(question)

            # 检查动态环境信息是否有变化
            new_current_env_info = get_current_env_info()
            if new_current_env_info != self._last_current_env_info:
                env_entry = (
                    f"[Reminder: the system information updated] {new_current_env_info}"
                )
                self.context_manager.add_memory(
                    MemoryType.LLM, {"role": "user", "content": env_entry}
                )
                self._last_current_env_info = new_current_env_info
                self.current_env_info = new_current_env_info

            context = self.context_manager.as_messages()

            system_message = self.prompt_manager.substitute_template(
                "oracle",
                user_nickname=os.getenv("USER", "user"),
                uname_info=self.uname_info,
                os_info=self.os_info,
                basic_env_info=self.basic_env_info,
                output_language=self.output_language,
            )

            # Wrap the LLM call in a per-operation cancel scope
            with self._safe_cancel_scope() as scope:
                self._current_op_scope = scope
                self.operation_in_progress = True
                # 设置 AI 执行状态
                self.interruption_manager.set_state(ShellState.AI_THINKING)
                # 保存输入缓冲区以便中断后恢复
                self.interruption_manager.save_input_buffer(original_input)
                # 标志：是否正常完成（非取消）
                completed_normally = True
                try:
                    response = await self.ask_oracle(
                        question, system_message, history=context
                    )
                    if len(response) > 0:
                        await self.process_ai_response(response, border_style="green")
                except anyio.get_cancelled_exc_class():
                    # 被取消，不清除输入缓冲区
                    completed_normally = False
                    raise
                finally:
                    # 注意：不在 finally 中清除 operation_in_progress
                    # 让外层的异常处理来清除
                    self._current_op_scope = None
                    self.interruption_manager.set_state(ShellState.NORMAL)
                    # 只有正常完成时才清除输入缓冲区
                    if completed_normally:
                        self.interruption_manager._input_buffer = None
                        self.interruption_manager._restore_input = False

            # 正常完成后清除 operation_in_progress
            self.operation_in_progress = False

        except (KeyboardInterrupt, EOFError):
            self.operation_in_progress = False
            self.handle_processing_cancelled()
            # 恢复正常状态
            self.interruption_manager.set_state(ShellState.NORMAL)
        except json.JSONDecodeError:
            self.operation_in_progress = False
            self.interruption_manager.set_state(ShellState.NORMAL)
            self._show_tool_args_json_error_hint()

    async def process_ai_response(self, response: str, border_style: str = "green"):
        # self.console.print(f"process_ai_response: {response}")

        # First, check if the response contains a JSON command
        json_cmd = self.try_parse_json_output(response)
        if json_cmd:
            # Handle the JSON command
            await self.handle_json_command(json_cmd)
        else:
            # Normal response without JSON command
            self.console.print(Panel(Markdown(response), border_style=border_style))

    async def handle_error_detect(self, command: str, stdout: str, stderr: str):
        """Handle smart error detection when return code is 0"""
        system_message = self.prompt_manager.substitute_template(
            "error_detect",
            user_nickname=os.getenv("USER", "user"),
            uname_info=self.uname_info,
            os_info=self.os_info,
            basic_env_info=self.basic_env_info,
            output_language=self.output_language,
        )
        prompt = f"""
        <command_result>
        Command: {command}
        stdout:
        {stdout}
        stderr:
        {stderr}
        </command_result>"""

        try:
            # Wrap the LLM call in a per-operation cancel scope
            with self._safe_cancel_scope() as scope:
                self._current_op_scope = scope
                self.operation_in_progress = True
                # 设置 AI 执行状态
                self.interruption_manager.set_state(ShellState.AI_THINKING)
                # 标志：是否正常完成（非取消）
                completed_normally = True
                try:
                    response = await self.ask_oracle_fast(
                        dedent(prompt), system_message
                    )
                    if len(response) > 0:
                        json_cmd = self.try_parse_json_output(response)
                        if json_cmd and json_cmd.get("type") == "error_detect":
                            if json_cmd.get("is_success"):
                                return

                            reason = json_cmd.get("reason", "")
                            self.console.print(
                                t("shell.error.potential_error", reason=reason),
                                style="bold red",
                            )
                            await self.handle_command_error(
                                command, stdout, stderr, reason
                            )
                except anyio.get_cancelled_exc_class():
                    # 被取消
                    completed_normally = False
                    raise
                finally:
                    self._current_op_scope = None
                    self.interruption_manager.set_state(ShellState.NORMAL)
                    # 只有正常完成时才清除输入缓冲区
                    if completed_normally:
                        self.interruption_manager._input_buffer = None
                        self.interruption_manager._restore_input = False

            # 正常完成后清除 operation_in_progress
            self.operation_in_progress = False

        except (KeyboardInterrupt, EOFError):
            self.operation_in_progress = False
            self.handle_processing_cancelled()
            # 恢复正常状态
            self.interruption_manager.set_state(ShellState.NORMAL)

    async def handle_command_error(
        self, command: str, stdout: str, stderr: str, reason: Optional[str] = None
    ):
        """Handle command error, assumes that only actual execution errors are passed here."""
        # Error is already printed by caller, don't print again

        # Set CORRECT_PENDING state and show hint
        # The actual input handling will be done in get_user_input
        self.interruption_manager.set_state(ShellState.CORRECT_PENDING)
        self.interruption_manager.show_prompt(
            PromptConfig(
                message=t("shell.error_correction.press_semicolon_hint"),
                window_seconds=60.0,  # Long timeout
            )
        )

        # Store error info for later use when user confirms
        self._pending_error_correction = {
            "command": command,
            "stdout": stdout,
            "stderr": stderr,
            "reason": reason,
        }

    async def _execute_error_correction(self):
        """Execute the error correction flow after user confirmed with 'y'."""
        if not self._pending_error_correction:
            return

        cmd = self._pending_error_correction.get("command", "")
        stdout = self._pending_error_correction.get("stdout", "")
        stderr = self._pending_error_correction.get("stderr", "")
        reason = self._pending_error_correction.get("reason")

        # Clear the pending error info
        self._pending_error_correction = None

        # Get the cmd_error prompt template
        system_message = self.prompt_manager.substitute_template(
            "cmd_error",
            user_nickname=os.getenv("USER", "user"),
            uname_info=self.uname_info,
            os_info=self.os_info,
            basic_env_info=self.basic_env_info,
            output_language=self.output_language,
        )

        prompt = f"""
        <command_result>
        Command: {cmd}
        stdout:
        {stdout}
        stderr:
        {stderr}
        </command_result>"""

        if reason:
            prompt = (
                f"[bold red]Command failed[/bold red] with reason: {reason}\n\n{prompt}"
            )

        try:
            # Wrap the LLM call in a per-operation cancel scope
            with self._safe_cancel_scope() as scope:
                self._current_op_scope = scope
                self.operation_in_progress = True
                # 设置 AI 执行状态
                self.interruption_manager.set_state(ShellState.AI_THINKING)
                # 标志：是否正常完成（非取消）
                completed_normally = True
                try:
                    response = await self.ask_oracle_fast(prompt, system_message)
                    json_cmd = self.try_parse_json_output(response)
                    has_json_block = bool(
                        re.search(
                            r"```json\s*\n(.*?)\n\s*```",
                            response or "",
                            re.DOTALL,
                        )
                    )
                    if not json_cmd or not has_json_block:
                        preview = (
                            response
                            if response and len(response) <= 2000
                            else f"{(response or '')[:2000]}... [truncated]"
                        )
                        self.logger.warning("cmd_error raw response: %s", preview)
                        self.logger.warning(
                            "cmd_error parsed json: %s",
                            json_cmd if json_cmd else "<none>",
                        )
                    await self.process_ai_response(response, border_style="red")
                except anyio.get_cancelled_exc_class():
                    # 被取消
                    completed_normally = False
                    raise
                finally:
                    self._current_op_scope = None
                    self.interruption_manager.set_state(ShellState.NORMAL)
                    # 只有正常完成时才清除输入缓冲区
                    if completed_normally:
                        self.interruption_manager._input_buffer = None
                        self.interruption_manager._restore_input = False

            # 正常完成后清除 operation_in_progress
            self.operation_in_progress = False

        except (KeyboardInterrupt, EOFError):
            self.operation_in_progress = False
            self.handle_processing_cancelled()
            # 恢复正常状态
            self.interruption_manager.set_state(ShellState.NORMAL)

    async def handle_cd_command(self, user_input: str):
        """Handle cd command to change working directory."""
        cwd = os.getcwd()
        result = BuiltinHandlers.handle_cd(user_input, cwd, self.directory_stack)

        # Apply state changes
        if result.new_cwd:
            os.chdir(result.new_cwd)
            # Update PWD environment variable
            os.environ["PWD"] = result.new_cwd

        # Apply environment variable changes
        for key, value in result.env_vars_to_set.items():
            os.environ[key] = value

        # Display output
        if result.success:
            self.console.print(
                result.output, style="green" if result.returncode == 0 else "cyan"
            )
            await self.history_manager.add_entry(
                command=user_input,
                source="user",
                returncode=result.returncode,
                stdout=result.output,
                stderr="",
            )
        else:
            self.console.print(f"❌ {result.error}", style="red")
            if "no such file or directory" in result.error:
                self._suggest_similar_directories(
                    user_input.split()[-1] if len(user_input.split()) > 1 else "~"
                )
            # Trigger error correction for built-in command failures
            await self.handle_command_error(user_input, "", result.error)

        # Add to shell history
        self.add_to_history(
            user_input,
            result.returncode,
            result.output if result.success else "",
            result.error if not result.success else "",
        )

    def _suggest_similar_directories(self, target_dir: str):
        """Suggest similar directory names if the target doesn't exist"""
        try:
            parent_dir = os.path.dirname(target_dir)
            if not parent_dir or not os.path.exists(parent_dir):
                return

            target_name = os.path.basename(target_dir).lower()
            entries = os.listdir(parent_dir)
            directories = [
                e for e in entries if os.path.isdir(os.path.join(parent_dir, e))
            ]

            # Find similar directory names
            suggestions = []
            for dir_name in directories:
                if target_name in dir_name.lower() or dir_name.lower() in target_name:
                    suggestions.append(dir_name)

            if suggestions:
                self.console.print("💡 Did you mean:", style="yellow")
                for suggestion in suggestions[:3]:  # Show up to 3 suggestions
                    full_path = os.path.join(parent_dir, suggestion)
                    self.console.print(f"   cd {full_path}", style="yellow")

        except Exception:
            pass  # Silently ignore suggestion errors

    async def handle_pushd_command(self, user_input: str):
        """Handle pushd command to push directory onto stack."""
        cwd = os.getcwd()
        result = BuiltinHandlers.handle_pushd(user_input, cwd, self.directory_stack)

        # Apply state changes
        if result.new_cwd:
            os.chdir(result.new_cwd)

        # Apply directory stack push (for paths with spaces)
        if result.directory_stack_push:
            self.directory_stack.push(result.directory_stack_push)

        # Apply environment variable changes
        for key, value in result.env_vars_to_set.items():
            os.environ[key] = value

        # Display output
        if result.success:
            self.console.print(result.output, style="green")
            self._show_directory_stack()
            await self.history_manager.add_entry(
                command=user_input,
                source="user",
                returncode=result.returncode,
                stdout=result.output,
                stderr="",
            )
        else:
            self.console.print(f"❌ {result.error}", style="red")
            # Trigger error correction for built-in command failures
            await self.handle_command_error(user_input, "", result.error)

        # Add to shell history
        self.add_to_history(
            user_input,
            result.returncode,
            result.output if result.success else "",
            result.error if not result.success else "",
        )

    async def handle_popd_command(self, user_input: str):
        """Handle popd command to pop directory from stack."""
        cwd = os.getcwd()
        result = BuiltinHandlers.handle_popd(user_input, cwd, self.directory_stack)

        # Apply state changes
        if result.new_cwd:
            os.chdir(result.new_cwd)

        # Apply environment variable changes
        for key, value in result.env_vars_to_set.items():
            os.environ[key] = value

        # Display output
        if result.success:
            self.console.print(result.output, style="green")
            self._show_directory_stack()
            await self.history_manager.add_entry(
                command=user_input,
                source="user",
                returncode=result.returncode,
                stdout=result.output,
                stderr="",
            )
        else:
            self.console.print(f"❌ {result.error}", style="red")
            # Trigger error correction for built-in command failures
            await self.handle_command_error(user_input, "", result.error)

        # Add to shell history
        self.add_to_history(
            user_input,
            result.returncode,
            result.output if result.success else "",
            result.error if not result.success else "",
        )

    async def handle_dirs_command(self, user_input: str):
        """Handle dirs command to show directory stack."""
        cwd = os.getcwd()
        result = BuiltinHandlers.handle_dirs(user_input, cwd, self.directory_stack)

        # Display output
        if result.success:
            self.console.print(result.output, style="cyan")
            await self.history_manager.add_entry(
                command=user_input,
                source="user",
                returncode=result.returncode,
                stdout=result.output,
                stderr="",
            )
        else:
            self.console.print(f"❌ {result.error}", style="red")
            # Trigger error correction for built-in command failures
            await self.handle_command_error(user_input, "", result.error)

        # Add to shell history
        self.add_to_history(
            user_input,
            result.returncode,
            result.output if result.success else "",
            result.error if not result.success else "",
        )

    async def handle_setup_command(self, user_input: str) -> None:
        """Handle /setup command to re-run the interactive setup wizard."""
        try:
            parts = shlex.split(user_input)
        except ValueError:
            parts = [user_input.strip()]

        if len(parts) > 1 and parts[1] in {"--help", "-h"}:
            self.help_manager.show_help("/setup")
            return

        if self.config_manager is None:
            message = t("shell.setup.no_config_manager")
            self.console.print(message, style="red")
            self.add_to_history(user_input, 1, "", message)
            await self.history_manager.add_entry(
                command=user_input,
                source="user",
                returncode=1,
                stdout="",
                stderr=message,
            )
            return

        from aish.wizard.setup_wizard import run_interactive_setup

        new_config = await to_thread.run_sync(
            run_interactive_setup,
            self.config_manager,
        )
        if new_config is None:
            message = t("shell.setup.cancelled")
            self.console.print(message, style="yellow")
            self.add_to_history(user_input, 0, message, "")
            await self.history_manager.add_entry(
                command=user_input,
                source="user",
                returncode=0,
                stdout=message,
                stderr="",
            )
            return

        # Apply the new configuration to the running shell.
        new_model = new_config.model
        new_api_base = new_config.api_base
        new_api_key = new_config.api_key
        new_is_free_key = new_config.is_free_key

        self.llm_session.update_model(
            new_model,
            api_base=new_api_base,
            api_key=new_api_key,
        )
        self.context_manager.set_model(new_model)
        self.config.model = new_model
        self.config.api_base = new_api_base
        self.config.api_key = new_api_key
        self.config.is_free_key = new_is_free_key
        if self.session_record is not None:
            self.session_record.model = new_model

        message = t("shell.setup.applied", model=new_model)
        self.console.print(message, style="green")
        self.add_to_history(user_input, 0, message, "")
        await self.history_manager.add_entry(
            command=user_input,
            source="user",
            returncode=0,
            stdout=message,
            stderr="",
        )

    async def handle_model_command(self, user_input: str) -> None:
        async def report_model_error(message: str) -> None:
            self.console.print(message, style="red")
            self.add_to_history(user_input, 1, "", message)
            await self.history_manager.add_entry(
                command=user_input,
                source="user",
                returncode=1,
                stdout="",
                stderr=message,
            )

        try:
            parts = shlex.split(user_input)
        except ValueError:
            message = t("cli.parse_errors.generic", message="parse error")
            await report_model_error(message)
            return

        if len(parts) == 1:
            current_model = self.config.model or t("shell.model.unset")
            message = t("shell.model.current", model=current_model)
            self.console.print(message)
            self.add_to_history(user_input, 0, message, "")
            await self.history_manager.add_entry(
                command=user_input,
                source="user",
                returncode=0,
                stdout=message,
                stderr="",
            )
            return

        if parts[1] in {"--help", "-h"}:
            self.help_manager.show_help("/model")
            return

        new_model = " ".join(parts[1:]).strip()
        if not new_model:
            message = t("shell.model.invalid")
            await report_model_error(message)
            return

        if new_model == self.config.model:
            message = t("shell.model.switch_same", model=new_model)
            self.console.print(message, style="dim")
            self.add_to_history(user_input, 0, message, "")
            await self.history_manager.add_entry(
                command=user_input,
                source="user",
                returncode=0,
                stdout=message,
                stderr="",
            )
            return

        self.console.print(t("shell.model.switching", model=new_model), style="dim")
        provider = get_provider_for_model(new_model)
        validation_error = await provider.validate_model_switch(
            model=new_model,
            config=self.config,
        )
        if validation_error:
            await report_model_error(validation_error)
            return

        self.llm_session.update_model(
            new_model,
            api_base=self.config.api_base,
            api_key=self.config.api_key,
        )
        self.context_manager.set_model(new_model)
        self.config.model = new_model
        if self.session_record is not None:
            self.session_record.model = new_model
        if self.config_manager is not None:
            self.config_manager.set_model(new_model)

        # Update TUI status bar with new model
        if self._tui_app is not None:
            self._tui_app.update_status(model=new_model)

        message = t("shell.model.switch_success", model=new_model)
        self.console.print(message, style="green")
        self.add_to_history(user_input, 0, message, "")
        await self.history_manager.add_entry(
            command=user_input,
            source="user",
            returncode=0,
            stdout=message,
            stderr="",
        )
        return

    async def handle_history_command(
        self,
        user_input: str,
        record_history: bool = True,
    ) -> tuple[int, str]:
        """Handle history command to show command history with bash-like options"""

        def add_shell_history(
            command: str, returncode: int, stdout: str, stderr: str
        ) -> None:
            if record_history:
                self.add_to_history(command, returncode, stdout, stderr)

        async def add_persistent_history(
            command: str,
            returncode: int,
            stdout: str,
            stderr: str,
        ) -> None:
            if record_history:
                await self.history_manager.add_entry(
                    command=command,
                    source="user",
                    returncode=returncode,
                    stdout=stdout,
                    stderr=stderr,
                )

        try:
            # Parse command arguments
            try:
                parts = shlex.split(user_input)
            except ValueError:
                parts = user_input.split()

            # Default options
            clear_history = False
            delete_offset = None
            limit = None

            # Parse options
            i = 1
            while i < len(parts):
                arg = parts[i]

                if arg == "-c":
                    clear_history = True
                    i += 1
                elif arg == "-d" and i + 1 < len(parts):
                    try:
                        delete_offset = int(parts[i + 1])
                        i += 2
                    except ValueError:
                        error_msg = "history: -d: numeric argument required"
                        self.console.print(f"❌ {error_msg}", style="red")
                        add_shell_history(user_input, 1, "", error_msg)
                        await add_persistent_history(user_input, 1, "", error_msg)
                        # Trigger error correction for built-in command failures
                        await self.handle_command_error(user_input, "", error_msg)
                        return 1, error_msg
                elif arg.startswith("-"):
                    # Unknown option
                    error_msg = f"history: {arg}: invalid option"
                    self.console.print(f"❌ {error_msg}", style="red")
                    add_shell_history(user_input, 1, "", error_msg)
                    await add_persistent_history(user_input, 1, "", error_msg)
                    # Trigger error correction for built-in command failures
                    await self.handle_command_error(user_input, "", error_msg)
                    return 1, error_msg
                else:
                    # This should be a number (limit)
                    try:
                        limit = int(arg)
                        i += 1
                    except ValueError:
                        error_msg = f"history: {arg}: numeric argument required"
                        self.console.print(f"❌ {error_msg}", style="red")
                        add_shell_history(user_input, 1, "", error_msg)
                        await add_persistent_history(user_input, 1, "", error_msg)
                        # Trigger error correction for built-in command failures
                        await self.handle_command_error(user_input, "", error_msg)
                        return 1, error_msg

            # Handle clear history (clear current session only)
            if clear_history:
                current_session_id = self.history_manager.get_session_uuid()
                if await self.history_manager.delete_session(current_session_id):
                    self.console.print(
                        "📚 Current session history cleared", style="green"
                    )
                    add_shell_history(
                        user_input, 0, "Current session history cleared", ""
                    )
                    await add_persistent_history(
                        user_input,
                        0,
                        "Current session history cleared",
                        "",
                    )
                else:
                    error_msg = "history: failed to clear history"
                    self.console.print(f"❌ {error_msg}", style="red")
                    add_shell_history(user_input, 1, "", error_msg)
                    await add_persistent_history(user_input, 1, "", error_msg)
                    # Trigger error correction for built-in command failures
                    await self.handle_command_error(user_input, "", error_msg)
                    return 1, error_msg
                return 0, ""

            # Handle delete entry by index (1-based, from oldest to newest)
            if delete_offset is not None:
                # Get current session history count
                current_session_id = self.history_manager.get_session_uuid()
                all_entries = await self.history_manager.get_history(
                    limit=None,
                    session_uuid=current_session_id,
                )

                # Debug: show total count
                self.console.print(
                    f"[Debug] Total entries: {len(all_entries)}, deleting: {delete_offset}",
                    style="dim",
                )

                # If offset exceeds range, delete the last entry
                if delete_offset > len(all_entries):
                    delete_offset = len(all_entries)
                elif delete_offset < 1:
                    delete_offset = 1

                if await self.history_manager.delete_entry_by_index(delete_offset):
                    self.console.print(
                        f"📚 Deleted history entry {delete_offset}", style="green"
                    )
                    add_shell_history(
                        user_input, 0, f"Deleted entry {delete_offset}", ""
                    )
                    await add_persistent_history(
                        user_input,
                        0,
                        f"Deleted entry {delete_offset}",
                        "",
                    )
                else:
                    error_msg = (
                        f"history: {delete_offset}: history position out of range"
                    )
                    self.console.print(f"❌ {error_msg}", style="red")
                    add_shell_history(user_input, 1, "", error_msg)
                    await add_persistent_history(user_input, 1, "", error_msg)
                    # Trigger error correction for built-in command failures
                    await self.handle_command_error(user_input, "", error_msg)
                    return 1, error_msg
                return 0, ""

            # Get history from the history manager (current session only by default)
            current_session_id = self.history_manager.get_session_uuid()
            history_entries = await self.history_manager.get_history(
                limit=limit,
                session_uuid=current_session_id,
            )

            if not history_entries:
                self.console.print(
                    "📚 No command history found for current session", style="yellow"
                )
                add_shell_history(user_input, 0, "No command history found", "")
                await add_persistent_history(
                    user_input, 0, "No command history found", ""
                )
                return 0, ""

            # Display history with enhanced formatting
            # Reverse so newest entries appear at the bottom (like bash history)
            history_entries = list(reversed(history_entries))
            self.console.print(
                f"📚 Command History (Session: {current_session_id[:8]}...):",
                style="cyan",
            )
            for i, entry in enumerate(history_entries, 1):
                display_line = entry.to_display_string()
                self.console.print(f"  {i:4d}  {display_line}", style="cyan")

            history_info = (
                f"Showing {len(history_entries)} commands from current session"
            )
            add_shell_history(user_input, 0, history_info, "")
            await add_persistent_history(user_input, 0, history_info, "")
            return 0, ""

        except Exception as e:
            error_msg = f"history: error: {e}"
            self.console.print(f"❌ {error_msg}", style="red")
            add_shell_history(user_input, 1, "", error_msg)
            await add_persistent_history(user_input, 1, "", error_msg)
            # Trigger error correction for built-in command failures
            await self.handle_command_error(user_input, "", error_msg)
            return 1, error_msg

    async def handle_export_command(self, user_input: str):
        """Handle export command with bash-compatible options"""
        try:
            # Parse command arguments with options support
            parts = shlex.split(user_input)

            # Parse options
            show_all = False
            remove_export = False
            export_function = False
            var_assignments = []
            disable_options = False

            i = 1
            while i < len(parts):
                arg = parts[i]

                if arg == "--":
                    # Disable further option processing
                    disable_options = True
                    i += 1
                    break
                elif arg == "-p":
                    show_all = True
                    i += 1
                elif arg == "-n":
                    remove_export = True
                    i += 1
                elif arg == "-f":
                    export_function = True
                    i += 1
                elif arg.startswith("-") and not disable_options:
                    # Unknown option
                    error_msg = f"export: invalid option -- '{arg[1:]}'"
                    self.console.print(f"❌ {error_msg}", style="red")
                    self.add_to_history(user_input, 1, "", error_msg)
                    await self.history_manager.add_entry(
                        command=user_input,
                        source="user",
                        returncode=1,
                        stdout="",
                        stderr=error_msg,
                    )
                    # Trigger error correction for built-in command failures
                    await self.handle_command_error(user_input, "", error_msg)
                    return
                else:
                    # Variable assignment or name
                    var_assignments.append(arg)
                    i += 1

            # Handle remaining arguments (after --)
            while i < len(parts):
                var_assignments.append(parts[i])
                i += 1

            # Handle function export (not supported)
            if export_function:
                self.console.print(
                    "⚠️  Function export is not supported in AI Shell", style="yellow"
                )
                self.add_to_history(user_input, 0, "Function export not supported", "")
                await self.history_manager.add_entry(
                    command=user_input,
                    source="user",
                    returncode=0,
                    stdout="Function export not supported",
                    stderr="",
                )
                return

            # Display exported variables
            if show_all or (len(parts) == 1):
                self._display_exported_env_vars()
                self.add_to_history(
                    user_input, 0, "Displayed exported environment variables", ""
                )
                await self.history_manager.add_entry(
                    command=user_input,
                    source="user",
                    returncode=0,
                    stdout="Displayed exported environment variables",
                    stderr="",
                )
                return

            # Handle removing export attribute
            if remove_export:
                if not var_assignments:
                    error_msg = "export: -n: option requires an argument"
                    self.console.print(f"❌ {error_msg}", style="red")
                    self.add_to_history(user_input, 1, "", error_msg)
                    await self.history_manager.add_entry(
                        command=user_input,
                        source="user",
                        returncode=1,
                        stdout="",
                        stderr=error_msg,
                    )
                    return

                success_count = 0
                for var_name in var_assignments:
                    if self.env_manager.remove_export(var_name):
                        success_count += 1
                        self.console.print(
                            f"✅ Removed export attribute from {var_name}",
                            style="green",
                        )
                    else:
                        self.console.print(
                            f"ℹ️  Variable {var_name} not found", style="yellow"
                        )

                self.add_to_history(
                    user_input, 0, f"Removed export from {success_count} variables", ""
                )
                await self.history_manager.add_entry(
                    command=user_input,
                    source="user",
                    returncode=0,
                    stdout=f"Removed export from {success_count} variables",
                    stderr="",
                )
                return

            # Handle variable assignments
            if not var_assignments:
                # No arguments, display exported variables
                self._display_exported_env_vars()
                self.add_to_history(
                    user_input, 0, "Displayed exported environment variables", ""
                )
                await self.history_manager.add_entry(
                    command=user_input,
                    source="user",
                    returncode=0,
                    stdout="Displayed exported environment variables",
                    stderr="",
                )
                return

            # Parse variable assignments
            success_count = 0
            for assignment in var_assignments:
                if "=" in assignment:
                    # VAR=value format
                    key, value = assignment.split("=", 1)
                    key = key.strip()
                    value = value.strip()

                    # Remove possible quotes
                    if (value.startswith('"') and value.endswith('"')) or (
                        value.startswith("'") and value.endswith("'")
                    ):
                        value = value[1:-1]

                    # Set and export environment variable
                    if self.env_manager.set_var(key, value, export=True):
                        success_count += 1
                        self.console.print(
                            f"✅ Set and exported {key}={value}", style="green"
                        )
                    else:
                        self.console.print(f"❌ Failed to set {key}", style="red")
                else:
                    # Only variable name, mark as exported
                    if self.env_manager.get_var(assignment):
                        self.env_manager._exported_vars.add(assignment)
                        success_count += 1
                        self.console.print(f"✅ Exported {assignment}", style="green")
                    else:
                        self.console.print(
                            f"ℹ️  Variable {assignment} not found, creating with empty value",
                            style="yellow",
                        )
                        self.env_manager.set_var(assignment, "", export=True)
                        success_count += 1

            self.add_to_history(
                user_input, 0, f"Processed {success_count} variable assignments", ""
            )
            await self.history_manager.add_entry(
                command=user_input,
                source="user",
                returncode=0,
                stdout=f"Processed {success_count} variable assignments",
                stderr="",
            )

        except Exception as e:
            error_msg = f"export: error: {e}"
            self.console.print(f"❌ {error_msg}", style="red")
            self.add_to_history(user_input, 1, "", error_msg)
            await self.history_manager.add_entry(
                command=user_input,
                source="user",
                returncode=1,
                stdout="",
                stderr=error_msg,
            )

    def _display_exported_env_vars(self):
        """Display all exported environment variables"""
        exported_vars = self.env_manager.get_exported_vars()
        if not exported_vars:
            self.console.print("No exported environment variables", style="yellow")
            return

        self.console.print("Exported environment variables:", style="cyan")
        for key, value in sorted(exported_vars.items()):
            # Truncate long values for display
            display_value = value
            if len(value) > 100:
                display_value = value[:100] + "..."
            # Use bash declare -x format
            self.console.print(f'declare -x {key}="{display_value}"', style="white")

    async def handle_unset_command(self, user_input: str):
        """Handle unset command with bash-compatible options"""
        try:
            parts = shlex.split(user_input)

            # Parse options
            unset_func = False
            unset_ref = False
            var_names = []
            disable_options = False

            i = 1
            while i < len(parts):
                arg = parts[i]

                if arg == "--":
                    # Disable further option processing
                    disable_options = True
                    i += 1
                    break
                elif arg == "-v":
                    i += 1
                elif arg == "-f":
                    unset_func = True
                    i += 1
                elif arg == "-n":
                    unset_ref = True
                    i += 1
                elif arg.startswith("-") and not disable_options:
                    # Unknown option
                    error_msg = f"unset: invalid option -- '{arg[1:]}'"
                    self.console.print(f"❌ {error_msg}", style="red")
                    self.add_to_history(user_input, 1, "", error_msg)
                    await self.history_manager.add_entry(
                        command=user_input,
                        source="user",
                        returncode=1,
                        stdout="",
                        stderr=error_msg,
                    )
                    # Trigger error correction for built-in command failures
                    await self.handle_command_error(user_input, "", error_msg)
                    return
                else:
                    # Variable name
                    var_names.append(arg)
                    i += 1

            # Handle remaining arguments (after --)
            while i < len(parts):
                var_names.append(parts[i])
                i += 1

            # Check for variable names
            if not var_names:
                error_msg = "unset: usage: unset [-v] [-f] [-n] [name ...]"
                self.console.print(f"❌ {error_msg}", style="red")
                self.add_to_history(user_input, 1, "", error_msg)
                await self.history_manager.add_entry(
                    command=user_input,
                    source="user",
                    returncode=1,
                    stdout="",
                    stderr=error_msg,
                )
                return

            # Handle function unset (not supported)
            if unset_func:
                self.console.print(
                    "⚠️  Function unset is not supported in AI Shell", style="yellow"
                )
                self.add_to_history(user_input, 0, "Function unset not supported", "")
                await self.history_manager.add_entry(
                    command=user_input,
                    source="user",
                    returncode=0,
                    stdout="Function unset not supported",
                    stderr="",
                )
                return

            # Handle name reference unset (not supported)
            if unset_ref:
                self.console.print(
                    "⚠️  Name reference unset is not supported in AI Shell",
                    style="yellow",
                )
                self.add_to_history(
                    user_input, 0, "Name reference unset not supported", ""
                )
                await self.history_manager.add_entry(
                    command=user_input,
                    source="user",
                    returncode=0,
                    stdout="Name reference unset not supported",
                    stderr="",
                )
                return

            # Unset environment variables (default behavior)
            success_count = 0
            not_found_count = 0

            for var_name in var_names:
                if self.env_manager.unset_var(var_name):
                    success_count += 1
                    self.console.print(f"✅ Unset {var_name}", style="green")
                else:
                    not_found_count += 1
                    # Variable doesn't exist, but not considered an error (consistent with bash)
                    self.console.print(
                        f"ℹ️  Variable {var_name} not found", style="yellow"
                    )

            summary = f"Unset {success_count} variables"
            if not_found_count > 0:
                summary += f", {not_found_count} not found"

            self.add_to_history(user_input, 0, summary, "")
            await self.history_manager.add_entry(
                command=user_input,
                source="user",
                returncode=0,
                stdout=summary,
                stderr="",
            )

        except Exception as e:
            error_msg = f"unset: error: {e}"
            self.console.print(f"❌ {error_msg}", style="red")
            self.add_to_history(user_input, 1, "", error_msg)
            await self.history_manager.add_entry(
                command=user_input,
                source="user",
                returncode=1,
                stdout="",
                stderr=error_msg,
            )

    async def handle_pwd_command(self, user_input: str):
        """Handle pwd command to show current working directory with proper symlink handling"""
        try:
            parts = shlex.split(user_input)
        except ValueError:
            parts = user_input.split()

        # Parse options
        show_logical = False
        show_physical = False
        i = 1
        while i < len(parts):
            arg = parts[i]
            if arg == "-L":
                show_logical = True
                i += 1
            elif arg == "-P":
                show_physical = True
                i += 1
            elif arg.startswith("-"):
                # Unknown option
                error_msg = f"pwd: invalid option -- '{arg[1:]}'\nUsage: pwd [-LP]"
                self.console.print(f"❌ {error_msg}", style="red")
                self.add_to_history(user_input, 1, "", error_msg)
                await self.history_manager.add_entry(
                    command=user_input,
                    source="user",
                    returncode=1,
                    stdout="",
                    stderr=error_msg,
                )
                # Trigger error correction for built-in command failures
                await self.handle_command_error(user_input, "", error_msg)
                return
            else:
                break

        # Default behavior: show logical path (like bash)
        if not show_physical:
            show_logical = True

        # Get the appropriate path
        if show_logical:
            # Show logical path (respecting symlinks)
            current_path = os.environ.get("PWD", os.getcwd())
        else:
            # Show physical path (resolving symlinks)
            current_path = os.getcwd()

        self.console.print(current_path)

        # Add to local history manager
        await self.history_manager.add_entry(
            command=user_input,
            source="user",
            returncode=0,
            stdout=current_path,
            stderr="",
        )

        self.add_to_history(user_input, 0, f"Current directory: {current_path}", "")

    def _show_directory_stack(self):
        """Show the current directory stack"""
        current = os.getcwd()
        stack_display = [f"📁 {os.path.basename(current)} (current)"]

        for i, dir_path in enumerate(reversed(self.directory_stack)):
            stack_display.append(
                f"  {len(self.directory_stack) - i}: {os.path.basename(dir_path)}"
            )

        if len(self.directory_stack) == 0:
            self.console.print(
                "🗂️  Directory stack: only current directory", style="cyan"
            )
        else:
            self.console.print("🗂️  Directory stack:", style="cyan")
            for line in stack_display:
                self.console.print(line, style="cyan")

    def try_parse_json_output(self, response: str) -> Optional[dict]:
        """
        Detect and extract JSON command from AI response.
        Returns the JSON command dict if found, None otherwise.
        """
        try:
            # Look for JSON blocks in the response using regex
            json_pattern = r"```json\s*\n(.*?)\n\s*```"
            json_matches = re.findall(json_pattern, response, re.DOTALL)

            for json_str in json_matches:
                try:
                    json_data = json.loads(json_str.strip())
                    return json_data
                except json.JSONDecodeError:
                    continue

            # If no code block found, try to find standalone JSON
            # Look for lines that could be JSON
            lines = response.split("\n")
            for line in lines:
                line = line.strip()
                if line.startswith("{") and line.endswith("}"):
                    try:
                        json_data = json.loads(line)
                        return json_data
                    except json.JSONDecodeError:
                        continue

            return None
        except Exception:
            return None

    async def handle_json_command(self, json_cmd: dict):
        # "<Interrupted received.>"
        if self.interruption_manager.state in (
            ShellState.AI_THINKING,
            ShellState.SANDBOX_EVAL,
            ShellState.COMMAND_EXEC,
        ):
            self.interruption_manager.set_state(ShellState.NORMAL)

        if json_cmd.get("type") == "long_running_command":
            command = json_cmd.get("command", "")
            self.console.print(f"🚀 {command}", style="bold cyan")
            return

        elif json_cmd.get("type") == "corrected_command":
            command = json_cmd.get("command", "")
            description = json_cmd.get("description", "")

            self.console.print(
                f"💡 [bold cyan]{t('shell.error_correction.corrected_command_label')}[/bold cyan] {command}"
            )
            self.console.print(f"[grey50]({description})[/grey50]")

            if not command or not command.strip():
                return

            from prompt_toolkit.key_binding import KeyBindings

            kb = KeyBindings()

            @kb.add("y")
            @kb.add("Y")
            def _(event):
                """用户按 Y/y 直接确认执行"""
                event.app.exit(result="y")

            try:
                confirm = (
                    (
                        await self.session.prompt_async(
                            t("shell.prompt.confirm_execute"),
                            handle_sigint=False,
                            key_bindings=kb,
                        )
                    )
                    .strip()
                    .lower()
                )

                if confirm in ["y", "yes", "是", "是的"]:
                    self._remember_approved_command(command)

                    with self._safe_cancel_scope() as scope:
                        self._current_op_scope = scope
                        self.operation_in_progress = True
                        try:
                            result = await self.execute_command_with_security(
                                command, record_history=False
                            )
                            returncode, stdout, stderr = result.to_tuple()
                            self.add_to_history(
                                command,
                                returncode,
                                stdout,
                                stderr,
                                offload=result.offload,
                            )
                            await self.history_manager.add_entry(
                                command=command,
                                source="ai",
                                returncode=returncode,
                                stdout=stdout,
                                stderr=stderr,
                            )
                            if returncode != 0:
                                await self.handle_command_error(command, stdout, stderr)
                        finally:
                            self.operation_in_progress = False
                            self._current_op_scope = None

            except KeyboardInterrupt:
                raise
            except EOFError:
                pass

            return

        return

    def print_help(self):
        """Print help information"""
        self.help_manager.show_general_help()

    def _get_shell_preview_bytes(self) -> int:
        offload_cfg = getattr(self.config, "bash_output_offload", None)
        preview_raw = getattr(offload_cfg, "preview_bytes", 1024)
        try:
            preview_bytes = int(preview_raw)
        except (TypeError, ValueError):
            preview_bytes = 1024
        if preview_bytes <= 0:
            preview_bytes = 1024
        return preview_bytes

    def _truncate_utf8_preview(self, text: str, limit_bytes: int) -> tuple[str, bool]:
        if not text:
            return "", False
        if limit_bytes <= 0:
            return "", True
        raw = text.encode("utf-8")
        if len(raw) <= limit_bytes:
            return text, False
        return raw[:limit_bytes].decode("utf-8", errors="ignore"), True

    def _normalize_shell_offload_payload(
        self, offload: dict[str, Any] | None
    ) -> dict[str, Any]:
        payload: dict[str, Any] = dict(offload) if isinstance(offload, dict) else {}
        status = str(payload.get("status", "inline") or "inline")
        payload["status"] = status
        if status == "inline" and not payload.get("reason"):
            payload["reason"] = "not_offloaded"
        return payload

    def add_to_history(
        self,
        command: str,
        returncode: int,
        stdout: str,
        stderr: str,
        offload: dict[str, Any] | None = None,
    ):
        """Add shell execution context for LLM with output previews and offload hints."""
        # Update last exit code for prompt hooks
        self._last_exit_code = returncode

        preview_bytes = self._get_shell_preview_bytes()
        stdout_preview, stdout_truncated = self._truncate_utf8_preview(
            stdout or "",
            preview_bytes,
        )
        stderr_preview, stderr_truncated = self._truncate_utf8_preview(
            stderr or "",
            preview_bytes,
        )

        if not stdout_preview:
            stdout_preview = ""
        if not stderr_preview:
            stderr_preview = ""

        if stdout_truncated:
            stdout_preview += (
                f"\n... [stdout preview truncated to {preview_bytes} bytes]"
            )
        if stderr_truncated:
            stderr_preview += (
                f"\n... [stderr preview truncated to {preview_bytes} bytes]"
            )

        status_symbol = "✓" if returncode == 0 else "✗"
        summary = f"$ {command} → {status_symbol} (exit {returncode})"

        offload_payload = self._normalize_shell_offload_payload(offload)
        offload_json = json.dumps(
            offload_payload, ensure_ascii=False, separators=(",", ":")
        )

        history_entry = "\n".join(
            [
                summary,
                "<stdout>",
                stdout_preview,
                "</stdout>",
                "<stderr>",
                stderr_preview,
                "</stderr>",
                "<return_code>",
                str(returncode),
                "</return_code>",
                "<offload>",
                offload_json,
                "</offload>",
            ]
        )
        self.context_manager.add_memory(MemoryType.SHELL, history_entry)

    def _get_heredoc_delimiter(self, command: str) -> Optional[str]:
        """Extract heredoc delimiter if the command contains a heredoc.

        Examples:
          - "cat << EOF" -> "EOF"
          - "cat << 'EOF'" -> "EOF"
          - "cat << \\EOF" -> "EOF"
        """
        if not command or "<<" not in command:
            return None

        try:
            parts = shlex.split(command)
        except Exception:
            return None

        for i, token in enumerate(parts):
            if token in ("<<", "<<-"):
                if i + 1 >= len(parts):
                    return None
                delimiter = parts[i + 1]
                if delimiter.startswith("\\"):
                    delimiter = delimiter.lstrip("\\")
                return delimiter or None

        return None

    def _has_shell_operators(self, user_input: str) -> bool:
        """Check if input contains shell operators that suggest it's a command"""
        shell_operators = [
            "&&",
            "||",
            "|",
            ">",
            ">>",
            "<",
            "<<",
            "<<<",
            "&",
            ";",
            ";;",
            ";&",
            ";;&",
            "|&",
            ">&",
            "<&",
            "2>",
            "2>>",
            "1>",
            "1>>",
            "&>",
            "&>>",
        ]

        # Check for any shell operators
        for operator in shell_operators:
            if operator in user_input:
                return True

        # Check for command substitution
        if ("$(" in user_input and ")" in user_input) or "`" in user_input:
            return True

        # Check for process substitution
        if ("<(" in user_input and ")" in user_input) or (
            ">(" in user_input and ")" in user_input
        ):
            return True

        return False

    async def is_command_request(self, user_input: str, cmd_parts: list[str]) -> bool:
        """Check if the user input is a command request.

        Always returns True for non-empty input - execute everything as a command.
        Error handling will offer AI correction if needed.
        """
        if self.starts_with_question_mark(user_input):
            return False

        # Guard against empty cmd_parts
        if not cmd_parts:
            return False

        # Always treat as command - let execution handle errors
        return True

    async def process_input(self, user_input: str):
        """Process user input and execute appropriate action"""
        try:
            self.processing_input = True

            # Reset per-input LLM failure flags
            self._command_detection_llm_failed = False

            # Reset cancellation token for new user input
            self.llm_session.reset_cancellation_token()
            # Keep compatibility with historical /model special command entry.
            try:
                cmd_parts = shlex.split(user_input)
            except ValueError:
                cmd_parts = None
            if cmd_parts and cmd_parts[0] == "/model":
                await self.handle_model_command(user_input)
                return
            if cmd_parts and cmd_parts[0] == "/setup":
                await self.handle_setup_command(user_input)
                return
            route = self._input_router.route(user_input)
            stripped_input = user_input.strip()
            if route.intent == InputIntent.EMPTY:
                return

            # Lazy skill reload: only reload after invalidation, at the next safe point.
            try:
                self.skill_manager.reload_if_dirty()
            except Exception:
                pass

            # Lazy script reload: reload scripts if invalidated
            if hasattr(self, "script_registry"):
                try:
                    self.script_registry.reload_if_dirty()
                except Exception:
                    pass

            ctx = ActionContext(
                raw_input=user_input,
                stripped_input=stripped_input,
                route_data=route.as_dict(),
            )
            action = self._actions.get(route.intent)
            if action is None:
                action = self._actions[InputIntent.COMMAND_OR_AI]

            outcome = await action.execute(ctx)
            if not outcome.handled:
                await self._actions[InputIntent.COMMAND_OR_AI].execute(ctx)

        except anyio.get_cancelled_exc_class():
            self.handle_processing_cancelled()

        finally:
            self.processing_input = False

    # Animation utility methods
    def _get_spinner_patterns(self) -> dict[str, list[str]]:
        """Get different spinner animation patterns"""
        return {
            "braille": ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"],
            "dots": ["⠁", "⠂", "⠄", "⡀", "⢀", "⠠", "⠐", "⠈"],
            "thinking": ["🤔", "💭", "🧠", "⚡", "✨", "🔍", "💡", "🎯"],
            "progress": ["●○○○", "○●○○", "○○●○", "○○○●", "○○●○", "○●○○"],
            "arrows": ["←", "↖", "↑", "↗", "→", "↘", "↓", "↙"],
            "clock": [
                "🕐",
                "🕑",
                "🕒",
                "🕓",
                "🕔",
                "🕕",
                "🕖",
                "🕗",
                "🕘",
                "🕙",
                "🕚",
                "🕛",
            ],
        }

    def _get_current_spinner_char(self, pattern_name: str = "braille") -> str:
        """Get current character from spinner pattern"""
        patterns = self._get_spinner_patterns()
        pattern = patterns.get(pattern_name, patterns["braille"])
        return pattern[self.animation_counter % len(pattern)]

    def _reset_reasoning_state(self) -> None:
        self._reasoning_display_enabled = False
        self._reasoning_active = False
        self._reasoning_partial = ""
        self._reasoning_lines = []
        self._last_reasoning_render_lines = []

    def _finalize_content_preview(self) -> None:
        if not self._content_preview_active:
            return
        self.console.print()
        self._content_preview_active = False
        self._at_line_start = True

    def _append_reasoning_delta(self, delta: str) -> list[str]:
        normalized = delta.replace("\r\n", "\n").replace("\r", "\n")
        parts = normalized.split("\n")
        if not parts:
            return []

        self._reasoning_partial += parts[0]
        for segment in parts[1:]:
            self._reasoning_lines.append(self._reasoning_partial)
            self._reasoning_partial = segment
            if len(self._reasoning_lines) > self._reasoning_max_lines:
                self._reasoning_lines = self._reasoning_lines[
                    -self._reasoning_max_lines :
                ]

        all_lines = list(self._reasoning_lines)
        if self._reasoning_partial:
            all_lines.append(self._reasoning_partial)

        if not all_lines:
            return []

        return all_lines[-self._reasoning_max_lines :]

    def _update_reasoning_live(self, lines: list[str]) -> None:
        self._check_terminal_resize()
        if not self.current_live:
            self.current_live = Live(console=self.console, auto_refresh=False)
            self.current_live.start()

        self._last_reasoning_render_lines = list(lines)
        with self.animation_lock:
            spinner_char = self._get_current_spinner_char("dots")
            self.animation_counter += 1

        max_width = max(10, self.console.size.width - 4)
        trimmed_lines = []
        for line in lines:
            if len(line) > max_width:
                trimmed_lines.append(line[-max_width:])
            else:
                trimmed_lines.append(line)

        if trimmed_lines:
            thinking_label = t("shell.status.thinking")
            display_text = "\n".join(
                [f"{spinner_char} {thinking_label}", *trimmed_lines]
            )
        else:
            thinking_label = t("shell.status.thinking")
            display_text = f"{spinner_char} {thinking_label}..."

        self.current_live.update(Text(display_text, style="grey50"), refresh=True)

    def _animate_thinking(self) -> None:
        """Background animation loop for thinking indicators"""
        while self.animation_active:
            try:
                self._check_terminal_resize()
                with self.animation_lock:
                    if not self.animation_active or not self.current_live:
                        break

                    current_live = self.current_live
                    pattern = self._animation_pattern
                    base_text = self._animation_base_text
                    update_text = self._animation_update_text

                    spinner_char = self._get_current_spinner_char(pattern)
                    self.animation_counter += 1

                if update_text:
                    display_text = f"{spinner_char} {update_text}"
                else:
                    display_text = f"{spinner_char} {base_text}..."

                current_live.update(Text(display_text, style="blue"), refresh=True)

                time.sleep(0.15)  # 150ms for smooth animation
            except Exception:
                # Silently handle animation errors to avoid disrupting main flow
                break

    def _start_animation(
        self,
        base_text: Optional[str] = None,
        pattern: str = "braille",
        update_text: Optional[str] = None,
    ) -> None:
        """Start the background animation thread"""
        with self.animation_lock:
            self._animation_base_text = base_text or t("shell.status.thinking")
            self._animation_pattern = pattern
            self._animation_update_text = update_text

            if self.animation_thread and self.animation_thread.is_alive():
                self.animation_active = True
                return

            self.animation_active = True
            self.animation_counter = 0

        self.animation_thread = threading.Thread(
            target=self._animate_thinking,
            daemon=True,
        )
        self.animation_thread.start()

    def _stop_animation(self) -> None:
        """Stop the background animation thread with robust cleanup"""
        try:
            with self.animation_lock:
                self.animation_active = False

            if self.animation_thread and self.animation_thread.is_alive():
                # Give thread time to stop gracefully
                self.animation_thread.join(timeout=0.5)

                # If thread is still alive, it will be cleaned up by daemon=True
                if self.animation_thread.is_alive():
                    # Thread will be terminated when main thread exits
                    pass
        except Exception:
            # Silently handle any cleanup errors to prevent cascading issues
            pass
        finally:
            # Always reset thread reference
            self.animation_thread = None

    # Event handlers for LLM interactions
    def handle_llm_event(self, event: LLMEvent):
        """Main event handler that routes events to specific handlers"""
        return self._llm_event_router.handle(event)

    def _truncate_log_value(self, value: object, max_len: int = 200) -> str:
        text = str(value)
        if len(text) <= max_len:
            return text
        return f"{text[:max_len]}..."

    def _summarize_llm_event(self, event: LLMEvent) -> dict[str, object]:
        data = event.data or {}
        summary: dict[str, object] = {}

        if event.event_type == LLMEventType.OP_START:
            summary["operation"] = data.get("operation")
            summary["turn_id"] = data.get("turn_id")
            summary["stream"] = data.get("stream")
        elif event.event_type == LLMEventType.OP_END:
            summary["operation"] = data.get("operation")
            summary["turn_id"] = data.get("turn_id")
            summary["cancelled"] = data.get("cancelled")
            if data.get("cancelled_reason") is not None:
                summary["cancelled_reason"] = self._truncate_log_value(
                    data.get("cancelled_reason")
                )
        elif event.event_type == LLMEventType.GENERATION_START:
            summary["turn_id"] = data.get("turn_id")
            summary["generation_id"] = data.get("generation_id")
            summary["generation_type"] = data.get("generation_type")
            summary["stream"] = data.get("stream")
        elif event.event_type == LLMEventType.GENERATION_END:
            summary["turn_id"] = data.get("turn_id")
            summary["generation_id"] = data.get("generation_id")
            summary["status"] = data.get("status")
            if data.get("finish_reason") is not None:
                summary["finish_reason"] = data.get("finish_reason")
            if data.get("error_message") is not None:
                summary["error_message"] = self._truncate_log_value(
                    data.get("error_message")
                )
        elif event.event_type in (
            LLMEventType.REASONING_START,
            LLMEventType.REASONING_END,
        ):
            summary["turn_id"] = data.get("turn_id")
            summary["generation_id"] = data.get("generation_id")

        source = data.get("source")
        if source is not None:
            summary["source"] = source

        return summary

    def _log_llm_event_debug(self, event: LLMEvent) -> None:
        summary = self._summarize_llm_event(event)
        self.logger.debug("LLM %s: %s", event.event_type.value, summary)

    def handle_operation_start(self, event: LLMEvent):
        """Handle operation start event.

        NOTE: UI feedback is driven by GENERATION_START/END. OP_START is useful for
        external UIs to correlate events across a whole turn.
        """
        self._log_llm_event_debug(event)
        return None

    def handle_generation_start(self, event: LLMEvent):
        """Handle one model request start (user-visible progress)."""
        self._log_llm_event_debug(event)
        self._finalize_content_preview()
        self._reset_reasoning_state()
        self._last_streaming_accumulated = ""
        self._reasoning_display_enabled = True
        return self.handle_thinking_start(event)

    def handle_generation_end(self, event: LLMEvent):
        """Handle one model request end (stop progress UI)."""
        self._log_llm_event_debug(event)
        self._stop_animation()
        self._reset_reasoning_state()
        self._last_streaming_accumulated = ""
        self._finalize_content_preview()
        if self.current_live:
            self.current_live.update("", refresh=True)
            self.current_live.stop()
            self.current_live = None
        return None

    def handle_reasoning_start(self, event: LLMEvent):
        """Handle provider reasoning stream start."""
        if not self._reasoning_display_enabled:
            return None

        self._finalize_content_preview()
        self._stop_animation()
        self._reasoning_active = True
        self._last_streaming_accumulated = ""
        self._reasoning_partial = ""
        self._reasoning_lines = []
        self._update_reasoning_live([])

        self._log_llm_event_debug(event)
        return None

    def handle_reasoning_delta(self, event: LLMEvent):
        """Handle provider reasoning deltas with a scrolling preview."""
        if not self._reasoning_display_enabled:
            return None

        delta = str(event.data.get("delta") or "")
        if not delta:
            return None

        if not self._reasoning_active:
            self.handle_reasoning_start(event)

        lines = self._append_reasoning_delta(delta)
        self._update_reasoning_live(lines)
        return None

    def handle_reasoning_end(self, event: LLMEvent):
        """Handle provider reasoning stream end."""
        self._reasoning_active = False
        self._log_llm_event_debug(event)
        return None

    def handle_content_delta(self, event: LLMEvent):
        """Handle streamed/synthesized assistant text chunks."""
        # Stop animation on the first emitted content (final or not) to avoid
        # competing Live updates.
        self._stop_animation()
        self._reset_reasoning_state()
        self._last_streaming_accumulated = ""

        if self.current_live:
            self.current_live.update("", refresh=True)
            self.current_live.stop()
            self.current_live = None

        content = event.data.get("delta") or event.data.get("accumulated") or ""
        content = str(content)
        if not content:
            return None

        if not self._content_preview_active:
            self._content_preview_active = True
            content = f"🤖 {content}"

        self.console.print(Text(content, style="bold grey50"), end="")
        self._at_line_start = False
        return None

    def handle_operation_end(self, event: LLMEvent):
        """Handle operation end event - clean up any UI state."""
        self._log_llm_event_debug(event)

        self._stop_animation()
        self._reset_reasoning_state()
        self._last_streaming_accumulated = ""
        self._finalize_content_preview()
        return self.handle_completion_done(event)

    def handle_thinking_start(self, event: LLMEvent):
        """Handle thinking start event - show enhanced rotating animation"""
        # Stop any existing animation
        self._stop_animation()
        self._last_streaming_accumulated = ""
        self._last_reasoning_render_lines = []

        if self.current_live:
            self.current_live.stop()

        # Create new Live display for thinking animation
        self.current_live = Live(console=self.console, auto_refresh=False)
        self.current_live.start()

        # Start enhanced rotating animation
        # Use different patterns based on thinking stage
        pattern = "braille"  # Use braille patterns for thinking

        # Check if this is from system_diagnose_agent and add indentation
        base_text = t("shell.status.thinking_now")
        if event.data and event.data.get("source") == "system_diagnose_agent":
            base_text = t("shell.status.system_diagnosing")
            pattern = "dots"  # Use different pattern for agent

        self._start_animation(base_text=base_text, pattern=pattern)

    def handle_thinking_update(self, event: LLMEvent):
        """Handle thinking update event with enhanced rotating animation"""
        if self.current_live and self.animation_active:
            update_text = event.data.get("update_text")
            if not update_text:
                return None

            # Check if this is from system_diagnose_agent and adjust formatting
            base_text = t("shell.status.processing")
            if event.data and event.data.get("source") == "system_diagnose_agent":
                base_text = t("shell.status.system_analyzing")
                update_text = (
                    f"  {update_text}" if update_text else t("shell.status.analyzing")
                )

            # Update animation text without restarting the thread.
            self._start_animation(
                base_text=base_text, pattern="braille", update_text=update_text
            )
        return None

    def handle_thinking_end(self, event: LLMEvent):
        """Handle thinking end event - gracefully stop animation with completion indicator"""
        # Stop background animation
        self._stop_animation()

        if self.current_live:
            # Clear and stop the display
            self.current_live.update("", refresh=True)
            self.current_live.stop()
            self.current_live = None

    def _get_tool_arg_preview_settings(self, tool_name: str) -> ToolArgPreviewSettings:
        raw_config = getattr(self.config, "tool_arg_preview", None)
        if isinstance(raw_config, dict):
            settings = raw_config.get(tool_name) or raw_config.get("default")
            if isinstance(settings, ToolArgPreviewSettings):
                return settings
        return ToolArgPreviewSettings()

    def _truncate_tool_text(self, text: str, settings: ToolArgPreviewSettings) -> str:
        max_lines = settings.max_lines
        max_chars = settings.max_chars

        preview = text
        suffix_parts = []
        lines = text.splitlines()
        if max_lines and len(lines) > max_lines:
            preview = "\n".join(lines[:max_lines])
            suffix_parts.append(f"+{len(lines) - max_lines} lines")

        if max_chars and len(preview) > max_chars:
            extra_chars = len(preview) - max_chars
            preview = preview[:max_chars]
            suffix_parts.append(f"+{extra_chars} chars")

        if suffix_parts:
            return f"{preview} ... ({', '.join(suffix_parts)})"
        return preview

    def _format_tool_arg_value(
        self, value: object, settings: ToolArgPreviewSettings
    ) -> str:
        max_items = settings.max_items

        if isinstance(value, str):
            return self._truncate_tool_text(value, settings)
        if isinstance(value, dict):
            items = list(value.items())
            rendered_items = []
            for key, item in items[:max_items]:
                rendered_items.append(
                    f"{key}={self._format_tool_arg_value(item, settings)}"
                )
            if len(items) > max_items:
                rendered_items.append(f"... (+{len(items) - max_items} more)")
            return ", ".join(rendered_items)
        if isinstance(value, (list, tuple)):
            items = list(value)
            rendered_items = [
                self._format_tool_arg_value(item, settings)
                for item in items[:max_items]
            ]
            if len(items) > max_items:
                rendered_items.append(f"... (+{len(items) - max_items} more)")
            return "[" + ", ".join(rendered_items) + "]"
        return str(value)

    def _format_tool_args_for_display(self, tool_name: str, tool_args: object) -> str:
        if tool_name == "write_file" and isinstance(tool_args, dict):
            display_args = dict(tool_args)
            display_args.pop("content", None)
            return self._format_tool_arg_value(display_args, ToolArgPreviewSettings())
        settings = self._get_tool_arg_preview_settings(tool_name)
        if not settings.enabled:
            if isinstance(tool_args, dict) and len(tool_args) == 1:
                return str(next(iter(tool_args.values())))
            return str(tool_args)

        if isinstance(tool_args, dict) and len(tool_args) == 1:
            tool_args = next(iter(tool_args.values()))
        return self._format_tool_arg_value(tool_args, settings)

    def handle_tool_execution_start(self, event: LLMEvent):
        """Handle tool execution start event"""
        self._finalize_content_preview()
        tool_name = event.data.get("tool_name", "unknown")
        tool_args = self._format_tool_args_for_display(
            tool_name, event.data.get("tool_args", {})
        )

        # Check if this is from system_diagnose_agent and add indentation
        prefix = t("shell.tool.prefix")
        if event.data and event.data.get("source") == "system_diagnose_agent":
            prefix = t("shell.tool.prefix_diagnose")

        self.console.print(f"{prefix}: {tool_name} ({tool_args})", style="cyan")

    def handle_tool_execution_end(self, event: LLMEvent):
        """Handle tool execution end event"""
        tool_name = event.data.get("tool_name", "unknown")

        # Check if this is from system_diagnose_agent and add indentation
        if event.data and event.data.get("source") == "system_diagnose_agent":
            prefix = t("shell.tool.done_diagnose")
            self.console.print(f"{prefix}: {tool_name}", style="green")

    def _render_streaming_chunk(self, accumulated_content: str) -> None:
        display_text = str(accumulated_content).replace("\n", " ")
        terminal_width = self.console.size.width
        display_width = min(60, int(terminal_width * 0.8))

        if len(display_text) > display_width:
            display_text = "🤖 Thinking: " + display_text[-(display_width - 3) :]
        else:
            display_text = "🤖 Thinking: " + display_text

        if self.current_live:
            self.current_live.update(Text(display_text, style="green"), refresh=True)

    def handle_streaming_chunk(self, event: LLMEvent):
        """Handle streaming chunk event - display progressive text"""
        # Stop animation to avoid competing Live updates.
        self._stop_animation()
        self._check_terminal_resize()

        if not self.current_live:
            self.current_live = Live(console=self.console, auto_refresh=False)
            self.current_live.start()

        accumulated_content = event.data.get("accumulated") or event.data.get(
            "delta", ""
        )
        self._last_reasoning_render_lines = []
        self._last_streaming_accumulated = str(accumulated_content)
        self._render_streaming_chunk(self._last_streaming_accumulated)

    def handle_completion_done(self, event: LLMEvent):
        """Handle completion done event - clean up display"""
        self._last_streaming_accumulated = ""
        self._last_reasoning_render_lines = []
        if self.current_live:
            self.current_live.update("", refresh=True)
            self.current_live.stop()
            self.current_live = None

    def handle_error_event(self, event: LLMEvent):
        """Handle error event - display error message"""
        self._finalize_content_preview()

        # If LLM failed while we were only trying to detect whether input is a command,
        # avoid triggering a second LLM request for the same input.
        if self._llm_call_context == "command_detection":
            self._command_detection_llm_failed = True

        error_message = event.data.get("error_message", "Unknown error")
        error_type = event.data.get("error_type", "general")
        error_details = event.data.get("error_details")

        # Check for quota/rate limit errors (only for free key mode)
        # self.config is ConfigModel, use .is_free_key attribute
        is_free_key = getattr(self.config, "is_free_key", False)
        quota_exhausted = False
        service_unavailable = False
        if is_free_key:
            error_lower = str(error_message).lower()
            # Check for quota exhaustion (rate limit, quota exceeded)
            quota_exhausted = any(
                keyword in error_lower
                for keyword in [
                    "rate limit", "quota", "insufficient", "429", "exceeded",
                    "too many requests"
                ]
            )
            # Check for service unavailability (auth errors, not found, server errors)
            # These may indicate the free key is invalid or server resources exhausted
            if not quota_exhausted:
                service_unavailable = any(
                    keyword in error_lower
                    for keyword in [
                        "401", "403", "404", "500", "502", "503", "504",
                        "authentication", "unauthorized", "forbidden",
                        "not found", "internal server error", "bad gateway",
                        "service unavailable", "gateway timeout"
                    ]
                )

        if error_type == "streaming_error":
            self.console.print(f"❌ Streaming Error: {error_message}", style="red")
        elif error_type == "litellm_error":
            self.console.print(
                Panel(
                    Markdown(str(error_message)),
                    title=t("shell.error.llm_error_title"),
                    border_style="red",
                )
            )
            if getattr(self.config, "verbose", False) and error_details:
                self.console.print(
                    Panel(
                        Text(str(error_details), style="dim"),
                        title=t("shell.error.llm_error_details_title"),
                        border_style="dim",
                    )
                )
            # Show quota exhausted hint
            if quota_exhausted:
                self.console.print(
                    Panel(
                        t("cli.setup.free_key_quota_exhausted_hint"),
                        title=t("cli.setup.free_key_quota_exhausted"),
                        border_style="yellow",
                    )
                )
                self.console.print(
                    f"💡 {t('shell.hint.run_setup')}",
                    style="dim"
                )
            # Show service unavailable hint (for free key)
            elif service_unavailable:
                self.console.print(
                    Panel(
                        t("cli.setup.free_key_service_unavailable_hint"),
                        title=t("cli.setup.free_key_service_unavailable"),
                        border_style="yellow",
                    )
                )
                self.console.print(
                    f"💡 {t('shell.hint.run_setup')}",
                    style="dim"
                )
        else:
            self.console.print(f"❌ Error: {error_message}", style="red")

        # Clean up live display on error
        self._reset_reasoning_state()
        self._last_streaming_accumulated = ""
        if self.current_live:
            self.current_live.update("", refresh=True)
            self.current_live.stop()
            self.current_live = None

    def handle_processing_cancelled(self, event: Optional[LLMEvent] = None):
        self._stop_animation()
        self._reset_reasoning_state()
        self._last_streaming_accumulated = ""
        self._finalize_content_preview()

        # Clear any live display
        if self.current_live:
            self.current_live.update("", refresh=True)
            self.current_live.stop()
            self.current_live = None

        # 检查是否为工具确认被拒绝（用户输入 N），如果是则不显示取消消息
        is_tool_denied = (
            event and event.data and event.data.get("reason") == "tool_cancelled"
        )

        # Show cancellation message with appropriate formatting
        if not is_tool_denied:
            if (
                event
                and event.data
                and event.data.get("source") == "system_diagnose_agent"
            ):
                self.console.print(t("shell.diagnose_cancelled"), style="yellow")
            else:
                # 优先使用最后的 AI 执行状态
                last_ai_state = self.interruption_manager.get_last_ai_state()

                if last_ai_state == ShellState.AI_THINKING:
                    # AI 推理阶段：显示瞬时提示
                    self.console.print("<Interrupted received.>", style="dim")
                elif last_ai_state == ShellState.SANDBOX_EVAL:
                    # 沙箱评估阶段：显示瞬时提示
                    self.console.print(
                        "<Stopping... finalizing current task.>", style="dim"
                    )
                elif last_ai_state == ShellState.COMMAND_EXEC:
                    # 命令执行阶段：显示瞬时提示
                    self.console.print(
                        "<Stopping... finishing current task (this may take a moment)>",
                        style="dim",
                    )
                # 移除 else 分支，不显示通用的 "❌ 操作已取消" 消息
                # 只通过事件系统显示瞬时提示

        # 清除最后的 AI 状态
        self.interruption_manager.clear_last_ai_state()

    def _on_interrupt_requested(self) -> None:
        """中断回调函数 - 由 InterruptionManager 调用"""
        # 取消 LLM 操作
        # 瞬时提示会在 handle_processing_cancelled 中根据状态显示
        self.llm_session.cancellation_token.cancel(
            CancellationReason.USER_INTERRUPT, "User pressed Ctrl+C"
        )

    def handle_tool_confirmation_required(self, event: LLMEvent) -> LLMCallbackResult:
        """Handle tool confirmation required event - display confirmation dialog and get user response"""
        return _prompt_handle_tool_confirmation_required(self, event)

    def handle_ask_user_required(self, event: LLMEvent) -> LLMCallbackResult:
        """Handle ask_user event - show interactive single-choice UI.

        Uses inline UI at bottom of screen if config.tui.inline_ui is True,
        otherwise uses the traditional modal dialog.
        """
        # Check if inline UI is enabled in config (default to True)
        try:
            inline_ui = getattr(getattr(self.config, "tui", None), "inline_ui", True)
        except Exception:
            inline_ui = True
        if inline_ui:
            return _prompt_handle_ask_user_required_inline(self, event)
        return _prompt_handle_ask_user_required(self, event)

    def request_user_choice(self, data: dict) -> tuple[str | None, str]:
        """Request a user choice via the shell UI.

        This method is used by plan_agent to get user input for plan confirmation.

        Args:
            data: Dictionary containing:
                - prompt: The question to ask
                - options: List of option dicts with 'value' and 'label'
                - default: Default option value
                - title: Optional title for the selection
                - allow_cancel: Whether to allow cancellation
                - allow_custom_input: Whether to allow custom input

        Returns:
            (selected_value, status) where status is one of:
            - "selected": User made a selection
            - "cancelled": User cancelled
            - "unavailable": UI not available (non-TTY)
            - "error": An error occurred
        """
        import sys

        # Check if we have a terminal available
        # Note: In plan_agent context, we may be running in a ThreadPoolExecutor
        # but still have access to the terminal
        try:
            has_tty = sys.stdin.isatty() and sys.stdout.isatty()
        except Exception:
            has_tty = False

        # If no TTY but we have a running shell, try anyway
        # (This handles the case where plan_agent runs in ThreadPoolExecutor)
        if not has_tty and not self.running:
            return None, "unavailable"

        try:
            # Create event and handle it
            event = LLMEvent(
                event_type=LLMEventType.ASK_USER_REQUIRED,
                data=data,
                timestamp=time.time(),
            )
            self.handle_ask_user_required(event)

            # Check if user made a selection
            selected_value = data.get("selected_value")
            if isinstance(selected_value, str) and selected_value:
                return selected_value, "selected"

            # Check for custom input
            custom_input = data.get("custom_input")
            if isinstance(custom_input, str) and custom_input.strip():
                return custom_input.strip(), "selected"

            return None, "cancelled"
        except KeyboardInterrupt:
            raise
        except Exception:
            return None, "error"

    def _display_security_panel(self, data: dict, panel_mode: str = "confirm"):
        """Display rich security panel for AI tool calls."""
        _prompt_display_security_panel(self, data, panel_mode=panel_mode)

    def _get_shell_command_confirmation(self, data: dict) -> LLMCallbackResult:
        """Get confirmation for shell command execution - reuses LLM confirmation UI"""
        # Display confirmation request using existing UI
        self._display_security_panel(data, panel_mode="confirm")

        # Get user confirmation using existing method
        return self._get_user_confirmation(
            remember_command=data.get("command"),
            allow_remember=True,
        )

    def _get_user_confirmation(
        self,
        remember_command: Optional[str] = None,
        allow_remember: bool = False,
    ) -> LLMCallbackResult:
        """Get interactive confirmation from user"""
        return _prompt_get_user_confirmation(
            self,
            remember_command=remember_command,
            allow_remember=allow_remember,
        )

    async def run(self):
        """Main shell loop"""
        self.logger.info("AI-Shell started")
        self.print_welcome()

        try:
            # 使用 anyio 的信号接收器来处理 SIGINT，防止 anyio.run() 的快速退出机制
            # 注意: open_signal_receiver 返回普通上下文管理器，不是异步的
            with anyio.open_signal_receiver(signal.SIGINT) as sigs:
                # 创建一个任务来处理信号
                async def signal_handler():
                    try:
                        async for _ in sigs:
                            # 取消当前操作
                            if self._current_op_scope is not None:
                                self._current_op_scope.cancel()
                            self.llm_session.cancellation_token.cancel(
                                CancellationReason.USER_INTERRUPT,
                                "SIGINT received",
                            )
                    except anyio.get_cancelled_exc_class():
                        # 任务被取消，正常退出
                        pass

                async def skill_hotreload_task():
                    try:
                        await self._skill_hotreload_service.run()
                    except anyio.get_cancelled_exc_class():
                        pass
                    except Exception:
                        # Never crash the shell due to watcher errors.
                        pass

                async def llm_init_task():
                    """后台初始化 LLM，避免首次调用时阻塞"""
                    try:
                        await self.llm_session._background_initialize()
                    except anyio.get_cancelled_exc_class():
                        pass
                    except Exception:
                        # 初始化失败不影响后续使用
                        pass

                async def _loop_body():
                    while self.running:
                        try:
                            prompt_text = self.get_prompt()

                            # 内层循环：处理空输入（清空后继续在同一行等待输入）
                            while True:
                                # Get first line of input
                                first_input = await self.get_user_input(prompt_text)

                                # Check for semicolon key press for error correction
                                # Only if there's pending error correction
                                if (
                                    self._pending_error_correction
                                    and first_input == "__CORRECT_SEMICOLON__"
                                ):
                                    await self._execute_error_correction()
                                    continue

                                # 如果输入为空（通过 Ctrl+C 或 Esc 清空），继续在同一行等待
                                # 不换行，不重新显示 prompt
                                if not first_input or not first_input.strip():
                                    continue
                                break

                            # Check if AI mode (starts with ?) or line ends with backslash for continuation
                            is_ai_mode = self.starts_with_question_mark(
                                first_input.strip()
                            )
                            needs_continuation = first_input.rstrip().endswith("\\")

                            if is_ai_mode or needs_continuation:
                                # Use multiline input for AI mode or backslash continuation
                                # For AI mode, preserve newlines; for commands, use spaces (fish-style)
                                user_input = await self._get_multiline_input(
                                    prompt_text, first_input, ai_mode=is_ai_mode
                                )
                            else:
                                # For regular commands without continuation, just use the single line input
                                user_input = first_input

                            await self.process_input(user_input)
                        except (KeyboardInterrupt, anyio.get_cancelled_exc_class()):
                            # Check if user requested exit (Ctrl+C twice at input prompt)
                            if self._user_requested_exit:
                                self.logger.info("Exit requested by Ctrl+C")
                                self.running = False
                                self._exit_shell()
                                break

                            # Operation cancelled (e.g., Ctrl+C during AI execution). Clean up and continue loop.
                            if self.operation_in_progress:
                                self.handle_processing_cancelled()
                                self.operation_in_progress = False
                            # re-render prompt cleanly by a short sleep to allow prompt_toolkit to reset
                            await anyio.sleep(0)
                            continue
                        except EOFError:
                            if not self.processing_input:
                                self.running = False
                                self._exit_shell()
                                break
                            else:
                                self.llm_session.cancellation_token.cancel(
                                    CancellationReason.USER_INTERRUPT,
                                    "EOF during processing",
                                )
                                self.handle_processing_cancelled()
                                continue
                        except Exception as e:
                            self.console.print(
                                f"💥 Unexpected error: {str(e)}", style="red"
                            )
                            continue

                # 在任务组中同时运行信号处理器和主循环
                try:
                    async with anyio.create_task_group() as tg:
                        tg.start_soon(signal_handler)
                        tg.start_soon(skill_hotreload_task)
                        tg.start_soon(llm_init_task)  # 后台初始化 LLM
                        await _loop_body()
                        # 当 _loop_body 退出后，取消 signal_handler
                        # 显式停止 hotreload service，否则 watchfiles.awatch 可能因为 debounce 而阻塞退出
                        self._skill_hotreload_service.stop()
                        tg.cancel_scope.cancel()
                except AssertionError:
                    # Fallback for non-anyio runtimes (e.g. pytest-asyncio).
                    await _loop_body()
        finally:
            # Stop any running animations
            self._stop_animation()

            try:
                # Reduced timeout from 2.0s to 1.0s since LLM cleanup now has internal timeouts
                with anyio.move_on_after(1.0):
                    await to_thread.run_sync(self.llm_session.cleanup)
            except Exception:
                pass  # 忽略清理错误，确保程序能正常退出
            self.logger.info("AI-Shell exited")


async def main():
    """Main entry point"""
    config = Config()
    skill_manager = SkillManager()
    skill_manager.load_all_skills()
    shell = AIShell(
        config=config.model_config,
        skill_manager=skill_manager,
        config_manager=config,
    )
    await shell.run()


if __name__ == "__main__":
    anyio.run(main)
