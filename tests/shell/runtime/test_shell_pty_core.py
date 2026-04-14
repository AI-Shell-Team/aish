"""Focused unit tests for the PTY shell core."""

from __future__ import annotations

import os
import threading
from types import SimpleNamespace

import pytest
from unittest.mock import Mock
from unittest.mock import call

from aish.config import ConfigModel
from aish.llm import LLMSession
from aish.memory.config import MemoryConfig
from aish.memory.models import MemoryCategory
from aish.i18n import t
from aish.plan import PlanApprovalStatus, PlanPhase
from aish.terminal.pty.command_state import CommandResult, CommandState
from aish.terminal.pty.control_protocol import BackendControlEvent
from aish.terminal.pty.manager import PTYManager
from aish.terminal.interaction import (
    InteractionAnswer,
    InteractionAnswerType,
    InteractionResponse,
    InteractionStatus,
)
from aish.skills import SkillManager
from aish.shell.runtime.ai import AIHandler
from aish.shell.runtime.app import PTYAIShell
from aish.shell.runtime.output import OutputProcessor


class _FakePTYManager:
    def __init__(
        self,
        *,
        last_command: str = "",
        last_exit_code: int = 0,
        error_info=None,
    ):
        self.sent: list[bytes] = []
        self._master_fd = 1
        self._control_buffer = b""
        self._command_state = CommandState()
        self._completed_results: list[CommandResult] = []
        self._completion_condition = threading.Condition()
        self._exit_code_callback = None
        self._error_info = error_info
        self.last_command = last_command
        self.last_exit_code = last_exit_code
        self.register_user_command = Mock(side_effect=self._remember_user_command)
        self.clear_error_correction = Mock(side_effect=self._clear_error_correction)
        self.consume_error = Mock(side_effect=self._consume_error)
        self.handle_backend_event = Mock(side_effect=self._handle_backend_event)

    def send(self, data: bytes) -> int:
        self.sent.append(data)
        return len(data)

    def _remember_user_command(self, command: str) -> None:
        self._command_state.register_user_command(command)
        self.last_command = command

    def _clear_error_correction(self) -> None:
        self._command_state.clear_error_correction()

    def _consume_error(self):
        if self._error_info is not None:
            error_info = self._error_info
            self._error_info = None
            return error_info
        return self._command_state.consume_error()

    def _handle_backend_event(self, event: BackendControlEvent):
        result = PTYManager.handle_backend_event(self, event)
        self.last_command = self._command_state.last_command
        self.last_exit_code = self._command_state.last_exit_code
        return result

    @property
    def can_correct_last_error(self) -> bool:
        return self._command_state.can_correct_last_error


def _make_ai_handler() -> tuple[AIHandler, Mock]:
    pty_manager = _FakePTYManager()
    llm_session = Mock()
    llm_session.cancellation_token = Mock()
    prompt_manager = Mock()
    prompt_manager.substitute_template.return_value = "system"
    skill_manager = Mock()
    skill_manager.list_skills.return_value = []
    user_interaction = Mock()

    handler = AIHandler(
        pty_manager=pty_manager,
        llm_session=llm_session,
        prompt_manager=prompt_manager,
        context_manager=Mock(),
        skill_manager=skill_manager,
        user_interaction=user_interaction,
    )

    shell = Mock()
    shell.get_edit_buffer_text.return_value = ""
    shell.interruption_manager = Mock()
    shell.history_manager = Mock()
    shell.handle_processing_cancelled = Mock()
    shell._on_interrupt_requested = Mock()
    shell.submit_backend_command = Mock()
    shell.submit_ai_backend_command = Mock(return_value=True)
    shell.operation_in_progress = False
    handler.shell = shell
    return handler, shell


def test_ai_handler_skips_prompt_redraw_when_question_is_cancelled():
    handler, shell = _make_ai_handler()

    def _cancel_operation(coro, shell, history_entry=None):
        _ = (shell, history_entry)
        coro.close()
        return (None, True)

    handler._execute_ai_operation = Mock(side_effect=_cancel_operation)
    handler._display_ai_response = Mock()

    handler.handle_question("hello")

    handler._display_ai_response.assert_not_called()
    shell.submit_backend_command.assert_not_called()


