"""Shell application runtime."""

from __future__ import annotations

import anyio
import datetime as dt
import getpass
import os
import select
import shlex
import shutil
import signal
import sys
import termios
import threading
import time
import tty
import uuid
from pathlib import Path
from typing import TYPE_CHECKING, Any, Optional

from rich.console import Console
from rich.live import Live

from ...config import (
    Config,
    ConfigModel,
    ToolArgPreviewSettings,
    get_default_session_db_path,
)
from ...i18n import t
from ...llm.providers.registry import get_provider_for_model
from ...plan import (
    PLAN_STATE_KEY,
    PlanApprovalStatus,
    PlanPhase,
    decode_plan_state,
    encode_plan_state,
)
from ...terminal.pty.control_protocol import BackendControlEvent
from ...prompts import PromptManager
from ...terminal.pty import PTYManager
from ...state.logging import set_session_uuid
from ...system_info import (
    get_current_env_info,
    get_or_fetch_static_env_info,
    get_output_language,
)
from ...security.security_manager import SimpleSecurityManager
from ...state import (
    ContextManager,
    HistoryManager,
    MemoryType,
    SessionRecord,
    SessionStore,
)
from ...skills.hotreload import SkillHotReloadService
from ...wizard.setup_wizard import run_interactive_setup
from .ai import AIHandler
from .events import LLMEventRouter
from ..commands import BuiltinRegistry, SHELL_EXIT_COMMANDS
from ..ui.interaction import PTYUserInteraction
from ..ui.welcome import build_welcome_renderable
from .output import OutputProcessor
from ..ui.editor import ShellPromptController
from ..ui.prompt_io import (
    display_security_panel,
    get_user_confirmation,
    handle_interaction_required,
)

if TYPE_CHECKING:
    from ...llm import LLMSession, LLMEventType
    from ...skills import SkillManager
else:
    from ...llm import LLMEventType


