"""Focused unit tests for the PTY shell core."""

from __future__ import annotations

import os
import threading
from types import SimpleNamespace

import pytest
from unittest.mock import Mock
from unittest.mock import call

from aish.memory.config import MemoryConfig
from aish.memory.models import MemoryCategory
from aish.i18n import t
from aish.plan import PlanApprovalStatus, PlanPhase
from aish.terminal.pty.command_state import CommandResult, CommandState
from aish.terminal.pty.control_protocol import BackendControlEvent
from aish.terminal.pty.manager import PTYManager
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
        self.startup_ready = False
        self.startup_cwd = None
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

    @property
    def command_state(self) -> CommandState:
        return self._command_state


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
    shell.content_was_streamed = False
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
def test_ai_handler_handles_single_question_without_host_followup():
    handler, shell = _make_ai_handler()

    def _complete_operation(coro, shell, history_entry=None):
        _ = shell
        coro.close()
        return ("first-response", False)

    handler._execute_ai_operation = Mock(side_effect=_complete_operation)
    handler._display_ai_response = Mock()
    handler._auto_retain_memory = Mock()
    shell.consume_pending_ai_followup = Mock(return_value={"prompt": "ignored"})

    handler.handle_question("hello")

    assert handler._execute_ai_operation.call_count == 1
    history_entry = handler._execute_ai_operation.call_args.kwargs["history_entry"]
    assert history_entry["command"] == "hello"
    shell.consume_pending_ai_followup.assert_not_called()
    handler._display_ai_response.assert_called_once_with("first-response")


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


def test_output_processor_filters_quit_echo():
    processor = OutputProcessor(_FakePTYManager())
    processor.set_filter_exit_echo(True)

    assert processor.process(b"\rquit\r\n") == b""


def test_output_processor_filters_user_command_echo():
    processor = OutputProcessor(_FakePTYManager())
    processor.prepare_user_command_echo("pwd", 5)

    rendered = processor.process(b"pwd\r\n")

    assert rendered == b""


def test_output_processor_filters_user_command_echo_before_command_output():
    processor = OutputProcessor(_FakePTYManager())
    processor.prepare_user_command_echo("pwd", 5)

    rendered = processor.process(b"pwd\r\n/tmp/project\r\n")

    assert rendered == b"/tmp/project\r\n"


def test_setup_pty_reuses_manager_startup_handshake(monkeypatch, tmp_path):
    frontend_cwd = str(tmp_path / "frontend")
    backend_cwd = str(tmp_path / "backend")
    sleep_mock = Mock()

    class _StartedPTYManager:
        def __init__(self, *, rows: int, cols: int, cwd: str, use_output_thread: bool):
            assert rows == 24
            assert cols == 80
            assert cwd == frontend_cwd
            assert use_output_thread is False
            self.startup_ready = True
            self.startup_cwd = backend_cwd

        def start(self) -> None:
            return None

    def _terminal_size(*_args, **_kwargs):
        return os.terminal_size((80, 24))

    monkeypatch.setattr("aish.shell.runtime.app.PTYManager", _StartedPTYManager)
    monkeypatch.setattr("aish.shell.runtime.app.shutil.get_terminal_size", _terminal_size)
    monkeypatch.setattr("aish.shell.runtime.app.time.sleep", sleep_mock)
    monkeypatch.setattr("aish.shell.runtime.app.get_current_env_info", lambda: "env-info")
    monkeypatch.setattr("aish.shell.runtime.app.os.chdir", lambda _cwd: None)

    shell = object.__new__(PTYAIShell)
    shell._current_cwd = frontend_cwd
    shell._backend_session_ready = False
    shell._shell_phase = "booting"

    PTYAIShell._setup_pty(shell)

    assert shell._pty_manager is not None
    assert shell._current_cwd == backend_cwd
    assert shell.current_env_info == "env-info"
    assert shell._backend_session_ready is True
    assert shell._shell_phase == "editing"
    sleep_mock.assert_not_called()