@pytest.mark.timeout(5)
def test_ai_handler_runs_pending_followup_after_current_question():
    handler, shell = _make_ai_handler()

    def _complete_operation(coro, shell, history_entry=None):
        _ = shell
        coro.close()
        if history_entry and history_entry["command"] == "[plan approved] continue implementation":
            return ("second-response", False)
        return ("first-response", False)

    handler._execute_ai_operation = Mock(side_effect=_complete_operation)
    handler._display_ai_response = Mock()
    handler._auto_retain_memory = Mock()
    shell.consume_pending_ai_followup = Mock(
        side_effect=[
            {
                "prompt": "Implement the approved plan now.",
                "history_command": "[plan approved] continue implementation",
            },
            None,
        ]
    )

    handler.handle_question("hello")

    assert handler._execute_ai_operation.call_count == 2
    first_history = handler._execute_ai_operation.call_args_list[0].kwargs["history_entry"]
    second_history = handler._execute_ai_operation.call_args_list[1].kwargs["history_entry"]
    assert first_history["command"] == "hello"
    assert second_history["command"] == "[plan approved] continue implementation"
    handler._display_ai_response.assert_has_calls(
        [call("first-response"), call("second-response")]
    )


def test_ai_handler_executes_corrected_command_via_security_submission():
    handler, shell = _make_ai_handler()

    result = handler._ask_execute_command("rm -rf /tmp/demo")

    assert result is True
    shell.submit_ai_backend_command.assert_called_once_with("rm -rf /tmp/demo")
    shell.submit_backend_command.assert_not_called()


def test_ai_handler_marks_cancelled_operation_and_notifies_shell():
    handler, shell = _make_ai_handler()
    handler._run_async_in_thread = Mock(
        side_effect=KeyboardInterrupt("AI operation cancelled by user")
    )

    response, was_cancelled = handler._execute_ai_operation(object(), shell)

    assert response is None
    assert was_cancelled is True
    shell.handle_processing_cancelled.assert_called_once_with()


def test_ai_handler_auto_retain_persists_explicit_fact():
    handler, shell = _make_ai_handler()
    shell.memory_manager = Mock()
    shell.config = Mock(memory=MemoryConfig(auto_retain=True))

    handler._auto_retain_memory(
        "Remember that the production database runs on port 5432.",
        "Understood.",
    )

    shell.memory_manager.store.assert_called_once_with(
        content="the production database runs on port 5432",
        category=MemoryCategory.ENVIRONMENT,
        source="auto",
        importance=0.7,
    )


def test_ai_handler_auto_retain_ignores_regular_questions():
    handler, shell = _make_ai_handler()
    shell.memory_manager = Mock()
    shell.config = Mock(memory=MemoryConfig(auto_retain=True))

    handler._auto_retain_memory(
        "Why does the production database run on port 5432?",
        "Because of the current deployment configuration.",
    )

    shell.memory_manager.store.assert_not_called()


def test_ai_handler_refuses_error_correction_for_interactive_session_exit(capsys):
    handler, _ = _make_ai_handler()
    handler.pty_manager.register_user_command("ssh root@example.com")
    handler.pty_manager.handle_backend_event(
        BackendControlEvent(
            version=1,
            type="command_started",
            ts=1,
            payload={"command": "ssh root@example.com"},
        )
    )
    handler.pty_manager.handle_backend_event(
        BackendControlEvent(
            version=1,
            type="prompt_ready",
            ts=2,
            payload={"exit_code": 255},
        )
    )

    handler.handle_error_correction()

    assert "No previous error to fix." in capsys.readouterr().out


def test_output_processor_filters_exit_echo():
    processor = OutputProcessor(_FakePTYManager())
    processor.set_filter_exit_echo(True)

    assert processor.process(b"\rexit\r\n") == b""


def test_output_processor_filters_prefixed_user_command_echo():
    processor = OutputProcessor(_FakePTYManager())
    processor.prepare_user_command_echo("pwd", 5)

    rendered = processor.process(
        b" __AISH_ACTIVE_COMMAND_SEQ=5; __AISH_ACTIVE_COMMAND_TEXT=pwd; pwd\r\n"
    )

    assert rendered == b""


def test_output_processor_filters_prefixed_user_command_echo_before_command_output():
    processor = OutputProcessor(_FakePTYManager())
    processor.prepare_user_command_echo("pwd", 5)

    rendered = processor.process(
        b" __AISH_ACTIVE_COMMAND_SEQ=5; __AISH_ACTIVE_COMMAND_TEXT=pwd; pwd\r\n/tmp/project\r\n"
    )

    assert rendered == b"/tmp/project\r\n"


class _FakeLive:
    def __init__(self, *args, **kwargs):
        self.started = False

    def start(self):
        self.started = True

    def stop(self):
        self.started = False

    def update(self, *args, **kwargs):
        return None


def test_handle_thinking_start_skips_blank_line_when_already_at_line_start(monkeypatch):
    shell = object.__new__(PTYAIShell)
    shell._stop_animation = Mock()
    shell._last_streaming_accumulated = ""
    shell._last_reasoning_render_lines = []
    shell.current_live = None
    shell.console = Mock()
    shell._at_line_start = True
    shell._start_animation = Mock()

    monkeypatch.setattr("aish.shell.runtime.app.Live", _FakeLive)

    PTYAIShell.handle_thinking_start(shell, Mock())

    shell.console.print.assert_not_called()
    assert isinstance(shell.current_live, _FakeLive)
    shell._start_animation.assert_called_once_with(base_text="思考中", pattern="braille")