class PTYAIShell:
    """AI shell with direct PTY connection."""

    _TTFT_DISPLAY_THRESHOLD = 0.1  # minimum seconds to show timing summary

    def __init__(
        self,
        config: ConfigModel,
        skill_manager: "SkillManager",
        config_manager: Optional[Config] = None,
        console: Optional[Console] = None,
    ):
        self.config = config
        self.skill_manager = skill_manager
        self.config_manager = config_manager
        self.console = console or Console(force_terminal=True)

        # Session persistence
        self.session_record: Optional[SessionRecord] = None
        self._create_new_session_record()
        if self.session_record:
            set_session_uuid(self.session_record.session_uuid)

        # History manager
        self.history_manager = HistoryManager(
            db_path=self._resolve_session_db_path(),
            session_uuid=self.session_record.session_uuid if self.session_record else str(uuid.uuid4()),
        )

        self._pty_manager: Optional[PTYManager] = None
        self._prompt_controller: Optional[ShellPromptController] = None
        self._output_processor: Optional[OutputProcessor] = None
        self._ai_handler: Optional[AIHandler] = None
        self._running = False
        self._original_termios: Optional[list] = None

        self.prompt_manager: PromptManager = PromptManager()
        self.context_manager: ContextManager = ContextManager(
            max_llm_messages=getattr(config, "max_llm_messages", 50),
            max_shell_messages=getattr(config, "max_shell_messages", 20),
            token_budget=getattr(config, "context_token_budget", None),
            model=config.model,
            enable_token_estimation=getattr(config, "enable_token_estimation", True),
        )

        # Long-term memory must be initialized before LLM session so that
        # MemoryTool can be registered during session construction.
        self.memory_manager = None
        memory_config = getattr(config, "memory", None)
        if memory_config and getattr(memory_config, "enabled", False):
            from aish.memory.manager import MemoryManager

            self.memory_manager = MemoryManager(config=memory_config)
            # Inject session context from memory (always returns something useful)
            mem_ctx = self.memory_manager.get_session_context()
            if mem_ctx:
                self.context_manager.add_memory(
                    MemoryType.KNOWLEDGE,
                    {"key": "long_term_memory", "value": mem_ctx},
                )

        self.llm_session: "LLMSession" = self._create_llm_session()

        self.uname_info, self.os_info, self.basic_env_info = get_or_fetch_static_env_info()
        self.output_language = get_output_language(config)
        self.current_env_info = get_current_env_info()

        env_entry = f"[system information] {self.current_env_info}"
        self.context_manager.add_memory(
            MemoryType.LLM, {"role": "user", "content": env_entry}
        )
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

        self.skill_hotreload_service: SkillHotReloadService = SkillHotReloadService(
            skill_manager=skill_manager, debounce_ms=200
        )

        self.llm_event_router: LLMEventRouter = LLMEventRouter(
            handlers={
                LLMEventType.OP_START: self.handle_operation_start,
                LLMEventType.OP_END: self.handle_op_end,
                LLMEventType.GENERATION_START: self.handle_generation_start,
                LLMEventType.GENERATION_END: self.handle_generation_end,
                LLMEventType.REASONING_START: self.handle_reasoning_start,
                LLMEventType.REASONING_DELTA: self.handle_reasoning_delta,
                LLMEventType.REASONING_END: self.handle_reasoning_end,
                LLMEventType.CONTENT_DELTA: self.handle_content_delta,
                LLMEventType.TOOL_EXECUTION_START: self.handle_tool_execution_start,
                LLMEventType.TOOL_EXECUTION_END: self.handle_tool_execution_end,
                LLMEventType.ERROR: self.handle_error_event,
                LLMEventType.CANCELLED: self.handle_processing_cancelled,
                LLMEventType.INTERACTION_REQUIRED: self.handle_interaction_required,
                LLMEventType.TOOL_CONFIRMATION_REQUIRED: self.handle_tool_confirmation_required,
            }
        )
        self.animation_active: bool = False
        self.animation_thread: Optional[threading.Thread] = None
        self.animation_lock: threading.Lock = threading.Lock()
        self.animation_counter: int = 0
        # Response timing state
        self._thinking_start_time: float = 0.0  # monotonic time when thinking starts
        self._timing_active: bool = False           # whether a timing-tracked operation is in progress
        self._ttft: float = 0.0                 # time to first token in seconds
        self._ttft_recorded: bool = False       # whether first token has been seen
        self._animation_base_text: str = "思考中"
        self._animation_pattern: str = "braille"
        self._animation_update_text: Optional[str] = None
        self._reasoning_display_enabled: bool = False
        self._reasoning_active: bool = False
        self._reasoning_partial: str = ""
        self._reasoning_lines: list[str] = []
        self._reasoning_max_lines: int = 2
        self._last_reasoning_render_lines: list[str] = []
        self._last_streaming_accumulated: str = ""
        self.current_live: Optional[Any] = None
        self._content_preview_active: bool = False
        self._content_streamed_to_terminal: bool = False
        self._at_line_start: bool = True

        self._current_op_scope: Optional[Any] = None
        self._user_requested_exit: bool = False
        self.operation_in_progress: bool = False

        self.user_interaction: PTYUserInteraction = PTYUserInteraction()
        self._approved_ai_commands: set[str] = set()
        self._editing_buffer_text: str = ""
        self._backend_control_buffer: bytes = b""
        self._backend_protocol_events: list[BackendControlEvent] = []
        self._backend_protocol_errors: list[str] = []
        self._backend_session_ready: bool = False
        self._last_backend_event: Optional[BackendControlEvent] = None
        self._shell_phase: str = "booting"
        self._next_command_seq: int = 1
        self._pending_command_seq: Optional[int] = None
        self._pending_command_text: Optional[str] = None
        self._current_cwd: str = os.getcwd()
        self.security_manager = SimpleSecurityManager(
            console=self.console,
            repo_root=Path(self._current_cwd).resolve(),
        )

        self._last_exit_code: int = 0
        self._shell_preview_bytes: int = 4096


    def _resolve_session_db_path(self) -> Path:
        """Resolve session database path from config or default."""
        db_path_value = getattr(self.config, "session_db_path", "")
        if not str(db_path_value).strip():
            db_path_value = get_default_session_db_path()
        return Path(db_path_value).expanduser()

    def _create_new_session_record(self) -> None:
        """Create a new persisted session record."""
        db_path = self._resolve_session_db_path()

        store: Optional[SessionStore] = None
        try:
            store = SessionStore(db_path)
            self.session_record = store.create_session(
                model=self.config.model,
                api_base=getattr(self.config, "api_base", None),
                run_user=getpass.getuser(),
                state={
                    "temperature": getattr(self.config, "temperature", None),
                    "max_tokens": getattr(self.config, "max_tokens", None),
                    PLAN_STATE_KEY: encode_plan_state(
                        {},
                        decode_plan_state({}, default_source_session_uuid=""),
                    ).get(PLAN_STATE_KEY),
                },
            )
        except Exception:
            self.session_record = SessionRecord(
                session_uuid=str(uuid.uuid4()),
                created_at=dt.datetime.utcnow(),
                model=self.config.model,
                api_base=getattr(self.config, "api_base", None),
                run_user=getpass.getuser(),
                state={
                    "temperature": getattr(self.config, "temperature", None),
                    "max_tokens": getattr(self.config, "max_tokens", None),
                    PLAN_STATE_KEY: encode_plan_state(
                        {},
                        decode_plan_state({}, default_source_session_uuid=""),
                    ).get(PLAN_STATE_KEY),
                },
            )
        finally:
            if store is not None:
                try:
                    store.close()
                except Exception:
                    pass

    def _create_llm_session(self) -> "LLMSession":
        """Create LLMSession with all necessary dependencies."""
        import logging

        from ..interruption import InterruptionManager
        from ...llm import LLMSession

        logger = logging.getLogger(__name__)
        interruption_manager = InterruptionManager()
        self.interruption_manager = interruption_manager
        interruption_manager.set_interrupt_callback(self._on_interrupt_requested)
        session_record = getattr(self, "session_record", None)

        session = LLMSession(
            config=self.config,
            skill_manager=self.skill_manager,
            event_callback=self.handle_llm_event,
            env_manager=None,
            interruption_manager=interruption_manager,
            is_command_approved=self._is_command_approved,
            history_manager=getattr(self, "history_manager", None),
            memory_manager=getattr(self, "memory_manager", None),
            session_uuid=session_record.session_uuid if session_record else None,
            session_state=session_record.state if session_record else None,
            session_state_updater=self._update_current_session_state,
        )

        def init_litellm_in_background() -> None:
            try:
                session._get_litellm()
                session._get_acompletion()
                with session._sync_init_lock:
                    session._initialized = True
                logger.info("LLM client initialized successfully in background")
            except Exception as error:
                logger.warning(
                    "LLM background initialization failed: %s, will retry on first use",
                    error,
                )

        init_thread = threading.Thread(target=init_litellm_in_background, daemon=True)
        init_thread.start()
        return session

    def _reset_timing_state(self) -> None:
        self._timing_active = False
        self._thinking_start_time = 0.0
        self._ttft = 0.0
        self._ttft_recorded = False

    def _update_current_session_state(self, state_patch: dict[str, object]) -> None:
        if self.session_record is None:
            return

        merged = dict(self.session_record.state)
        merged.update(state_patch)
        self.session_record.state = merged

        store: Optional[SessionStore] = None
        try:
            store = SessionStore(self._resolve_session_db_path())
            updated = store.update_session_state(
                self.session_record.session_uuid,
                state_patch,
            )
            if updated is not None:
                self.session_record = updated
        except Exception:
            pass
        finally:
            if store is not None:
                try:
                    store.close()
                except Exception:
                    pass

    def handle_operation_start(self, event) -> None:
        self._timing_active = True
        self._thinking_start_time = time.monotonic()
        self._ttft_recorded = False
        self._ttft = 0.0
        self._content_streamed_to_terminal = False
        return None

    def handle_op_end(self, event) -> None:
        # Display timing summary at operation end
        if self._timing_active and self._ttft >= self._TTFT_DISPLAY_THRESHOLD:
            self.console.print()
            self.console.print(
                f"思考: {self._ttft:.1f}s",
                style="dim",
            )
        self._reset_timing_state()
        return None

    def handle_generation_start(self, event) -> None:
        self._finalize_content_preview()
        self._reset_reasoning_state()
        self._last_streaming_accumulated = ""
        self._reasoning_display_enabled = True
        return self.handle_thinking_start(event)

    def handle_thinking_start(self, event) -> None:
        self._stop_animation()
        self._last_streaming_accumulated = ""
        self._last_reasoning_render_lines = []

        if self.current_live:
            self.current_live.stop()

        if not self._at_line_start:
            self.console.print()
            self._at_line_start = True

        self.current_live = Live(console=self.console, auto_refresh=False, transient=True)
        self.current_live.start()
        self._start_animation(base_text="思考中", pattern="braille")
        return None

    def _update_reasoning_live(self, lines: list[str]) -> None:
        from rich.text import Text

        self._check_terminal_resize()
        if not self.current_live:
            self.current_live = Live(console=self.console, auto_refresh=False, transient=True)
            self.current_live.start()

        self._last_reasoning_render_lines = list(lines)
        with self.animation_lock:
            spinner_char = self._get_current_spinner_char("dots")
            self.animation_counter += 1
            thinking_start = self._thinking_start_time

        max_width = max(10, self.console.size.width - 4)
        trimmed_lines = [line[-max_width:] if len(line) > max_width else line for line in lines]
        thinking_label = "思考中"
        elapsed = time.monotonic() - thinking_start if thinking_start > 0 else 0
        if trimmed_lines:
            display_text = "\n".join([f"{spinner_char} {thinking_label} ... {elapsed:.1f}s", *trimmed_lines])
        else:
            display_text = f"{spinner_char} {thinking_label} ... {elapsed:.1f}s"

        self.current_live.update(Text(display_text, style="grey50"), refresh=True)

    def _render_streaming_chunk(self, accumulated_content: str) -> None:
        from rich.text import Text

        display_text = str(accumulated_content).replace("\n", " ")
        terminal_width = self.console.size.width
        display_width = min(60, int(terminal_width * 0.8))

        if len(display_text) > display_width:
            display_text = "🤖 思考中: " + display_text[-(display_width - 3):]
        else:
            display_text = "🤖 思考中: " + display_text

        if self.current_live:
            self.current_live.update(Text(display_text, style="green"), refresh=True)

    def _start_animation(
        self,
        base_text: Optional[str] = None,
        pattern: str = "braille",
        update_text: Optional[str] = None,
    ) -> None:
        with self.animation_lock:
            self._animation_base_text = base_text or "思考中"
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

    def _animate_thinking(self) -> None:
        from rich.text import Text

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

                    # Read _thinking_start_time under lock for thread safety
                    thinking_start = self._thinking_start_time

                if update_text:
                    display_text = f"{spinner_char} {update_text}"
                elif thinking_start > 0:
                    elapsed = time.monotonic() - thinking_start
                    display_text = f"{spinner_char} {base_text} ... {elapsed:.1f}s"
                else:
                    display_text = f"{spinner_char} {base_text}..."

                current_live.update(Text(display_text, style="blue"), refresh=True)
                time.sleep(0.15)
            except Exception:
                break

    def _stop_animation(self) -> None:
        try:
            with self.animation_lock:
                self.animation_active = False

            if self.animation_thread and self.animation_thread.is_alive():
                self.animation_thread.join(timeout=0.5)
        except Exception:
            pass
        finally:
            self.animation_thread = None

    def handle_generation_end(self, event) -> None:
        self._stop_animation()
        self._reset_reasoning_state()
        self._last_streaming_accumulated = ""
        self._finalize_content_preview()
        if self.current_live:
            self.current_live.update("", refresh=True)
            self.current_live.stop()
            self.current_live = None
        return None

    def handle_completion_done(self, event) -> None:
        self._last_streaming_accumulated = ""
        self._last_reasoning_render_lines = []
        if self.current_live:
            self.current_live.update("", refresh=True)
            self.current_live.stop()
            self.current_live = None
        return None

    def handle_reasoning_start(self, event) -> None:
        if not self._reasoning_display_enabled:
            return None

        self._finalize_content_preview()
        self._stop_animation()
        self._reasoning_active = True
        self._last_streaming_accumulated = ""
        self._reasoning_partial = ""
        self._reasoning_lines = []
        self._update_reasoning_live([])
        return None

    def handle_reasoning_delta(self, event) -> None:
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
                self._reasoning_lines = self._reasoning_lines[-self._reasoning_max_lines :]

        all_lines = list(self._reasoning_lines)
        if self._reasoning_partial:
            all_lines.append(self._reasoning_partial)

        if not all_lines:
            return []
        return all_lines[-self._reasoning_max_lines :]

    def handle_reasoning_end(self, event) -> None:
        self._reasoning_active = False
        return None

    def handle_content_delta(self, event) -> None:
        from rich.text import Text

        self._stop_animation()
        self._reset_reasoning_state()
        self._last_streaming_accumulated = ""

        if self.current_live:
            self.current_live.update("", refresh=True)
            self.current_live.stop()
            self.current_live = None

        is_final = bool(event.data.get("is_final", True))

        if is_final and not self._ttft_recorded and self._thinking_start_time > 0:
            self._ttft = time.monotonic() - self._thinking_start_time
            self._ttft_recorded = True

        content = event.data.get("delta") or event.data.get("accumulated") or ""
        content = str(content)
        if not content:
            return None

        if not self._content_preview_active:
            self._content_preview_active = True
            content = f"🤖 {content}"

        if is_final:
            self._content_streamed_to_terminal = True

        self.console.print(Text(content, style="bold grey50"), end="")
        self._at_line_start = False
        return None

    def handle_tool_execution_start(self, event) -> None:
        self._finalize_content_preview()
        tool_name = event.data.get("tool_name", "unknown")
        tool_args = event.data.get("tool_args", {})

        # Show plan artifact writes with a special label instead of
        # generic write_file / edit_file, so the user perceives them as
        # plan-managed operations rather than ordinary file mutations.
        if (
            tool_name in {"write_file", "edit_file"}
            and isinstance(tool_args, dict)
            and self._is_plan_artifact_write(tool_args)
        ):
            artifact = tool_args.get("file_path", "")
            self.console.print(
                t("plan.tool.update_artifact", path=artifact),
                style="cyan",
            )
            return None

        display_args = self._format_tool_args_for_display(tool_name, tool_args)

        prefix = t("shell.tool.prefix")
        if event.data and event.data.get("source") == "system_diagnose_agent":
            prefix = t("shell.tool.prefix_diagnose")

        self.console.print(f"{prefix}: {tool_name} ({display_args})", style="cyan")
        return None

    def handle_tool_execution_end(self, event) -> None:
        tool_name = event.data.get("tool_name", "unknown")

        if tool_name == "exit_plan_mode":
            result_data = event.data.get("result_data")
            if isinstance(result_data, dict):
                decision = str(result_data.get("decision") or "").strip().lower()
                if decision == "approve":
                    self.console.print(t("plan.approval.approved"), style="green")
                    return None
                if decision == "changes_requested":
                    self.console.print(
                        t("plan.approval.changes_requested"),
                        style="yellow",
                    )
                    return None
                if decision == "cancelled":
                    self.console.print(
                        t("plan.approval.cancelled"),
                        style="yellow",
                    )
                    return None

        if event.data and event.data.get("source") == "system_diagnose_agent":
            prefix = t("shell.tool.done_diagnose")
            self.console.print(f"{prefix}: {tool_name}", style="green")
        return None

    def handle_error_event(self, event) -> None:
        self._finalize_content_preview()
        error_msg = event.data.get("error_message", "Unknown error")
        self.console.print(t("shell.error.llm_error_message", error=error_msg), style="red")

        self._reset_reasoning_state()
        self._last_streaming_accumulated = ""
        if self.current_live:
            self.current_live.update("", refresh=True)
            self.current_live.stop()
            self.current_live = None
        return None

    def _on_interrupt_requested(self) -> None:
        from ...state.cancellation import CancellationReason

        self.llm_session.cancellation_token.cancel(
            CancellationReason.USER_INTERRUPT, "User pressed Ctrl+C"
        )

    def handle_processing_cancelled(self, event=None) -> None:
        from ..interruption import ShellState

        self._stop_animation()
        self._reset_timing_state()
        self._reset_reasoning_state()
        self._last_streaming_accumulated = ""
        self._finalize_content_preview()

        if self.current_live:
            self.current_live.update("", refresh=True)
            self.current_live.stop()
            self.current_live = None

        is_tool_denied = (
            event and event.data and event.data.get("reason") == "tool_cancelled"
        )

        if not is_tool_denied:
            last_ai_state = self.interruption_manager.get_last_ai_state()

            if last_ai_state == ShellState.AI_THINKING:
                self.console.print("<Interrupted received.>", style="dim")
            elif last_ai_state == ShellState.SANDBOX_EVAL:
                self.console.print(
                    "<Stopping... finalizing current task.>", style="dim"
                )
            elif last_ai_state == ShellState.COMMAND_EXEC:
                self.console.print(
                    "<Stopping... finishing current task (this may take a moment)>",
                    style="dim",
                )

        self.interruption_manager.clear_last_ai_state()

    def handle_interaction_required(self, event):
        return handle_interaction_required(self, event)

    def handle_tool_confirmation_required(self, event):
        from ...llm import LLMCallbackResult

        self._stop_animation()
        if self.current_live:
            try:
                self.current_live.update("", refresh=True)
                self.current_live.stop()
            finally:
                self.current_live = None

        self._finalize_content_preview()
        data = event.data or {}
        panel = data.get("panel") if isinstance(data.get("panel"), dict) else {}
        panel_mode = str(panel.get("mode") or data.get("panel_mode", "confirm")).lower()

        display_security_panel(self, data, panel_mode=panel_mode)

        if panel_mode == "confirm":
            remember_command = panel.get("remember_key", data.get("remember_key"))
            allow_remember = bool(panel.get("allow_remember", data.get("allow_remember")))
            return get_user_confirmation(
                self,
                remember_command=remember_command,
                allow_remember=allow_remember,
            )
        return LLMCallbackResult.CONTINUE

    def handle_llm_event(self, event) -> Any:
        return self.llm_event_router.handle(event)

    def run(self) -> None:
        self._setup_signals()
        self._save_terminal()
        self._show_welcome()
        self._setup_pty()
        self._setup_components()

        self._running = True
        try:
            self._wait_for_backend_ready()
            while self._running:
                self._drain_backend_io(timeout=0.0)
                if not self._running:
                    break

                if self._shell_phase != "editing":
                    self._passthrough_until_prompt_ready()
                    continue

                line = self._prompt_for_command()
                if line is None:
                    continue

                self._handle_prompt_submission(line)

                if self._shell_phase in ("command_submitted", "running_passthrough"):
                    self._passthrough_until_prompt_ready()
        except KeyboardInterrupt:
            pass
        finally:
            self._cleanup()

    def _setup_signals(self) -> None:
        signal.signal(signal.SIGTERM, self._sigterm_handler)
        signal.signal(signal.SIGWINCH, self._sigwinch_handler)

    def _sigterm_handler(self, signum, frame) -> None:
        self._running = False

    def _sigwinch_handler(self, signum, frame) -> None:
        if self._pty_manager:
            try:
                size = shutil.get_terminal_size()
                self._pty_manager.resize(size.lines, size.columns)
            except Exception:
                pass

    def _save_terminal(self) -> None:
        try:
            self._original_termios = termios.tcgetattr(sys.stdin.fileno())
        except Exception:
            pass

    def _set_raw_mode(self) -> None:
        if self._original_termios:
            tty.setraw(sys.stdin.fileno())

    def _restore_terminal(self) -> None:
        if self._original_termios:
            try:
                termios.tcsetattr(
                    sys.stdin.fileno(), termios.TCSADRAIN, self._original_termios
                )
            except Exception:
                pass

    def _get_spinner_patterns(self) -> dict[str, list[str]]:
        return {
            "braille": ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"],
            "dots": ["⠁", "⠂", "⠄", "⡀", "⢀", "⠠", "⠐", "⠈"],
            "thinking": ["🤔", "💭", "🧠", "⚡", "✨", "🔍", "💡", "🎯"],
            "progress": ["●○○○", "○●○○", "○○●○", "○○○●", "○○●○", "○●○○"],
            "arrows": ["←", "↖", "↑", "↗", "→", "↘", "↓", "↙"],
            "clock": [
                "🕐", "🕑", "🕒", "🕓", "🕔", "🕕", "🕖", "🕗",
                "🕘", "🕙", "🕚", "🕛",
            ],
        }

    def _get_current_spinner_char(self, pattern_name: str = "braille") -> str:
        patterns = self._get_spinner_patterns()
        pattern = patterns.get(pattern_name, patterns["braille"])
        return pattern[self.animation_counter % len(pattern)]

    def _reset_reasoning_state(self) -> None:
        self._reasoning_display_enabled = False
        self._reasoning_active = False
        self._reasoning_partial = ""
        self._reasoning_lines = []
        self._last_reasoning_render_lines = []

    @property
    def content_was_streamed(self) -> bool:
        """Whether content was streamed to the terminal during this operation."""
        return self._content_streamed_to_terminal

    def _finalize_content_preview(self) -> None:
        if not self._content_preview_active:
            return
        self.console.print()
        self._content_preview_active = False
        self._at_line_start = True

    def _read_terminal_size(self) -> tuple[int, int]:
        """Return terminal (rows, cols), falling back to (24, 80)."""
        try:
            size = shutil.get_terminal_size()
            return (size.lines, size.columns)
        except Exception:
            return (24, 80)

    def _compute_ask_user_max_visible(
        self,
        total_options: int,
        term_rows: int,
        allow_custom_input: bool,
        max_visible_cap: int = 12,
    ) -> int:
        """Calculate how many option rows the modal can show."""
        # Fixed rows: title(1) + separator_top(1) + prompt(1) + blank(1)
        #             + separator_bottom(1) + hint(1) = 6
        fixed_rows = 6
        if allow_custom_input:
            fixed_rows += 2  # custom header + input row
        available = max(1, term_rows - fixed_rows)
        return min(available, max_visible_cap, max(1, total_options))

    def _is_ui_resize_enabled(self) -> bool:
        """Whether the modal should watch for terminal resize events."""
        return True

    @staticmethod
    def _safe_cancel_scope():
        scope = anyio.CancelScope()
        try:
            entered = scope.__enter__()
        except (AssertionError, Exception):
            yield None
            return
        try:
            yield entered
        finally:
            scope.__exit__(None, None, None)

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

    def _is_plan_artifact_write(self, tool_args: dict[str, object]) -> bool:
        """Check if tool_args target the bound plan artifact file."""
        if not hasattr(self, "llm_session") or self.llm_session is None:
            return False
        if self.llm_session.plan_state.phase != PlanPhase.PLANNING.value:
            return False
        return self.llm_session._is_bound_plan_artifact_write(tool_args)

    def _remember_approved_command(self, command: str) -> None:
        command = str(command)
        if not command:
            return
        self._approved_ai_commands.add(command)

    def _is_command_approved(self, command: str) -> bool:
        command = str(command)
        return bool(command) and command in self._approved_ai_commands

    def _build_shell_command_security_event_data(
        self,
        command: str,
        analysis: dict[str, object],
        *,
        panel_mode: str,
    ) -> dict[str, object]:
        panel = {
            "mode": panel_mode,
            "target": command,
            "analysis": analysis,
            "allow_remember": True,
            "remember_key": command,
        }
        return {
            "tool_name": "shell_command",
            "command": command,
            "security_analysis": analysis,
            "panel": panel,
            "panel_mode": panel_mode,
            "target": command,
            "analysis": analysis,
            "allow_remember": True,
            "remember_key": command,
        }

    def submit_ai_backend_command(self, command: str) -> bool:
        from ..interruption import ShellState
        from ...llm import LLMCallbackResult

        command = command.strip()
        if not command:
            return False
        if not self._pty_manager:
            return False

        rejected_msg = BuiltinRegistry.get_rejected_command_message(command)
        if rejected_msg:
            self.console.print(rejected_msg, style="red")
            return False

        if self._is_command_approved(command):
            self.submit_backend_command(command)
            return True

        cwd = Path(self._current_cwd or os.getcwd()).resolve()
        self.interruption_manager.set_state(ShellState.SANDBOX_EVAL)
        try:
            decision = self.security_manager.decide(
                command,
                is_ai_command=True,
                cwd=cwd,
            )
        finally:
            self.interruption_manager.set_state(ShellState.NORMAL)

        analysis = decision.analysis if isinstance(decision.analysis, dict) else {}
        if not decision.allow:
            display_security_panel(
                self,
                self._build_shell_command_security_event_data(
                    command,
                    analysis,
                    panel_mode="blocked",
                ),
                panel_mode="blocked",
            )
            return False

        if decision.require_confirmation:
            display_security_panel(
                self,
                self._build_shell_command_security_event_data(
                    command,
                    analysis,
                    panel_mode="confirm",
                ),
                panel_mode="confirm",
            )
            response = get_user_confirmation(
                self,
                remember_command=command,
                allow_remember=True,
            )
            if response != LLMCallbackResult.APPROVE:
                return False

        self.submit_backend_command(command)
        return True

    def _check_terminal_resize(self) -> None:
        try:
            current_size = shutil.get_terminal_size()
            if not hasattr(self, "_last_terminal_size"):
                self._last_terminal_size = current_size
                return
            if current_size == self._last_terminal_size:
                return
            self._last_terminal_size = current_size
            self._refresh_live_for_resize()
        except Exception:
            pass

    def _refresh_live_for_resize(self) -> None:
        if not self.current_live:
            return

        if self._reasoning_active or self._last_reasoning_render_lines:
            self._update_reasoning_live(self._last_reasoning_render_lines)
            return

        if self._last_streaming_accumulated:
            self._render_streaming_chunk(self._last_streaming_accumulated)

    def _show_welcome(self) -> None:
        try:
            welcome = build_welcome_renderable(self.config)
            self.console.print(welcome)
        except Exception:
            pass

    def _setup_pty(self) -> None:
        try:
            size = shutil.get_terminal_size()
            rows, cols = size.lines, size.columns
        except Exception:
            rows, cols = 24, 80

        self._pty_manager = PTYManager(
            rows=rows, cols=cols, cwd=self._current_cwd, use_output_thread=False,
        )
        self._pty_manager.start()
        if self._pty_manager.startup_cwd:
            self._sync_backend_cwd(self._pty_manager.startup_cwd)
        self._backend_session_ready = bool(
            getattr(self._pty_manager, "startup_session_ready", False)
            or getattr(self._pty_manager, "startup_ready", False)
        )
        if self._pty_manager.startup_ready:
            self._shell_phase = "editing"
        else:
            time.sleep(0.2)

    def _setup_components(self) -> None:
        self.user_interaction._original_termios = self._original_termios
        self._ai_handler = AIHandler(
            self._pty_manager,
            self.llm_session,
            self.prompt_manager,
            self.context_manager,
            self.skill_manager,
            self.user_interaction,
            self._original_termios,
            console=self.console,
        )
        self._ai_handler.shell = self
        self._output_processor = OutputProcessor(
            pty_manager=self._pty_manager,
            shell=self,
        )
        self._prompt_controller = ShellPromptController(
            history_manager=self.history_manager,
            interruption_manager=self.interruption_manager,
            on_buffer_change=self._set_edit_buffer_text,
            cwd_provider=lambda: self._current_cwd,
            prompt_theme=self.config.prompt_theme if self.config.enable_scripts else "",
            exit_code_provider=lambda: self._pty_manager.last_exit_code if self._pty_manager else 0,
            mode_provider=lambda: (
                "plan"
                if self.llm_session is not None
                and self.llm_session.plan_state.phase == PlanPhase.PLANNING.value
                else "aish"
            ),
            mode_toggle_handler=self.toggle_plan_mode,
        )

        # Wire PTY manager and context manager to BashTool for AI tool execution
        if self.llm_session and hasattr(self.llm_session, "bash_tool"):
            self.llm_session.bash_tool.pty_manager = self._pty_manager
            self.llm_session.bash_tool.context_manager = self.context_manager

    def _set_edit_buffer_text(self, text: str) -> None:
        self._editing_buffer_text = text

    def get_edit_buffer_text(self) -> str:
        return self._editing_buffer_text

    def _prompt_for_command(self) -> str | None:
        if self._prompt_controller is None:
            self._running = False
            return None

        self._restore_terminal()
        self._shell_phase = "editing"
        try:
            line = self._prompt_controller.prompt()
            line = self._collect_multiline_input(line)
        except KeyboardInterrupt:
            has_input = bool(self._editing_buffer_text.strip())
            self._editing_buffer_text = ""

            from ..interruption import InterruptAction

            action = self.interruption_manager.handle_ctrl_c(has_input)

            if action == InterruptAction.CONFIRM_EXIT:
                self.console.print("^C", style="dim")
                self._running = False
                return None
            elif action == InterruptAction.REQUEST_EXIT:
                self.console.print("^C  <press Ctrl+C again to exit>", style="dim")
                return None
            else:
                self.console.print("^C", style="dim")
            return None
        except EOFError:
            self._running = False
            return None

        self._editing_buffer_text = ""
        self._at_line_start = True
        return line

    @staticmethod
    def _line_requests_continuation(line: str) -> bool:
        return bool(line) and line.rstrip().endswith("\\")

    @staticmethod
    def _strip_continuation_backslash(line: str) -> str:
        stripped = line.rstrip()
        if not stripped.endswith("\\"):
            return line
        return stripped[:-1].rstrip()

    @staticmethod
    def _merge_multiline_input(lines: list[str], *, ai_mode: bool) -> str:
        if ai_mode:
            return "\n".join(lines)
        return " ".join(line for line in lines if line)

    def _collect_multiline_input(self, first_line: str) -> str:
        if self._prompt_controller is None:
            return first_line
        if not self._line_requests_continuation(first_line):
            return first_line

        ai_mode = self._is_ai_prefixed_line(first_line.lstrip())
        lines = [first_line]

        while self._line_requests_continuation(lines[-1]):
            lines[-1] = self._strip_continuation_backslash(lines[-1])
            continuation_line = self._prompt_controller.prompt("... ")
            lines.append(continuation_line)

        return self._merge_multiline_input(lines, ai_mode=ai_mode)

    def _is_ai_prefixed_line(self, line: str) -> bool:
        return line.startswith((";", "；"))

    @staticmethod
    def _parse_command_parts(line: str) -> list[str]:
        try:
            return shlex.split(line)
        except ValueError:
            return [line.strip()] if line.strip() else []

    @staticmethod
    def _is_special_command(parts: list[str]) -> bool:
        if not parts:
            return False
        return parts[0] in {"/model", "/setup", "/plan"}

    def _record_special_command_result(
        self,
        command: str,
        *,
        exit_code: int,
        stdout: str = "",
        stderr: str = "",
    ) -> None:
        self.add_shell_history(command, exit_code, stdout, stderr)

    def toggle_plan_mode(self) -> None:
        if not hasattr(self, "llm_session") or self.llm_session is None:
            return
        if self.llm_session.plan_state.phase == PlanPhase.PLANNING.value:
            self._leave_plan_mode_directly()
            return
        self.llm_session.begin_new_plan()

    def _leave_plan_mode_directly(self) -> None:
        if not hasattr(self, "llm_session") or self.llm_session is None:
            return
        plan_state = self.llm_session.plan_state
        if plan_state.phase != PlanPhase.PLANNING.value:
            return
        self.llm_session.update_plan_state(
            plan_state.with_updates(
                phase=PlanPhase.NORMAL.value,
                approval_status=PlanApprovalStatus.DRAFT.value,
                approved_artifact_path=None,
                approved_revision=None,
                approved_artifact_hash=None,
                approval_feedback_summary=None,
            )
        )

    def exit_plan_mode(self) -> None:
        """Exit plan mode through the tool-owned approval flow."""
        if not hasattr(self, "llm_session") or self.llm_session is None:
            return
        plan_state = self.llm_session.plan_state
        if plan_state.phase != PlanPhase.PLANNING.value:
            return
        result = self.llm_session.exit_plan_mode_tool(summary=plan_state.summary)
        decision = str(result.data.get("decision") or "").strip().lower()
        if decision == "approve":
            self.console.print(t("plan.approval.approved"), style="green")
        elif decision == "changes_requested":
            self.console.print(t("plan.approval.changes_requested"), style="yellow")
        elif decision == "cancelled":
            self.console.print(t("plan.approval.cancelled"), style="yellow")

    def handle_plan_command(self, user_input: str) -> None:
        parts = self._parse_command_parts(user_input)
        if not parts:
            return

        def _record(exit_code: int, stdout: str = "", stderr: str = "") -> None:
            self._record_special_command_result(
                user_input,
                exit_code=exit_code,
                stdout=stdout,
                stderr=stderr,
            )

        if not hasattr(self, "llm_session") or self.llm_session is None:
            message = t("plan.unavailable")
            self.console.print(message, style="red")
            _record(1, stderr=message)
            return

        if len(parts) > 1 and parts[1] in {"--help", "-h"}:
            message = "Usage: /plan [status]"
            self.console.print(message, style="cyan")
            _record(0, stdout=message)
            return

        plan_state = self.llm_session.plan_state
        subcommand = parts[1] if len(parts) > 1 else ""

        if subcommand == "status":
            mode = "plan" if plan_state.phase == PlanPhase.PLANNING.value else "shell"
            message = (
                f"mode={mode}, approval_status={plan_state.approval_status}, "
                f"artifact={plan_state.artifact_path or '-'}"
            )
            self.console.print(message)
            _record(0, stdout=message)
            return

        if subcommand and subcommand not in {"start", "exit"}:
            message = f"Unknown /plan subcommand: {subcommand}"
            self.console.print(message, style="red")
            _record(1, stderr=message)
            return

        # `/plan` acts as the main command: enter planning from shell or
        # show current plan status when already planning. Explicit subcommands
        # keep direct, unsurprising semantics.
        if plan_state.phase == PlanPhase.PLANNING.value:
            if not subcommand or subcommand == "start":
                # Bare `/plan` while already planning → show status
                mode = "plan"
                message = (
                    f"mode={mode}, approval_status={plan_state.approval_status}, "
                    f"artifact={plan_state.artifact_path or '-'}"
                )
                self.console.print(message)
                _record(0, stdout=message)
            else:
                # `/plan exit` while in plan mode
                self._leave_plan_mode_directly()
                _record(0)
        else:
            if subcommand == "exit":
                message = "mode=shell, approval_status=draft, artifact=-"
                self.console.print(message)
                _record(0, stdout=message)
            else:
                self.llm_session.begin_new_plan()
                _record(0)

    def handle_model_command(self, user_input: str) -> None:
        parts = self._parse_command_parts(user_input)
        if not parts:
            return

        def _report_error(message: str) -> None:
            self.console.print(message, style="red")
            self._record_special_command_result(
                user_input,
                exit_code=1,
                stdout="",
                stderr=message,
            )

        if len(parts) == 1:
            current_model = getattr(self.config, "model", None) or t("shell.model.unset")
            message = t("shell.model.current", model=current_model)
            self.console.print(message)
            self._record_special_command_result(
                user_input,
                exit_code=0,
                stdout=message,
                stderr="",
            )
            return

        if parts[1] in {"--help", "-h"}:
            message = "Usage: /model [model]"
            self.console.print(message, style="cyan")
            self._record_special_command_result(
                user_input,
                exit_code=0,
                stdout=message,
                stderr="",
            )
            return

        new_model = " ".join(parts[1:]).strip()
        if not new_model:
            _report_error(t("shell.model.invalid"))
            return

        if new_model == getattr(self.config, "model", None):
            message = t("shell.model.switch_same", model=new_model)
            self.console.print(message, style="dim")
            self._record_special_command_result(
                user_input,
                exit_code=0,
                stdout=message,
                stderr="",
            )
            return

        self.console.print(t("shell.model.switching", model=new_model), style="dim")
        provider = get_provider_for_model(new_model)

        async def _validate_model_switch() -> str | None:
            return await provider.validate_model_switch(model=new_model, config=self.config)

        try:
            validation_error = anyio.run(_validate_model_switch)
        except Exception as error:
            validation_error = t("shell.model.verify_failed", reason=str(error))

        if validation_error:
            _report_error(validation_error)
            return

        self.llm_session.update_model(
            new_model,
            api_base=getattr(self.config, "api_base", None),
            api_key=getattr(self.config, "api_key", None),
        )
        self.context_manager.set_model(new_model)
        self.config.model = new_model
        if self.session_record is not None:
            self.session_record.model = new_model
        if self.config_manager is not None:
            self.config_manager.set_model(new_model)

        message = t("shell.model.switch_success", model=new_model)
        self.console.print(message, style="green")
        self._record_special_command_result(
            user_input,
            exit_code=0,
            stdout=message,
            stderr="",
        )

    def handle_setup_command(self, user_input: str) -> None:
        parts = self._parse_command_parts(user_input)
        if len(parts) > 1 and parts[1] in {"--help", "-h"}:
            message = "Usage: /setup"
            self.console.print(message, style="cyan")
            self._record_special_command_result(
                user_input,
                exit_code=0,
                stdout=message,
                stderr="",
            )
            return

        if self.config_manager is None:
            message = t("shell.setup.no_config_manager")
            self.console.print(message, style="red")
            self._record_special_command_result(
                user_input,
                exit_code=1,
                stdout="",
                stderr=message,
            )
            return

        new_config = run_interactive_setup(self.config_manager)
        if new_config is None:
            message = t("shell.setup.cancelled")
            self.console.print(message, style="yellow")
            self._record_special_command_result(
                user_input,
                exit_code=0,
                stdout=message,
                stderr="",
            )
            return

        self.llm_session.update_model(
            new_config.model,
            api_base=new_config.api_base,
            api_key=new_config.api_key,
        )
        self.context_manager.set_model(new_config.model)
        self.config.model = new_config.model
        self.config.api_base = new_config.api_base
        self.config.api_key = new_config.api_key
        self.config.is_free_key = new_config.is_free_key
        if self.session_record is not None:
            self.session_record.model = new_config.model

        message = t("shell.setup.applied", model=new_config.model)
        self.console.print(message, style="green")
        self._record_special_command_result(
            user_input,
            exit_code=0,
            stdout=message,
            stderr="",
        )

    def _handle_prompt_submission(self, line: str) -> None:
        line = line.rstrip("\r\n")
        if not line.strip():
            if self._pty_manager is not None:
                self._pty_manager.clear_error_correction()
            return

        if self._prompt_controller is not None:
            self._prompt_controller.remember_command(line)

        if self._is_ai_prefixed_line(line):
            prompt = line[1:].strip()
            if self._ai_handler is None:
                return
            if prompt:
                self._ai_handler.handle_question(prompt)
            else:
                self._ai_handler.handle_error_correction()
            return

        parts = self._parse_command_parts(line)
        if self._is_special_command(parts):
            if parts[0] == "/model":
                self.handle_model_command(line)
            elif parts[0] == "/setup":
                self.handle_setup_command(line)
            elif parts[0] == "/plan":
                self.handle_plan_command(line)
            return

        self.submit_backend_command(line)

    def _drain_backend_io(self, timeout: float = 0.0) -> None:
        if self._pty_manager is None:
            return

        read_fds: list[int] = []
        if self._pty_manager._master_fd is not None:
            read_fds.append(self._pty_manager._master_fd)
        if self._pty_manager.control_fd is not None:
            read_fds.append(self._pty_manager.control_fd)
        if not read_fds:
            return

        try:
            ready, _, _ = select.select(read_fds, [], [], timeout)
        except (ValueError, OSError):
            return

        for fd in ready:
            if self._pty_manager.control_fd is not None and fd == self._pty_manager.control_fd:
                self._handle_control_event()
            elif self._pty_manager._master_fd is not None and fd == self._pty_manager._master_fd:
                self._handle_pty_output()

    def _wait_for_backend_ready(self, timeout: float = 2.0) -> None:
        deadline = time.monotonic() + timeout
        while self._running and not self._backend_session_ready and time.monotonic() < deadline:
            self._drain_backend_io(timeout=0.05)

        if not self._backend_session_ready:
            self._shell_phase = "editing"

    def _passthrough_until_prompt_ready(self) -> None:
        if self._pty_manager is None:
            self._running = False
            return

        try:
            termios.tcflush(sys.stdin.fileno(), termios.TCIFLUSH)
        except Exception:
            pass

        self._set_raw_mode()
        try:
            while self._running and self._shell_phase != "editing":
                read_fds = [sys.stdin.fileno()]
                if self._pty_manager._master_fd is not None:
                    read_fds.append(self._pty_manager._master_fd)
                if self._pty_manager.control_fd is not None:
                    read_fds.append(self._pty_manager.control_fd)

                try:
                    ready, _, _ = select.select(read_fds, [], [], 0.05)
                except (ValueError, OSError):
                    self._running = False
                    break

                for fd in ready:
                    if fd == sys.stdin.fileno():
                        self._handle_passthrough_stdin()
                    elif self._pty_manager.control_fd is not None and fd == self._pty_manager.control_fd:
                        self._handle_control_event()
                    elif self._pty_manager._master_fd is not None and fd == self._pty_manager._master_fd:
                        self._handle_pty_output()
        finally:
            self._restore_terminal()

    def _handle_passthrough_stdin(self) -> None:
        if self._pty_manager is None:
            self._running = False
            return

        try:
            data = os.read(sys.stdin.fileno(), 4096)
        except OSError:
            self._running = False
            return

        if not data:
            self._running = False
            return

        self._pty_manager.send(data)

    @staticmethod
    def _is_exit_command(command: str | None) -> bool:
        """Return whether a command should terminate the current shell."""
        if not command:
            return False
        stripped = command.strip().lower()
        return stripped in SHELL_EXIT_COMMANDS or stripped.startswith(
            tuple(f"{name} " for name in SHELL_EXIT_COMMANDS)
        )

    def _should_exit_on_pty_close(self) -> bool:
        """Detect whether PTY closure came from an explicit user exit."""
        if self._user_requested_exit:
            return True

        if self._pty_manager and self._is_exit_command(self._pty_manager.last_command):
            return True

        return self._is_exit_command(self._pending_command_text)

    def _handle_pty_output(self) -> None:
        try:
            data = os.read(self._pty_manager._master_fd, 1024 * 20)
            if data:
                processed = self._output_processor.process(data)
                if processed:
                    sys.stdout.buffer.write(processed)
                    sys.stdout.buffer.flush()
            else:
                # PTY slave closed (bash exited)
                if self._should_exit_on_pty_close():
                    # User typed an explicit shell-exit command — honor it
                    self._running = False
                elif self._restart_pty():
                    return
                else:
                    self._running = False
        except OSError:
            # PTY error - try to restart
            if self._should_exit_on_pty_close():
                self._running = False
            elif self._restart_pty():
                return
            else:
                self._running = False

    def _record_backend_protocol_error(self, message: str) -> None:
        self._backend_protocol_errors.append(message)
        self._backend_protocol_errors = self._backend_protocol_errors[-20:]

    def _register_submitted_command(self, command: str) -> int:
        seq = self._next_command_seq
        self._next_command_seq += 1
        self._pending_command_seq = seq
        self._pending_command_text = command
        self._shell_phase = "command_submitted"
        return seq

    @staticmethod
    def _normalize_exit_command(command: str) -> str:
        """Map shell-exit aliases to the backend command bash understands."""
        stripped = command.strip()
        if not stripped:
            return stripped

        first_token, separator, remainder = stripped.partition(" ")
        if first_token.lower() == "quit":
            return "exit" if not separator else f"exit {remainder}"

        return stripped

    def submit_backend_command(self, command: str) -> int | None:
        command = command.strip()
        if not command or not self._pty_manager:
            return None

        seq = self._register_submitted_command(command)
        backend_command = self._normalize_exit_command(command)

        is_exit_cmd = self._is_exit_command(command)
        if self._output_processor is not None:
            if is_exit_cmd:
                self._output_processor.set_filter_exit_echo(True)
                self._user_requested_exit = True
            self._output_processor.prepare_user_command_echo(backend_command, seq)

        self._pty_manager.send_command(backend_command, command_seq=seq, source="user")
        return seq

    def _sync_backend_cwd(self, cwd: object) -> None:
        if not isinstance(cwd, str) or not cwd:
            return

        self._current_cwd = cwd
        try:
            os.chdir(cwd)
        except Exception:
            pass

        try:
            self.current_env_info = get_current_env_info()
        except Exception:
            pass

    def _track_backend_event(self, event: BackendControlEvent) -> None:
        self._last_backend_event = event
        self._backend_protocol_events.append(event)
        self._backend_protocol_events = self._backend_protocol_events[-50:]

        self._sync_backend_cwd(event.payload.get("cwd"))

        resolved_command_seq = None
        has_backend_identity = False
        if self._pty_manager is not None and event.type in {"command_started", "prompt_ready"}:
            has_backend_identity = (
                event.payload.get("command_seq") is not None
                or event.payload.get("submission_id") is not None
            )
            resolved_command_seq = self._pty_manager.command_state.resolve_command_seq(
                event.payload.get("command_seq"),
                event.payload.get("submission_id"),
            )

        result = None
        if self._pty_manager is not None:
            result = self._pty_manager.handle_backend_event(event)

        if self._output_processor is not None:
            self._output_processor.handle_backend_event(event, result=result)

        if event.type == "session_ready":
            self._backend_session_ready = True
            self._shell_phase = "editing"
        elif event.type == "command_started":
            command_seq = resolved_command_seq
            if command_seq is None and not has_backend_identity:
                command_seq = self._pending_command_seq

            if self._pending_command_seq is None or command_seq == self._pending_command_seq:
                self._shell_phase = "running_passthrough"
        elif event.type == "prompt_ready":
            command_seq = resolved_command_seq
            if command_seq is None and not has_backend_identity:
                command_seq = self._pending_command_seq

            if self._pending_command_seq is None or command_seq == self._pending_command_seq:
                self._pending_command_seq = None
                self._pending_command_text = None
                self._shell_phase = "editing"
        elif event.type == "shell_exiting":
            self._shell_phase = "recovery_exit"
            self._running = False

    def _handle_control_event(self) -> None:
        if not self._pty_manager or self._pty_manager.control_fd is None:
            return

        try:
            data = os.read(self._pty_manager.control_fd, 4096)
        except OSError as error:
            self._record_backend_protocol_error(f"control channel read failed: {error}")
            return

        if not data:
            self._record_backend_protocol_error("control channel closed")
            return

        events, errors = self._pty_manager.decode_control_events(data)

        for proto_err in errors:
            self._record_backend_protocol_error(proto_err)

        for event in events:
            self._track_backend_event(event)

    def add_shell_history(
        self,
        command: str,
        returncode: int,
        stdout: str,
        stderr: str,
        offload: dict[str, Any] | None = None,
    ) -> None:
        """Add shell execution context for LLM with output previews and offload hints."""
        import json

        # Update last exit code for prompt hooks
        self._last_exit_code = returncode

        # Truncate output previews
        preview_bytes = 4096
        stdout_preview, stdout_truncated = self._truncate_utf8_preview(
            stdout or "", preview_bytes
        )
        stderr_preview, stderr_truncated = self._truncate_utf8_preview(
            stderr or "", preview_bytes
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

        offload_json = json.dumps(offload or {})

        history_entry = "\n".join(
            [
                f"[Shell] {command}",
                f"<returncode>{returncode}</returncode>",
                f"<stdout>{stdout_preview}</stdout>",
                f"<stderr>{stderr_preview}</stderr>",
                f"<offload>{offload_json}</offload>",
            ]
        )
        self.context_manager.add_memory(MemoryType.SHELL, history_entry)

        # Persist to database
        try:
            self.history_manager._add_entry_sync(
                command=command,
                source="user",
                returncode=returncode,
                stdout=stdout_preview[:1024] if stdout_preview else None,
                stderr=stderr_preview[:1024] if stderr_preview else None,
            )
        except Exception:
            pass

    def _truncate_utf8_preview(self, text: str, max_bytes: int) -> tuple[str, bool]:
        """Truncate text to fit within max_bytes while preserving valid UTF-8."""
        if not text:
            return "", False

        encoded = text.encode("utf-8", errors="replace")
        if len(encoded) <= max_bytes:
            return text, False

        # Truncate and decode back, handling potential partial characters
        truncated = encoded[:max_bytes]
        try:
            decoded = truncated.decode("utf-8")
        except UnicodeDecodeError:
            # Remove the last partial character
            decoded = truncated.rstrip().decode("utf-8", errors="ignore")

        return decoded, True

    def _restart_pty(self) -> bool:
        """Restart the PTY after the bash process died (e.g. via exec command).

        Returns True if restart succeeded, False otherwise.
        """
        try:
            # Save current directory before stopping
            old_cwd = self._current_cwd

            # Stop the old PTY
            if self._pty_manager:
                self._pty_manager.stop()

            # Get terminal size
            try:
                size = shutil.get_terminal_size()
                rows, cols = size.lines, size.columns
            except Exception:
                rows, cols = 24, 80

            # Create new PTY
            self._pty_manager = PTYManager(
                rows=rows, cols=cols, cwd=old_cwd, use_output_thread=False,
            )
            self._pty_manager.start()
            time.sleep(0.2)

            # Reconnect components to new PTY manager
            if self._ai_handler:
                self._ai_handler.pty_manager = self._pty_manager
            if self._output_processor:
                self._output_processor.pty_manager = self._pty_manager
            if self.llm_session and hasattr(self.llm_session, "bash_tool"):
                self.llm_session.bash_tool.pty_manager = self._pty_manager

            self._backend_control_buffer = b""
            self._backend_session_ready = False
            self._shell_phase = "booting"
            self._pending_command_seq = None
            self._pending_command_text = None

            # Notify user
            self._restore_terminal()
            self.console.print("[Shell restarted - previous session exited]", style="yellow")
            sys.stdout.flush()

            return True
        except Exception:
            return False

    def _cleanup(self) -> None:
        if not self._running and hasattr(self, "_cleanup_done"):
            return
        self._running = False
        self._cleanup_done = True

        self._stop_animation()
        self._finalize_content_preview()

        if self._pty_manager:
            self._pty_manager.stop()

        if hasattr(self, "history_manager"):
            try:
                self.history_manager.close()
            except Exception:
                pass

        if self.memory_manager:
            try:
                self.memory_manager.close()
            except Exception:
                pass

        self._restore_terminal()
        self.console.print(t("cli.startup.goodbye"))