def test_setup_pty_preserves_startup_session_ready_without_prompt(monkeypatch, tmp_path):
    frontend_cwd = str(tmp_path / "frontend")
    backend_cwd = str(tmp_path / "backend")
    sleep_mock = Mock()

    class _StartedPTYManager:
        def __init__(self, *, rows: int, cols: int, cwd: str, use_output_thread: bool):
            assert rows == 24
            assert cols == 80
            assert cwd == frontend_cwd
            assert use_output_thread is False
            self.startup_session_ready = True
            self.startup_ready = False
            self.startup_cwd = backend_cwd

        def start(self) -> None:
            return None

    def _terminal_size(*_args, **_kwargs):
        return os.terminal_size((80, 24))

    monkeypatch.setattr("aish.shell.runtime.app.PTYManager", _StartedPTYManager)
    monkeypatch.setattr("aish.shell.runtime.app.shutil.get_terminal_size", _terminal_size)
    monkeypatch.setattr("aish.shell.runtime.app.time.sleep", sleep_mock)
    monkeypatch.setattr("aish.shell.runtime.app.get_current_env_info", lambda: "env-info")
    monkeypatch.setattr("aish.shell.runtime.app.os.chdir", lambda _cwd: None)

    shell = object.__new__(PTYAIShell)
    shell._current_cwd = frontend_cwd
    shell._backend_session_ready = False
    shell._shell_phase = "booting"

    PTYAIShell._setup_pty(shell)

    assert shell._pty_manager is not None
    assert shell._current_cwd == backend_cwd
    assert shell._backend_session_ready is True
    assert shell._shell_phase == "booting"
    sleep_mock.assert_called_once_with(0.2)


def test_output_processor_filters_split_user_command_echo_across_chunks():
    processor = OutputProcessor(_FakePTYManager())
    processor.prepare_user_command_echo("pwd", 5)

    first = processor.process(b"pw")
    second = processor.process(b"d\r\n")

    assert first == b""
    assert second == b""


def test_output_processor_filters_split_user_command_echo_before_output():
    processor = OutputProcessor(_FakePTYManager())
    processor.prepare_user_command_echo("pwd", 5)

    first = processor.process(b"pw")
    second = processor.process(b"d\r\n/tmp/project\r\n")

    assert first == b""
    assert second == b"/tmp/project\r\n"


def test_output_processor_does_not_strip_nonleading_command_text():
    processor = OutputProcessor(_FakePTYManager())
    processor.prepare_user_command_echo("pwd", 5)

    rendered = processor.process(b"status: pwd\r\n")

    assert rendered == b"status: pwd\r\n"


def test_output_processor_filters_user_command_echo_after_async_output():
    processor = OutputProcessor(_FakePTYManager())
    processor.prepare_user_command_echo("pwd", 5)

    first = processor.process(b"[1]+ Done sleep 0.1\r\n")
    second = processor.process(b"pwd\r\n/tmp/project\r\n")

    assert first == b"[1]+ Done sleep 0.1\r\n"
    assert second == b"/tmp/project\r\n"


def test_output_processor_clears_pending_echo_on_prompt_ready():
    processor = OutputProcessor(_FakePTYManager())
    processor.prepare_user_command_echo("pwd", 5)

    processor.handle_backend_event(
        BackendControlEvent(
            version=1,
            type="prompt_ready",
            ts=1,
            payload={"exit_code": 0},
        ),
        result=CommandResult(command="pwd", exit_code=0, source="user"),
    )

    rendered = processor.process(b"pwd\r\n")

    assert rendered == b"pwd\r\n"


def test_pty_manager_send_command_serializes_metadata_and_pty_write():
    manager = PTYManager()
    manager._running = True
    manager._master_fd = 1

    events: list[tuple[str, str]] = []
    first_metadata_written = threading.Event()
    release_first = threading.Event()

    def fake_write_command_metadata(
        *, command: str, source: str, command_seq: int | None, submission_id: str | None = None
    ) -> None:
        _ = (source, command_seq, submission_id)
        events.append(("meta", command))
        if command == "one":
            first_metadata_written.set()
            release_first.wait(timeout=1)

    def fake_send(data: bytes) -> int:
        events.append(("send", data.decode("utf-8").strip()))
        return len(data)

    manager._write_command_metadata = fake_write_command_metadata
    manager.send = fake_send

    first = threading.Thread(
        target=manager.send_command,
        kwargs={"command": "one", "command_seq": 1},
    )
    second = threading.Thread(
        target=manager.send_command,
        kwargs={"command": "two", "command_seq": 2},
    )

    first.start()
    assert first_metadata_written.wait(timeout=1)

    second.start()
    threading.Event().wait(0.05)
    assert events == [("meta", "one")]

    release_first.set()
    first.join(timeout=1)
    second.join(timeout=1)

    assert events == [
        ("meta", "one"),
        ("send", "one"),
        ("meta", "two"),
        ("send", "two"),
    ]


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
    pty_manager.consume_error.assert_called_once_with()
    assert t("shell.error_correction.press_semicolon_hint") in capsys.readouterr().out