def test_handle_thinking_start_adds_blank_line_when_not_at_line_start(monkeypatch):
    shell = object.__new__(PTYAIShell)
    shell._stop_animation = Mock()
    shell._last_streaming_accumulated = ""
    shell._last_reasoning_render_lines = []
    shell.current_live = None
    shell.console = Mock()
    shell._at_line_start = False
    shell._start_animation = Mock()

    monkeypatch.setattr("aish.shell.runtime.app.Live", _FakeLive)

    PTYAIShell.handle_thinking_start(shell, Mock())

    shell.console.print.assert_called_once_with()
    assert shell._at_line_start is True


def test_handle_error_event_uses_rich_style_output():
    shell = object.__new__(PTYAIShell)
    shell.console = Mock()
    shell._finalize_content_preview = Mock()
    shell._reset_reasoning_state = Mock()
    shell._last_streaming_accumulated = "pending"
    shell.current_live = None

    PTYAIShell.handle_error_event(
        shell,
        Mock(data={"error_message": "InternalServerError: Connection error."}),
    )

    shell.console.print.assert_called_once_with(
        t("shell.error.llm_error_message", error="InternalServerError: Connection error."),
        style="red",
    )
    shell._finalize_content_preview.assert_called_once_with()
    shell._reset_reasoning_state.assert_called_once_with()
    assert shell._last_streaming_accumulated == ""


def test_restart_notification_uses_rich_style_output(monkeypatch):
    class _FakePTY:
        def __init__(self, *args, **kwargs):
            self.started = False
            self.stopped = False

        def start(self):
            self.started = True

        def stop(self):
            self.stopped = True

    monkeypatch.setattr("aish.shell.runtime.app.PTYManager", _FakePTY)
    monkeypatch.setattr(
        "aish.shell.runtime.app.shutil.get_terminal_size",
        lambda fallback=None: os.terminal_size((80, 24)),
    )
    monkeypatch.setattr("aish.shell.runtime.app.time.sleep", lambda _: None)

    shell = object.__new__(PTYAIShell)
    shell.llm_session = Mock()
    shell.llm_session.bash_tool = Mock()
    shell.llm_session.bash_tool.pty_manager = None
    shell._ai_handler = None
    shell._output_processor = None
    shell._pty_manager = Mock()
    shell._backend_control_buffer = b""
    shell._backend_session_ready = False
    shell._shell_phase = "running"
    shell._pending_command_seq = 1
    shell._pending_command_text = "pwd"
    shell._current_cwd = "/tmp"
    shell.console = Mock()
    shell._restore_terminal = Mock()

    result = PTYAIShell._restart_pty(shell)

    assert result is True
    shell.console.print.assert_called_once_with(
        "[Shell restarted - previous session exited]", style="yellow"
    )


def test_output_processor_prints_error_hint_when_command_fails(capsys):
    pty_manager = _FakePTYManager(error_info=("bad command", 1))
    processor = OutputProcessor(pty_manager)
    processor._waiting_for_result = True

    rendered = processor.process(b"stderr output")
    processor.handle_backend_event(
        BackendControlEvent(
            version=1,
            type="prompt_ready",
            ts=1,
            payload={"exit_code": 1},
        ),
        result=CommandResult(command="bad command", exit_code=1, source="user"),
    )

    assert rendered == b"stderr output"
    assert processor._waiting_for_result is False
    pty_manager.consume_error.assert_called_once_with()
    assert t("shell.error_correction.press_semicolon_hint") in capsys.readouterr().out


def test_pty_manager_send_command_injects_command_seq():
    manager = object.__new__(PTYManager)
    manager._command_state = CommandState()
    manager._completed_results = []
    manager._completion_condition = threading.Condition()
    manager._exit_code_callback = None
    sent: list[bytes] = []

    def _fake_send(data: bytes) -> int:
        sent.append(data)
        return len(data)

    manager.send = _fake_send  # type: ignore[method-assign]

    PTYManager.send_command(manager, "echo hi", command_seq=7)
    result = PTYManager.handle_backend_event(
        manager,
        BackendControlEvent(
            version=1,
            type="command_started",
            ts=1,
            payload={"command_seq": 7, "command": "echo hi"},
        ),
    )
    assert result is None
    result = PTYManager.handle_backend_event(
        manager,
        BackendControlEvent(
            version=1,
            type="prompt_ready",
            ts=2,
            payload={"command_seq": 7, "exit_code": 0},
        ),
    )

    assert sent == [
        b" __AISH_ACTIVE_COMMAND_SEQ=7; __AISH_ACTIVE_COMMAND_TEXT='echo hi'; echo hi\n"
    ]
    assert result is not None
    assert manager.last_command == "echo hi"


