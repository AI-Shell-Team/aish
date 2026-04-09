"""AI interaction handling for the PTY shell runtime."""

from __future__ import annotations

import anyio
import asyncio
import os
import re
import sys
from typing import TYPE_CHECKING, Optional

from ...state import ContextManager, MemoryType

from ...i18n import t
from ...prompts import PromptManager

if TYPE_CHECKING:
    from rich.console import Console

    from ...llm import LLMSession
    from ...terminal.pty import PTYManager
    from ...skills import SkillManager
    from .app import PTYAIShell
    from ..ui.interaction import PTYUserInteraction


class AIHandler:
    """Handle AI questions and error correction using LLMSession directly."""

    _SKILL_REF_EXTRACT_RE = re.compile(r"@(\w+)")
    _AUTO_RETAIN_PATTERNS = [
        re.compile(r"^(?:please\s+)?remember(?:\s+that)?\s+(?P<fact>.+)$", re.IGNORECASE),
        re.compile(r"^(?:please\s+)?note(?:\s+that)?\s+(?P<fact>.+)$", re.IGNORECASE),
        re.compile(r"^(?:for\s+future\s+reference[:,]?\s*)(?P<fact>.+)$", re.IGNORECASE),
        re.compile(r"^(?P<fact>i\s+prefer.+)$", re.IGNORECASE),
        re.compile(r"^(?P<fact>my\s+preferred.+)$", re.IGNORECASE),
        re.compile(r"^(?P<fact>we\s+use.+)$", re.IGNORECASE),
        re.compile(r"^(?P<fact>our\s+.+\s+(?:is|are).+)$", re.IGNORECASE),
    ]

    def __init__(
        self,
        pty_manager: "PTYManager",
        llm_session: "LLMSession",
        prompt_manager: PromptManager,
        context_manager: ContextManager,
        skill_manager: "SkillManager",
        user_interaction: "PTYUserInteraction",
        original_termios: Optional[list] = None,
        console: Optional["Console"] = None,
    ):
        self.pty_manager = pty_manager
        self.llm_session = llm_session
        self.prompt_manager = prompt_manager
        self.context_manager = context_manager
        self.skill_manager = skill_manager
        self.user_interaction = user_interaction
        self._original_termios = original_termios
        self.console = console
        self.shell: Optional["PTYAIShell"] = None

    def _require_shell(self) -> "PTYAIShell":
        if self.shell is None:
            raise RuntimeError("AIHandler is not attached to a shell instance")
        return self.shell

    def _restore_terminal_for_output(self) -> None:
        """Temporarily restore terminal settings for AI output."""
        if self._original_termios:
            try:
                import termios

                termios.tcsetattr(
                    sys.stdin.fileno(), termios.TCSADRAIN, self._original_termios
                )
            except Exception:
                pass
        sys.stdout.flush()

    def _set_raw_mode(self) -> None:
        """Re-enter raw mode after AI output."""
        if self._original_termios:
            try:
                import tty

                tty.setraw(sys.stdin.fileno())
            except Exception:
                pass

    @staticmethod
    def _try_parse_json_output(response: str) -> Optional[dict]:
        """Try to parse response as JSON command."""
        import json

        json_match = re.search(r'```(?:json)?\s*(\{.*?\})\s*```', response, re.DOTALL)
        if json_match:
            try:
                return json.loads(json_match.group(1))
            except json.JSONDecodeError:
                pass

        try:
            return json.loads(response.strip())
        except json.JSONDecodeError:
            return None

    @staticmethod
    def _shutdown_loop(loop: asyncio.AbstractEventLoop) -> None:
        pending = [task for task in asyncio.all_tasks(loop) if not task.done()]
        for task in pending:
            task.cancel()

        if pending:
            loop.run_until_complete(asyncio.gather(*pending, return_exceptions=True))

        loop.run_until_complete(loop.shutdown_asyncgens())

        shutdown_default_executor = getattr(loop, "shutdown_default_executor", None)
        if callable(shutdown_default_executor):
            loop.run_until_complete(shutdown_default_executor())

    @staticmethod
    def _run_async_in_thread(coro, cancellation_token=None):
        """Run an async coroutine in a separate thread with its own event loop.

        Uses polling-based cancellation to allow Ctrl+C interruption.
        """
        from concurrent.futures import ThreadPoolExecutor, TimeoutError as FutureTimeoutError

        result_box: list[Optional[str]] = [None]
        exc_box: list[BaseException | None] = [None]

        def run_in_thread():
            loop = asyncio.new_event_loop()
            asyncio.set_event_loop(loop)
            try:
                result_box[0] = loop.run_until_complete(coro)
            except BaseException as e:
                exc_box[0] = e
            finally:
                AIHandler._shutdown_loop(loop)
                loop.close()

        pool = ThreadPoolExecutor(max_workers=1)
        future = pool.submit(run_in_thread)
        try:
            while not future.done():
                try:
                    future.result(timeout=0.2)
                except FutureTimeoutError:
                    # Check if cancellation was requested
                    if cancellation_token and cancellation_token.is_cancelled():
                        raise KeyboardInterrupt("AI operation cancelled by user")
        finally:
            pool.shutdown(wait=False)

        if cancellation_token and cancellation_token.is_cancelled():
            raise KeyboardInterrupt("AI operation cancelled by user")

        # Future is done, get result or raise exception
        if exc_box[0] is not None:
            raise exc_box[0]
        return result_box[0]

    def _extract_skill_refs(self, text: str) -> list[str]:
        """Extract skill references from text."""
        available = {skill.metadata.name for skill in self.skill_manager.list_skills()}
        if not available:
            return []
        refs: list[str] = []
        seen: set[str] = set()
        for match in self._SKILL_REF_EXTRACT_RE.findall(text):
            name = match.lower()
            if name in available and name not in seen:
                refs.append(name)
                seen.add(name)
        return refs

    def _inject_skill_prefix(self, text: str) -> str:
        """Inject skill prefix into text."""
        refs = self._extract_skill_refs(text)
        if not refs:
            return text
        prefix = " ".join([f"use {name} skill to do this." for name in refs])
        return f"{prefix}\n\n{text}"

    def _recall_memories(self, query: str) -> None:
        """Inject relevant memories into context before AI interaction."""
        shell = getattr(self, "shell", None)
        if not shell:
            return
        mem_mgr = getattr(shell, "memory_manager", None)
        if not mem_mgr:
            return
        memory_config = getattr(shell.config, "memory", None)
        if not memory_config or not getattr(memory_config, "auto_recall", False):
            return
        try:
            # Always clear previous recall first to prevent pollution
            self.context_manager.knowledge_cache.pop("memory_recall", None)

            results = mem_mgr.recall(
                query, limit=getattr(memory_config, "recall_limit", 5)
            )
            if results:
                lines = ['<long-term-memory source="recall">']
                for r in results:
                    lines.append(f"- [{r.category.value}] {r.content}")
                lines.append("</long-term-memory>")
                text = "\n".join(lines)

                # Enforce recall_token_budget (~4 chars/token heuristic)
                budget = getattr(memory_config, "recall_token_budget", 512)
                max_chars = budget * 4
                if len(text) > max_chars:
                    text = text[:max_chars].rstrip() + "\n</long-term-memory>"

                self.context_manager.add_memory(
                    MemoryType.KNOWLEDGE,
                    {"key": "memory_recall", "value": text},
                )
        except Exception:
            pass  # Memory recall is best-effort

    def _auto_retain_memory(self, question: str, response: str) -> None:
        """Persist explicit durable facts from the user turn using light heuristics."""
        if not response.strip():
            return

        shell = getattr(self, "shell", None)
        if not shell:
            return
        mem_mgr = getattr(shell, "memory_manager", None)
        if not mem_mgr:
            return
        memory_config = getattr(shell.config, "memory", None)
        if not memory_config or not getattr(memory_config, "auto_retain", False):
            return

        fact = self._extract_retained_fact(question)
        if not fact:
            return

        from aish.memory.models import MemoryCategory

        try:
            mem_mgr.store(
                content=fact,
                category=self._categorize_retained_fact(fact, MemoryCategory),
                source="auto",
                importance=0.7,
            )
        except Exception:
            pass

    def _extract_retained_fact(self, question: str) -> str | None:
        cleaned = " ".join(question.strip().split())
        if not cleaned:
            return None

        for pattern in self._AUTO_RETAIN_PATTERNS:
            match = pattern.match(cleaned)
            if not match:
                continue
            fact = match.group("fact").strip(" .,!;:")
            if 8 <= len(fact) <= 240:
                return fact
        return None

    def _categorize_retained_fact(self, fact: str, memory_category):
        lowered = fact.casefold()
        if any(token in lowered for token in ["prefer", "preferred", "default", "always use"]):
            return memory_category.PREFERENCE
        if any(token in lowered for token in ["pattern", "convention", "style"]):
            return memory_category.PATTERN
        if any(token in lowered for token in ["fix", "solution", "workaround", "resolved"]):
            return memory_category.SOLUTION
        if any(
            token in lowered
            for token in [
                "repo",
                "repository",
                "branch",
                "database",
                "port",
                "path",
                "env",
                "environment",
                "workspace",
                "deploy",
                "server",
                "service",
            ]
        ):
            return memory_category.ENVIRONMENT
        return memory_category.OTHER


    @staticmethod
    def _get_cancel_exceptions() -> tuple[type[BaseException], ...]:
        """Return cancellation exception types available in the current context."""
        try:
            return (
                anyio.get_cancelled_exc_class(),
                asyncio.CancelledError,
                KeyboardInterrupt,
            )
        except Exception:
            return (asyncio.CancelledError, KeyboardInterrupt)

    def _execute_ai_operation(self, coro, shell, history_entry=None):
        """Execute an AI operation with state management and interrupt handling.

        Handles input buffer save, state transitions, temporary SIGINT
        handler, async execution with cancellation, and cleanup.
        """
        from ..interruption import ShellState

        self.llm_session.reset_cancellation_token()
        shell._user_requested_exit = False

        # Save input buffer before AI call for potential restore
        current_cmd = ""
        if hasattr(shell, "get_edit_buffer_text"):
            current_cmd = shell.get_edit_buffer_text()
        if current_cmd:
            shell.interruption_manager.save_input_buffer(current_cmd)

        shell.interruption_manager.set_state(ShellState.AI_THINKING)
        shell.operation_in_progress = True

        # Record to history
        if history_entry:
            try:
                shell.history_manager._add_entry_sync(**history_entry)
            except Exception:
                pass

        # Install temporary SIGINT handler since main thread is blocked
        # in cooked mode during AI call. Without this, Ctrl+C generates
        # SIGINT which kills the shell entirely.
        import signal as _signal

        def _sigint_handler(signum, frame):
            _ = (signum, frame)
            shell._on_interrupt_requested()

        _prev_sigint = _signal.getsignal(_signal.SIGINT)
        _signal.signal(_signal.SIGINT, _sigint_handler)
        response = None
        was_cancelled = False
        cancel_exceptions = self._get_cancel_exceptions()
        try:
            response = self._run_async_in_thread(
                coro, cancellation_token=self.llm_session.cancellation_token
            )
        except cancel_exceptions:
            was_cancelled = True
            shell.handle_processing_cancelled()
        finally:
            _signal.signal(_signal.SIGINT, _prev_sigint)
            shell.interruption_manager.set_state(ShellState.NORMAL)
            shell.operation_in_progress = False

        return response, was_cancelled

    def handle_error_correction(self) -> None:
        """Handle error correction."""
        if not getattr(self.pty_manager, "can_correct_last_error", False):
            print("\r\033[KNo previous error to fix.")
            return

        if self.pty_manager.last_exit_code in (0, 130):
            print("\r\033[KNo previous error to fix.")
            return

        cmd = self.pty_manager.last_command
        if not cmd:
            print("\r\033[KNo previous command to fix.")
            return

        try:
            self._restore_terminal_for_output()

            async def _fix():
                with self.llm_session.cancellation_token.open_cancel_scope():
                    system_message = self.prompt_manager.substitute_template(
                        "cmd_error",
                        user_nickname=os.getenv("USER", "user"),
                        uname_info=getattr(self, "uname_info", ""),
                        os_info=getattr(self, "os_info", ""),
                        basic_env_info=getattr(self, "basic_env_info", ""),
                        output_language=getattr(self, "output_language", "en"),
                    )

                    prompt = f"""<command_result>
Command: {cmd}
Exit code: {self.pty_manager.last_exit_code}
</command_result>

Please analyze the error and suggest a fix. Check the shell history context above for the actual error output."""

                    response = await self.llm_session.process_input(
                        prompt,
                        context_manager=self.context_manager,
                        system_message=system_message,
                        stream=True,
                    )

                    return response

            shell = self._require_shell()
            response, was_cancelled = self._execute_ai_operation(
                _fix(),
                shell,
                history_entry={
                    "command": f"[error_fix] {cmd}",
                    "source": "ai",
                    "returncode": None,
                    "stdout": None,
                    "stderr": None,
                },
            )

            if was_cancelled:
                return

            if response:
                corrected_cmd = self._display_ai_response(response)
                if corrected_cmd:
                    self._ask_execute_command(corrected_cmd)

        except Exception as error:
            print(f"\r\033[KError: {error}")

    def handle_question(self, question: str) -> None:
        """Handle AI question."""
        try:
            self._restore_terminal_for_output()

            async def _ask():
                with self.llm_session.cancellation_token.open_cancel_scope():
                    system_message = self.prompt_manager.substitute_template(
                        "oracle",
                        user_nickname=os.getenv("USER", "user"),
                        uname_info=getattr(self, "uname_info", ""),
                        os_info=getattr(self, "os_info", ""),
                        basic_env_info=getattr(self, "basic_env_info", ""),
                        output_language=getattr(self, "output_language", "en"),
                    )

                    question_processed = self._inject_skill_prefix(question)

                    # Recall: inject relevant memories before AI call
                    self._recall_memories(question_processed)

                    response = await self.llm_session.process_input(
                        question_processed,
                        context_manager=self.context_manager,
                        system_message=system_message,
                        stream=True,
                    )

                    return response

            shell = self._require_shell()
            response, was_cancelled = self._execute_ai_operation(
                _ask(),
                shell,
                history_entry={
                    "command": question,
                    "source": "ai",
                    "returncode": None,
                    "stdout": None,
                    "stderr": None,
                },
            )

            if was_cancelled:
                return

            if response:
                self._display_ai_response(response)
                self._auto_retain_memory(question, response)

        except Exception as error:
            print(f"\r\033[KError: {error}")

    def _get_console(self):
        """Get the shared Console instance, falling back to a new one if needed."""
        if self.console is not None:
            return self.console
        from rich.console import Console

        self.console = Console(force_terminal=True)
        return self.console

    def _display_ai_response(self, response: str) -> Optional[str]:
        """Display AI response, handling JSON command format."""
        from rich.box import HORIZONTALS
        from rich.markdown import Markdown
        from rich.panel import Panel

        console = self._get_console()

        json_cmd = self._try_parse_json_output(response)
        if json_cmd:
            if json_cmd.get("type") == "corrected_command":
                command = json_cmd.get("command", "").strip()
                description = json_cmd.get("description", "")
                if not command:
                    console.print(
                        f"[yellow]⚠ {t('shell.error_correction.no_valid_command')}[/yellow]"
                    )
                    if description:
                        clean_desc = description.split("Insufficient context")[0].strip()
                        if clean_desc:
                            console.print(f"   {clean_desc}")
                    console.print(
                        f"   [cyan]{t('shell.error_correction.retry_hint')}[/cyan]"
                    )
                    sys.stdout.flush()
                    sys.stderr.flush()
                    console.show_cursor()
                    return None
                console.print(
                    f"{t('shell.error_correction.corrected_command_title')} [bold cyan]{command}[/bold cyan]"
                )
                if description:
                    console.print(f"   {description}")
                sys.stdout.flush()
                sys.stderr.flush()
                return command

            console.print(Panel(Markdown(response), border_style="green", box=HORIZONTALS))
            sys.stdout.flush()
            sys.stderr.flush()
            console.show_cursor()
            return None

        console.print(Panel(Markdown(response), border_style="green", box=HORIZONTALS))
        sys.stdout.flush()
        sys.stderr.flush()
        console.show_cursor()
        return None

    def _ask_execute_command(self, command: str) -> bool:
        """Ask user if they want to execute the corrected command."""
        confirmed = self.user_interaction.get_confirmation(
            f"{t('shell.error_correction.confirm_execute_prefix')}\033[1;36m{command}\033[0m{t('shell.error_correction.confirm_execute_suffix')}"
        )
        if confirmed:
            shell = self._require_shell()
            return bool(shell.submit_ai_backend_command(command))
        return False