def test_pty_manager_send_command_sends_plain_user_command():
    manager = object.__new__(PTYManager)
    manager._command_state = CommandState()
    manager._completed_results = []
    manager._completion_condition = threading.Condition()
    manager._exit_code_callback = None
    manager._lock = threading.RLock()
    sent: list[bytes] = []

    def _fake_send(data: bytes) -> int:
        sent.append(data)
        return len(data)

    manager.send = _fake_send  # type: ignore[method-assign]

    PTYManager.send_command(manager, "echo hi", command_seq=7, source="user")
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

    assert sent == [b"echo hi\n"]
    assert result is not None
    assert manager.last_command == "echo hi"


def test_pty_manager_send_command_prefixes_backend_with_space_only():
    manager = object.__new__(PTYManager)
    manager._command_state = CommandState()
    manager._completed_results = []
    manager._completion_condition = threading.Condition()
    manager._exit_code_callback = None
    manager._lock = threading.RLock()
    sent: list[bytes] = []

    def _fake_send(data: bytes) -> int:
        sent.append(data)
        return len(data)

    manager.send = _fake_send  # type: ignore[method-assign]

    PTYManager.send_command(manager, "history | tail -n 5", command_seq=-1, source="backend")

    assert sent == [b" history | tail -n 5\n"]


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


def test_shell_tracks_phase_without_mutating_missing_command_seq_events():
    shell = object.__new__(PTYAIShell)
    shell._pty_manager = _FakePTYManager()
    shell._backend_protocol_events = []
    shell._backend_protocol_errors = []
    shell._last_backend_event = None
    shell._backend_session_ready = False
    shell._shell_phase = "booting"
    shell._next_command_seq = 1
    shell._pending_command_seq = 1
    shell._pending_command_text = "pwd"
    shell._running = True
    shell._output_processor = Mock()
    shell._pty_manager.command_state.register_command("pwd", source="user", command_seq=1)

    started = BackendControlEvent(
        version=1,
        type="command_started",
        ts=1,
        payload={"command": "pwd"},
    )
    PTYAIShell._track_backend_event(shell, started)

    assert shell._shell_phase == "running_passthrough"
    assert "command_seq" not in started.payload

    ready = BackendControlEvent(
        version=1,
        type="prompt_ready",
        ts=2,
        payload={"exit_code": 0},
    )
    PTYAIShell._track_backend_event(shell, ready)

    assert shell._shell_phase == "editing"
    assert shell._pending_command_seq is None
    assert shell._pending_command_text is None
    assert "command_seq" not in ready.payload


def test_shell_ignores_unmatched_identified_backend_events():
    shell = object.__new__(PTYAIShell)
    shell._pty_manager = _FakePTYManager()
    shell._backend_protocol_events = []
    shell._backend_protocol_errors = []
    shell._last_backend_event = None
    shell._backend_session_ready = False
    shell._shell_phase = "command_submitted"
    shell._next_command_seq = 1
    shell._pending_command_seq = 1
    shell._pending_command_text = "pwd"
    shell._running = True
    shell._output_processor = Mock()
    shell._pty_manager.command_state.register_command("pwd", source="user", command_seq=1)

    started = BackendControlEvent(
        version=1,
        type="command_started",
        ts=1,
        payload={"submission_id": "subm_missing", "command": "echo other"},
    )
    PTYAIShell._track_backend_event(shell, started)

    assert shell._shell_phase == "command_submitted"
    assert shell._pending_command_seq == 1
    assert shell._pending_command_text == "pwd"

    ready = BackendControlEvent(
        version=1,
        type="prompt_ready",
        ts=2,
        payload={"submission_id": "subm_missing", "exit_code": 0},
    )
    PTYAIShell._track_backend_event(shell, ready)

    assert shell._shell_phase == "command_submitted"
    assert shell._pending_command_seq == 1
    assert shell._pending_command_text == "pwd"


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
def test_handle_tool_execution_end_reports_plan_approval():
    shell = object.__new__(PTYAIShell)
    shell.console = Mock()

    PTYAIShell.handle_tool_execution_end(
        shell,
        SimpleNamespace(
            data={
                "tool_name": "exit_plan_mode",
                "result_data": {"decision": "approve"},
            }
        ),
    )

    shell.console.print.assert_called_once_with(
        t("plan.approval.approved"),
        style="green",
    )