def test_shell_tracks_command_seq_and_returns_to_editing_on_prompt_ready():
    shell = object.__new__(PTYAIShell)
    shell._pty_manager = _FakePTYManager()
    shell._backend_protocol_events = []
    shell._backend_protocol_errors = []
    shell._last_backend_event = None
    shell._backend_session_ready = False
    shell._shell_phase = "booting"
    shell._next_command_seq = 1
    shell._pending_command_seq = None
    shell._pending_command_text = None
    shell._running = True
    shell._output_processor = Mock()

    seq = PTYAIShell._register_submitted_command(shell, "pwd")

    assert seq == 1
    assert shell._shell_phase == "command_submitted"
    assert shell._pending_command_seq == 1

    started = BackendControlEvent(
        version=1,
        type="command_started",
        ts=1,
        payload={"command_seq": 1, "command": "pwd"},
    )
    PTYAIShell._track_backend_event(shell, started)
    assert shell._shell_phase == "running_passthrough"

    ready = BackendControlEvent(
        version=1,
        type="prompt_ready",
        ts=2,
        payload={"command_seq": 1, "exit_code": 0},
    )
    PTYAIShell._track_backend_event(shell, ready)

    assert shell._shell_phase == "editing"
    assert shell._pending_command_seq is None
    assert shell._pending_command_text is None
    assert shell._output_processor.handle_backend_event.call_count == 2


def test_shell_tracks_backend_cwd_from_prompt_ready(monkeypatch):
    shell = object.__new__(PTYAIShell)
    shell._pty_manager = _FakePTYManager()
    shell._backend_protocol_events = []
    shell._backend_protocol_errors = []
    shell._last_backend_event = None
    shell._backend_session_ready = False
    shell._shell_phase = "booting"
    shell._next_command_seq = 1
    shell._pending_command_seq = None
    shell._pending_command_text = None
    shell._running = True
    shell._output_processor = Mock()
    shell._current_cwd = "/old"
    shell.current_env_info = "old-env"

    chdir_mock = Mock()
    get_env_mock = Mock(return_value="new-env")
    monkeypatch.setattr("aish.shell.runtime.app.os.chdir", chdir_mock)
    monkeypatch.setattr("aish.shell.runtime.app.get_current_env_info", get_env_mock)

    ready = BackendControlEvent(
        version=1,
        type="prompt_ready",
        ts=2,
        payload={"exit_code": 0, "cwd": "/tmp/project"},
    )

    PTYAIShell._track_backend_event(shell, ready)

    assert shell._current_cwd == "/tmp/project"
    assert shell.current_env_info == "new-env"
    chdir_mock.assert_called_once_with("/tmp/project")
    get_env_mock.assert_called_once_with()


def test_shell_handle_prompt_submission_routes_semicolon_question_to_ai_handler():
    shell = object.__new__(PTYAIShell)
    shell._pty_manager = Mock()
    shell._ai_handler = Mock()
    shell._prompt_controller = Mock()
    shell.submit_backend_command = Mock()

    PTYAIShell._handle_prompt_submission(shell, ";hello there")

    shell._prompt_controller.remember_command.assert_called_once_with(";hello there")
    shell._ai_handler.handle_question.assert_called_once_with("hello there")
    shell._ai_handler.handle_error_correction.assert_not_called()
    shell.submit_backend_command.assert_not_called()


def test_shell_handle_prompt_submission_routes_bare_semicolon_to_error_correction():
    shell = object.__new__(PTYAIShell)
    shell._pty_manager = Mock()
    shell._ai_handler = Mock()
    shell._prompt_controller = Mock()
    shell.submit_backend_command = Mock()

    PTYAIShell._handle_prompt_submission(shell, ";")

    shell._prompt_controller.remember_command.assert_called_once_with(";")
    shell._ai_handler.handle_error_correction.assert_called_once_with()
    shell._ai_handler.handle_question.assert_not_called()
    shell.submit_backend_command.assert_not_called()


def test_shell_handle_prompt_submission_blank_line_clears_error_state():
    shell = object.__new__(PTYAIShell)
    shell._pty_manager = Mock()
    shell._ai_handler = Mock()
    shell._prompt_controller = Mock()
    shell.submit_backend_command = Mock()

    PTYAIShell._handle_prompt_submission(shell, "   ")

    shell._pty_manager.clear_error_correction.assert_called_once_with()
    shell._prompt_controller.remember_command.assert_not_called()
    shell.submit_backend_command.assert_not_called()


def test_shell_prompt_for_command_merges_backslash_continuation_for_command():
    shell = object.__new__(PTYAIShell)
    shell._prompt_controller = Mock()
    shell._prompt_controller.prompt.side_effect = ["echo hello \\", "world"]
    shell._restore_terminal = Mock()
    shell._editing_buffer_text = ""
    shell._shell_phase = "editing"
    shell.console = Mock()
    shell._running = True
    shell._at_line_start = False

    result = PTYAIShell._prompt_for_command(shell)

    assert result == "echo hello world"
    assert shell._prompt_controller.prompt.call_args_list == [call(), call("... ")]
    assert shell._at_line_start is True


def test_shell_prompt_for_command_merges_backslash_continuation_for_ai_prompt():
    shell = object.__new__(PTYAIShell)
    shell._prompt_controller = Mock()
    shell._prompt_controller.prompt.side_effect = [";你好 \\", "继续说"]
    shell._restore_terminal = Mock()
    shell._editing_buffer_text = ""
    shell._shell_phase = "editing"
    shell.console = Mock()
    shell._running = True
    shell._at_line_start = False

    result = PTYAIShell._prompt_for_command(shell)

    assert result == ";你好\n继续说"
    assert shell._prompt_controller.prompt.call_args_list == [call(), call("... ")]


def test_shell_handle_prompt_submission_routes_model_command_to_special_handler():
    shell = object.__new__(PTYAIShell)
    shell._pty_manager = Mock()
    shell._ai_handler = Mock()
    shell._prompt_controller = Mock()
    shell.submit_backend_command = Mock()
    shell.handle_model_command = Mock()
    shell.handle_setup_command = Mock()

    PTYAIShell._handle_prompt_submission(shell, "/model foo/bar")

    shell._prompt_controller.remember_command.assert_called_once_with("/model foo/bar")
    shell.handle_model_command.assert_called_once_with("/model foo/bar")
    shell.handle_setup_command.assert_not_called()
    shell.submit_backend_command.assert_not_called()


def test_shell_handle_prompt_submission_routes_setup_command_to_special_handler():
    shell = object.__new__(PTYAIShell)
    shell._pty_manager = Mock()
    shell._ai_handler = Mock()
    shell._prompt_controller = Mock()
    shell.submit_backend_command = Mock()
    shell.handle_model_command = Mock()
    shell.handle_setup_command = Mock()

    PTYAIShell._handle_prompt_submission(shell, "/setup")

    shell._prompt_controller.remember_command.assert_called_once_with("/setup")
    shell.handle_setup_command.assert_called_once_with("/setup")
    shell.handle_model_command.assert_not_called()
    shell.submit_backend_command.assert_not_called()


@pytest.mark.timeout(5)
def test_shell_handle_prompt_submission_routes_plan_command_to_special_handler():
    shell = object.__new__(PTYAIShell)
    shell._pty_manager = Mock()
    shell._ai_handler = Mock()
    shell._prompt_controller = Mock()
    shell.submit_backend_command = Mock()
    shell.handle_model_command = Mock()
    shell.handle_setup_command = Mock()
    shell.handle_plan_command = Mock()

    PTYAIShell._handle_prompt_submission(shell, "/plan start")

    shell._prompt_controller.remember_command.assert_called_once_with("/plan start")
    shell.handle_plan_command.assert_called_once_with("/plan start")
    shell.handle_model_command.assert_not_called()
    shell.handle_setup_command.assert_not_called()
    shell.submit_backend_command.assert_not_called()


@pytest.mark.timeout(5)
def test_shell_toggle_plan_mode_enters_plan_when_in_shell_mode():
    shell = object.__new__(PTYAIShell)
    shell.llm_session = Mock(plan_state=SimpleNamespace(phase=PlanPhase.NORMAL.value))
    shell.exit_plan_mode = Mock()
    shell._leave_plan_mode_directly = Mock()

    PTYAIShell.toggle_plan_mode(shell)

    shell.llm_session.begin_new_plan.assert_called_once_with()
    shell.exit_plan_mode.assert_not_called()
    shell._leave_plan_mode_directly.assert_not_called()


@pytest.mark.timeout(5)
def test_shell_toggle_plan_mode_exits_plan_when_already_planning():
    shell = object.__new__(PTYAIShell)
    shell.llm_session = Mock(plan_state=SimpleNamespace(phase=PlanPhase.PLANNING.value))
    shell.exit_plan_mode = Mock()
    shell._leave_plan_mode_directly = Mock()

    PTYAIShell.toggle_plan_mode(shell)

    shell._leave_plan_mode_directly.assert_called_once_with()
    shell.exit_plan_mode.assert_not_called()
    shell.llm_session.begin_new_plan.assert_not_called()