@pytest.mark.timeout(5)
def test_handle_tool_execution_end_reports_plan_changes_requested():
    shell = object.__new__(PTYAIShell)
    shell.console = Mock()

    PTYAIShell.handle_tool_execution_end(
        shell,
        SimpleNamespace(
            data={
                "tool_name": "exit_plan_mode",
                "result_data": {"decision": "changes_requested"},
            }
        ),
    )

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
    shell._output_processor.prepare_user_command_echo.assert_called_once_with("pwd", 3)
    shell._pty_manager.send_command.assert_called_once_with(
        "pwd", command_seq=3, source="user"
    )


def test_shell_submit_backend_command_treats_quit_as_exit():
    shell = object.__new__(PTYAIShell)
    shell._pty_manager = Mock()
    shell._output_processor = Mock()
    shell._next_command_seq = 8
    shell._pending_command_seq = None
    shell._pending_command_text = None
    shell._shell_phase = "editing"
    shell._user_requested_exit = False

    seq = PTYAIShell.submit_backend_command(shell, "quit")

    assert seq == 8
    assert shell._user_requested_exit is True
    shell._output_processor.set_filter_exit_echo.assert_called_once_with(True)
    shell._output_processor.set_waiting_for_result.assert_not_called()
    shell._output_processor.prepare_user_command_echo.assert_called_once_with(
        "exit", 8
    )
    shell._pty_manager.send_command.assert_called_once_with(
        "exit", command_seq=8, source="user"
    )