@pytest.mark.timeout(5)
def test_leave_plan_mode_directly_resets_planning_state_without_approval():
    shell = object.__new__(PTYAIShell)
    plan_state = Mock()
    plan_state.phase = PlanPhase.PLANNING.value
    plan_state.with_updates.return_value = "updated-plan-state"
    shell.llm_session = Mock(plan_state=plan_state)

    PTYAIShell._leave_plan_mode_directly(shell)

    shell.llm_session.update_plan_state.assert_called_once_with("updated-plan-state")
    plan_state.with_updates.assert_called_once_with(
        phase=PlanPhase.NORMAL.value,
        approval_status=PlanApprovalStatus.DRAFT.value,
        approved_artifact_path=None,
        approved_revision=None,
        approved_artifact_hash=None,
        approval_feedback_summary=None,
    )


@pytest.mark.timeout(5)
def test_handle_plan_command_exit_leaves_plan_mode_without_approval():
    shell = object.__new__(PTYAIShell)
    shell.console = Mock()
    shell.llm_session = Mock(plan_state=SimpleNamespace(phase=PlanPhase.PLANNING.value))
    shell.exit_plan_mode = Mock()
    shell._leave_plan_mode_directly = Mock()
    shell._record_special_command_result = Mock()

    PTYAIShell.handle_plan_command(shell, "/plan exit")

    shell._leave_plan_mode_directly.assert_called_once_with()
    shell.exit_plan_mode.assert_not_called()
    shell.llm_session.begin_new_plan.assert_not_called()


@pytest.mark.timeout(5)
def test_handle_plan_command_start_shows_status_when_already_planning():
    shell = object.__new__(PTYAIShell)
    shell.console = Mock()
    shell.llm_session = Mock(
        plan_state=SimpleNamespace(
            phase=PlanPhase.PLANNING.value,
            approval_status=PlanApprovalStatus.DRAFT.value,
            artifact_path="/tmp/plan.md",
        )
    )
    shell._record_special_command_result = Mock()

    PTYAIShell.handle_plan_command(shell, "/plan start")

    shell.console.print.assert_called_once_with(
        "mode=plan, approval_status=draft, artifact=/tmp/plan.md"
    )
    shell._record_special_command_result.assert_called_once_with(
        "/plan start",
        exit_code=0,
        stdout="mode=plan, approval_status=draft, artifact=/tmp/plan.md",
        stderr="",
    )


@pytest.mark.timeout(5)
def test_handle_plan_command_exit_in_shell_mode_is_noop_status():
    shell = object.__new__(PTYAIShell)
    shell.console = Mock()
    shell.llm_session = Mock(plan_state=SimpleNamespace(phase=PlanPhase.NORMAL.value))
    shell._record_special_command_result = Mock()

    PTYAIShell.handle_plan_command(shell, "/plan exit")

    shell.console.print.assert_called_once_with("mode=shell, approval_status=draft, artifact=-")
    shell.llm_session.begin_new_plan.assert_not_called()


@pytest.mark.timeout(5)
def test_handle_tool_execution_end_approves_plan_signal():
    shell = object.__new__(PTYAIShell)
    shell.console = Mock()
    shell.context_manager = Mock()
    shell.queue_pending_ai_followup = Mock()
    shell.llm_session = LLMSession(
        config=ConfigModel(model="test-model", api_key="test-key"),
        skill_manager=SkillManager(),
        session_uuid="shell-plan-approve",
    )
    shell.llm_session.begin_new_plan()
    artifact = shell.llm_session.plan_state.artifact
    assert artifact is not None
    artifact.write_text("# Plan\n\nApproved", encoding="utf-8")
    shell.llm_session.request_interaction = lambda request: InteractionResponse(
        interaction_id="approval-approve",
        status=InteractionStatus.SUBMITTED,
        answer=InteractionAnswer(
            type=InteractionAnswerType.OPTION,
            value="approve",
            label="Approve",
        ),
    )

    PTYAIShell.handle_tool_execution_end(
        shell,
        SimpleNamespace(
            data={
                "tool_name": "exit_plan_mode",
                "result_data": {
                    "signal": "exit_plan_mode",
                    "artifact_path": str(artifact),
                    "artifact_preview": "# Plan\n\nApproved",
                    "summary": "ready",
                },
            }
        ),
    )

    assert shell.llm_session.plan_state.phase == PlanPhase.NORMAL.value
    assert (
        shell.llm_session.plan_state.approval_status
        == PlanApprovalStatus.APPROVED.value
    )
    assert shell.llm_session.plan_state.approved_artifact_path
    shell.context_manager.add_memory.assert_called_once()
    shell.console.print.assert_called_once_with(
        t("plan.approval.approved"),
        style="green",
    )
    shell.queue_pending_ai_followup.assert_called_once()
    queued_prompt = shell.queue_pending_ai_followup.call_args.kwargs["prompt"]
    assert "Implement the approved plan now." in queued_prompt
    assert str(shell.llm_session.plan_state.approved_artifact_path) in queued_prompt


@pytest.mark.timeout(5)
def test_handle_tool_execution_end_keeps_plan_mode_when_changes_requested():
    shell = object.__new__(PTYAIShell)
    shell.console = Mock()
    shell.context_manager = Mock()
    shell.llm_session = LLMSession(
        config=ConfigModel(model="test-model", api_key="test-key"),
        skill_manager=SkillManager(),
        session_uuid="shell-plan-feedback",
    )
    shell.llm_session.begin_new_plan()
    artifact = shell.llm_session.plan_state.artifact
    assert artifact is not None
    artifact.write_text("# Plan\n\nNeeds work", encoding="utf-8")
    shell.llm_session.request_interaction = lambda request: InteractionResponse(
        interaction_id="approval-feedback",
        status=InteractionStatus.SUBMITTED,
        answer=InteractionAnswer(
            type=InteractionAnswerType.TEXT,
            value="Split deployment and validation into separate steps",
        ),
    )

    PTYAIShell.handle_tool_execution_end(
        shell,
        SimpleNamespace(
            data={
                "tool_name": "exit_plan_mode",
                "result_data": {
                    "signal": "exit_plan_mode",
                    "artifact_path": str(artifact),
                    "artifact_preview": "# Plan\n\nNeeds work",
                    "summary": "ready",
                },
            }
        ),
    )

    assert shell.llm_session.plan_state.phase == PlanPhase.PLANNING.value
    assert (
        shell.llm_session.plan_state.approval_status
        == PlanApprovalStatus.CHANGES_REQUESTED.value
    )
    assert shell.llm_session.plan_state.approval_feedback_summary == (
        "Split deployment and validation into separate steps"
    )
    shell.context_manager.add_memory.assert_called_once()
    shell.console.print.assert_called_once_with(
        t("plan.approval.changes_requested"),
        style="yellow",
    )


def test_shell_handle_model_command_reports_current_model():
    shell = object.__new__(PTYAIShell)
    shell.console = Mock()
    shell.config = Mock(model="demo/model")
    shell._record_special_command_result = Mock()

    PTYAIShell.handle_model_command(shell, "/model")

    shell.console.print.assert_called_once_with(t("shell.model.current", model="demo/model"))
    shell._record_special_command_result.assert_called_once_with(
        "/model",
        exit_code=0,
        stdout=t("shell.model.current", model="demo/model"),
        stderr="",
    )


def test_shell_submit_backend_command_registers_user_seq():
    shell = object.__new__(PTYAIShell)
    shell._pty_manager = Mock()
    shell._output_processor = Mock()
    shell._next_command_seq = 3
    shell._pending_command_seq = None
    shell._pending_command_text = None
    shell._shell_phase = "editing"
    shell._user_requested_exit = False

    seq = PTYAIShell.submit_backend_command(shell, "pwd")

    assert seq == 3
    assert shell._pending_command_seq == 3
    assert shell._pending_command_text == "pwd"
    assert shell._shell_phase == "command_submitted"
    shell._output_processor.set_waiting_for_result.assert_called_once_with(True, "pwd")
    shell._pty_manager.send_command.assert_called_once_with(
        "pwd", command_seq=3, source="user"
    )


def test_shell_submit_ai_backend_command_skips_confirmation_for_approved_command(monkeypatch):
    shell = object.__new__(PTYAIShell)
    shell._pty_manager = Mock()
    shell._approved_ai_commands = {"echo ok"}
    shell.submit_backend_command = Mock(return_value=9)
    shell.console = Mock()
    shell.interruption_manager = Mock()
    shell.security_manager = Mock()
    shell._current_cwd = "/tmp"

    result = PTYAIShell.submit_ai_backend_command(shell, "echo ok")

    assert result is True
    shell.submit_backend_command.assert_called_once_with("echo ok")
    shell.security_manager.decide.assert_not_called()


def test_shell_submit_ai_backend_command_blocks_high_risk_command(monkeypatch):
    shell = object.__new__(PTYAIShell)
    shell._pty_manager = Mock()
    shell._approved_ai_commands = set()
    shell.submit_backend_command = Mock()
    shell.console = Mock()
    shell.interruption_manager = Mock()
    shell._current_cwd = "/tmp"
    shell.security_manager = Mock(
        decide=Mock(return_value=Mock(allow=False, require_confirmation=False, analysis={"risk_level": "HIGH"}))
    )

    captured: list[tuple[dict[str, object], str]] = []
    monkeypatch.setattr(
        "aish.shell.runtime.app.display_security_panel",
        lambda shell_obj, data, panel_mode="confirm": captured.append((data, panel_mode)),
    )

    result = PTYAIShell.submit_ai_backend_command(shell, "rm -rf /tmp/demo")

    assert result is False
    shell.submit_backend_command.assert_not_called()
    assert captured
    assert captured[0][1] == "blocked"
    assert captured[0][0]["command"] == "rm -rf /tmp/demo"