def test_shell_submit_backend_command_maps_quit_exit_code_argument():
    shell = object.__new__(PTYAIShell)
    shell._pty_manager = Mock()
    shell._output_processor = Mock()
    shell._next_command_seq = 11
    shell._pending_command_seq = None
    shell._pending_command_text = None
    shell._shell_phase = "editing"
    shell._user_requested_exit = False

    seq = PTYAIShell.submit_backend_command(shell, "quit 7")

    assert seq == 11
    shell._pty_manager.send_command.assert_called_once_with(
        "exit 7", command_seq=11, source="user"
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


def test_shell_does_not_restart_after_explicit_quit_when_flag_was_not_set(monkeypatch):
    shell = object.__new__(PTYAIShell)
    shell._pty_manager = _FakePTYManager(last_command="quit")
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


def test_ttft_timing_records_on_first_content_delta():
    """Test that TTFT is recorded when first content delta arrives after operation start."""
    import time

    shell = object.__new__(PTYAIShell)
    shell.console = Mock()
    shell._stop_animation = Mock()
    shell._reset_reasoning_state = Mock()
    shell._finalize_content_preview = Mock()
    shell._start_animation = Mock()
    shell.current_live = None
    shell._at_line_start = True
    shell._last_streaming_accumulated = ""
    shell._content_preview_active = False

    # Initialize timing state
    shell._timing_active = False
    shell._thinking_start_time = 0.0
    shell._ttft = 0.0
    shell._ttft_recorded = False

    # Simulate operation start
    PTYAIShell.handle_operation_start(shell, Mock())

    assert shell._timing_active is True
    assert shell._thinking_start_time > 0
    assert shell._ttft == 0.0
    assert shell._ttft_recorded is False

    # Small delay to ensure measurable time
    time.sleep(0.01)

    # Simulate first content delta - should record TTFT
    PTYAIShell.handle_content_delta(shell, Mock(data={"delta": "Hello"}))

    assert shell._ttft > 0
    assert shell._ttft_recorded is True


def test_non_final_content_delta_does_not_mark_content_streamed():
    shell = object.__new__(PTYAIShell)
    shell.console = Mock()
    shell._stop_animation = Mock()
    shell._reset_reasoning_state = Mock()
    shell.current_live = None
    shell._at_line_start = True
    shell._last_streaming_accumulated = ""
    shell._content_preview_active = False
    shell._content_streamed_to_terminal = False
    shell._thinking_start_time = 0.0
    shell._ttft = 0.0
    shell._ttft_recorded = False

    PTYAIShell.handle_content_delta(
        shell,
        Mock(data={"delta": "Working on it", "is_final": False}),
    )

    assert shell._content_streamed_to_terminal is False
    shell.console.print.assert_called_once()


def test_final_content_delta_marks_content_streamed():
    shell = object.__new__(PTYAIShell)
    shell.console = Mock()
    shell._stop_animation = Mock()
    shell._reset_reasoning_state = Mock()
    shell.current_live = None
    shell._at_line_start = True
    shell._last_streaming_accumulated = ""
    shell._content_preview_active = False
    shell._content_streamed_to_terminal = False
    shell._thinking_start_time = 0.0
    shell._ttft = 0.0
    shell._ttft_recorded = False

    PTYAIShell.handle_content_delta(
        shell,
        Mock(data={"delta": "Final answer", "is_final": True}),
    )

    assert shell._content_streamed_to_terminal is True
    shell.console.print.assert_called_once()


def test_non_final_content_delta_does_not_record_ttft():
    shell = object.__new__(PTYAIShell)
    shell.console = Mock()
    shell._stop_animation = Mock()
    shell._reset_reasoning_state = Mock()
    shell.current_live = None
    shell._at_line_start = True
    shell._last_streaming_accumulated = ""
    shell._content_preview_active = False
    shell._content_streamed_to_terminal = False
    shell._thinking_start_time = 1.0
    shell._ttft = 0.0
    shell._ttft_recorded = False

    PTYAIShell.handle_content_delta(
        shell,
        Mock(data={"delta": "Preview before tool", "is_final": False}),
    )

    assert shell._ttft == 0.0
    assert shell._ttft_recorded is False


def test_op_end_does_not_render_ttft_for_non_final_preview_only():
    shell = object.__new__(PTYAIShell)
    shell.console = Mock()
    shell._stop_animation = Mock()
    shell._reset_reasoning_state = Mock()
    shell.current_live = None
    shell._at_line_start = True
    shell._last_streaming_accumulated = ""
    shell._content_preview_active = False
    shell._content_streamed_to_terminal = False
    shell._timing_active = True
    shell._thinking_start_time = 1.0
    shell._ttft = 0.0
    shell._ttft_recorded = False

    PTYAIShell.handle_content_delta(
        shell,
        Mock(data={"delta": "Preview before tool", "is_final": False}),
    )
    PTYAIShell.handle_op_end(shell, Mock())

    timing_calls = [
        call for call in shell.console.print.call_args_list
        if "思考:" in str(call)
    ]
    assert timing_calls == []


def test_ttft_timing_preserves_state_across_generations(monkeypatch):
    """Test that timing state is not reset between generations in multi-generation scenarios."""
    import time

    monkeypatch.setattr("aish.shell.runtime.app.Live", _FakeLive)

    shell = object.__new__(PTYAIShell)
    shell.console = Mock()
    shell._stop_animation = Mock()
    shell._reset_reasoning_state = Mock()
    shell._finalize_content_preview = Mock()
    shell._start_animation = Mock()
    shell.current_live = None
    shell._at_line_start = True
    shell._last_streaming_accumulated = ""
    shell._content_preview_active = False

    # Initialize timing state
    shell._timing_active = False
    shell._thinking_start_time = 0.0
    shell._ttft = 0.0
    shell._ttft_recorded = False

    # Simulate operation start
    PTYAIShell.handle_operation_start(shell, Mock())
    start_time = shell._thinking_start_time

    # Small delay
    time.sleep(0.01)

    # First generation: content delta records TTFT
    PTYAIShell.handle_content_delta(shell, Mock(data={"delta": "First response"}))

    first_ttft = shell._ttft
    assert first_ttft > 0
    assert shell._ttft_recorded is True

    # First generation ends
    PTYAIShell.handle_generation_end(shell, Mock())

    # Verify timing state is NOT reset
    assert shell._thinking_start_time == start_time
    assert shell._ttft == first_ttft
    assert shell._ttft_recorded is True

    # Second generation starts (tool call scenario)
    PTYAIShell.handle_generation_start(shell, Mock())

    # Verify thinking_start_time is NOT reset
    assert shell._thinking_start_time == start_time
    assert shell._ttft == first_ttft
    assert shell._ttft_recorded is True

    # Second generation ends
    PTYAIShell.handle_generation_end(shell, Mock())

    # Still not reset
    assert shell._thinking_start_time == start_time
    assert shell._ttft == first_ttft
    assert shell._ttft_recorded is True


def test_ttft_timing_summary_renders_at_op_end_not_generation_end():
    """Test that timing summary is printed at OP_END, not at GENERATION_END."""
    shell = object.__new__(PTYAIShell)
    shell.console = Mock()
    shell._stop_animation = Mock()
    shell._reset_reasoning_state = Mock()
    shell._finalize_content_preview = Mock()
    shell._start_animation = Mock()
    shell.current_live = Mock()
    shell._at_line_start = True
    shell._last_streaming_accumulated = ""

    # Initialize timing state with TTFT above threshold
    shell._timing_active = True
    shell._thinking_start_time = 1.0
    shell._ttft = 0.5  # Above _TTFT_DISPLAY_THRESHOLD (0.1)
    shell._ttft_recorded = True

    # Call generation_end - should NOT print timing summary
    PTYAIShell.handle_generation_end(shell, Mock())

    # Verify console.print was NOT called with timing text
    timing_calls = [
        call for call in shell.console.print.call_args_list
        if "思考:" in str(call) and "0.5s" in str(call)
    ]
    assert len(timing_calls) == 0

    # Call op_end - SHOULD print timing summary
    PTYAIShell.handle_op_end(shell, Mock())

    # Verify console.print was called with timing text
    shell.console.print.assert_called()


def test_ttft_timing_state_reset_after_op_end():
    """Test that timing state is properly reset after operation end."""
    shell = object.__new__(PTYAIShell)
    shell.console = Mock()
    shell._stop_animation = Mock()
    shell._reset_reasoning_state = Mock()
    shell._finalize_content_preview = Mock()
    shell._start_animation = Mock()
    shell.current_live = None
    shell._at_line_start = True
    shell._last_streaming_accumulated = ""

    # Initialize timing state
    shell._timing_active = True
    shell._thinking_start_time = 1.0
    shell._ttft = 0.5
    shell._ttft_recorded = True

    # Call op_end - should reset timing state
    PTYAIShell.handle_op_end(shell, Mock())

    # Verify all timing state is reset
    assert shell._timing_active is False
    assert shell._thinking_start_time == 0.0
    assert shell._ttft == 0.0
    assert shell._ttft_recorded is False


def test_ttft_timing_no_summary_below_threshold():
    """Test that timing summary is not printed when TTFT is below threshold."""
    shell = object.__new__(PTYAIShell)
    shell.console = Mock()
    shell._stop_animation = Mock()
    shell._reset_reasoning_state = Mock()
    shell._finalize_content_preview = Mock()
    shell._start_animation = Mock()
    shell.current_live = None
    shell._at_line_start = True
    shell._last_streaming_accumulated = ""

    # Initialize timing state with TTFT below threshold
    shell._timing_active = True
    shell._thinking_start_time = 1.0
    shell._ttft = 0.05  # Below _TTFT_DISPLAY_THRESHOLD (0.1)
    shell._ttft_recorded = True

    # Call op_end - should NOT print timing summary
    PTYAIShell.handle_op_end(shell, Mock())

    # Verify console.print was NOT called with timing text
    timing_calls = [
        call for call in shell.console.print.call_args_list
        if len(call[0]) > 0 and "思考:" in str(call[0][0])
    ]
    assert len(timing_calls) == 0


def test_ttft_thinking_start_does_not_modify_timing_state():
    """Test that handle_thinking_start does not modify _thinking_start_time if already set."""
    import time

    shell = object.__new__(PTYAIShell)
    shell.console = Mock()
    shell._stop_animation = Mock()
    shell._reset_reasoning_state = Mock()
    shell._finalize_content_preview = Mock()
    shell._start_animation = Mock()
    shell.current_live = None
    shell._at_line_start = True
    shell._last_streaming_accumulated = ""

    # Initialize timing state via operation start
    shell._timing_active = False
    shell._thinking_start_time = 0.0
    shell._ttft = 0.0
    shell._ttft_recorded = False

    PTYAIShell.handle_operation_start(shell, Mock())
    original_start_time = shell._thinking_start_time

    # Small delay
    time.sleep(0.01)

    # Call thinking_start - should NOT modify _thinking_start_time
    PTYAIShell.handle_thinking_start(shell, Mock())

    assert shell._thinking_start_time == original_start_time