def test_shell_submit_ai_backend_command_confirms_then_executes(monkeypatch):
    from aish.llm import LLMCallbackResult

    shell = object.__new__(PTYAIShell)
    shell._pty_manager = Mock()
    shell._approved_ai_commands = set()
    shell.submit_backend_command = Mock(return_value=5)
    shell.console = Mock()
    shell.interruption_manager = Mock()
    shell._current_cwd = "/tmp"
    shell.security_manager = Mock(
        decide=Mock(return_value=Mock(allow=True, require_confirmation=True, analysis={"risk_level": "MEDIUM"}))
    )

    captured: list[tuple[dict[str, object], str]] = []
    monkeypatch.setattr(
        "aish.shell.runtime.app.display_security_panel",
        lambda shell_obj, data, panel_mode="confirm": captured.append((data, panel_mode)),
    )
    monkeypatch.setattr(
        "aish.shell.runtime.app.get_user_confirmation",
        lambda shell_obj, remember_command=None, allow_remember=False: LLMCallbackResult.APPROVE,
    )

    result = PTYAIShell.submit_ai_backend_command(shell, "rm -rf /tmp/demo")

    assert result is True
    shell.submit_backend_command.assert_called_once_with("rm -rf /tmp/demo")
    assert captured
    assert captured[0][1] == "confirm"


def test_create_llm_session_wires_is_command_approved(monkeypatch):
    import aish.llm as llm_module

    captured: dict[str, object] = {}

    class _FakeSession:
        def __init__(self, **kwargs):
            captured.update(kwargs)
            self._sync_init_lock = Mock()
            self._initialized = False

        def _get_litellm(self):
            return None

        def _get_acompletion(self):
            return None

    class _FakeThread:
        def __init__(self, target=None, daemon=None):
            self._target = target

        def start(self):
            return None

    monkeypatch.setattr(llm_module, "LLMSession", _FakeSession)
    monkeypatch.setattr("aish.shell.runtime.app.threading.Thread", _FakeThread)

    shell = object.__new__(PTYAIShell)
    shell.config = Mock()
    shell.skill_manager = Mock()
    shell.handle_llm_event = Mock()
    shell.history_manager = Mock()
    shell._approved_ai_commands = set()
    shell._is_command_approved = PTYAIShell._is_command_approved.__get__(shell, PTYAIShell)
    shell._on_interrupt_requested = Mock()

    session = PTYAIShell._create_llm_session(shell)

    assert session is not None
    assert captured["is_command_approved"] is shell._is_command_approved


def test_shell_does_not_restart_after_explicit_exit_when_flag_was_not_set(monkeypatch):
    shell = object.__new__(PTYAIShell)
    shell._pty_manager = _FakePTYManager(last_command="exit")
    shell._output_processor = Mock()
    shell._pending_command_text = None
    shell._user_requested_exit = False
    shell._running = True
    shell._restart_pty = Mock(return_value=True)

    monkeypatch.setattr("aish.shell.runtime.app.os.read", lambda fd, size: b"")

    PTYAIShell._handle_pty_output(shell)

    assert shell._running is False
    shell._restart_pty.assert_not_called()


def test_backend_error_suppressed_prevents_repeated_hints(capsys):
    pty_manager = _FakePTYManager()
    processor = OutputProcessor(pty_manager)
    processor.handle_backend_event(
        BackendControlEvent(
            version=1,
            type="prompt_ready",
            ts=1,
            payload={"exit_code": 127},
        ),
        result=CommandResult(command="ipaw", exit_code=127, source="backend"),
    )
    captured = capsys.readouterr()
    assert t("shell.error_correction.press_semicolon_hint") not in captured.out


def test_user_command_error_shows_hint_exactly_once(capsys):
    pty_manager = _FakePTYManager(error_info=("bad_cmd", 1))
    processor = OutputProcessor(pty_manager)
    processor._waiting_for_result = True
    processor.handle_backend_event(
        BackendControlEvent(
            version=1,
            type="prompt_ready",
            ts=1,
            payload={"exit_code": 1},
        ),
        result=CommandResult(command="bad_cmd", exit_code=1, source="user"),
    )
    captured = capsys.readouterr()
    assert t("shell.error_correction.press_semicolon_hint") in captured.out

    processor.handle_backend_event(
        BackendControlEvent(
            version=1,
            type="prompt_ready",
            ts=2,
            payload={"exit_code": 1},
        ),
        result=None,
    )
    captured = capsys.readouterr()
    assert t("shell.error_correction.press_semicolon_hint") not in captured.out
