from __future__ import annotations

import io
import termios
import time
import pytest
from unittest.mock import patch

from rich.console import Console

from aish.plan.approval import PlanApprovalRequestBuilder
from aish.terminal.interaction import AskUserRequestBuilder
from aish.llm import LLMCallbackResult, LLMEvent, LLMEventType
from aish.shell.ui.prompt_io import (
    display_security_panel,
    handle_interaction_required,
    get_user_confirmation,
    render_interaction_modal,
    handle_tool_confirmation_required,
)


def _reset_i18n_cache() -> None:
    import aish.i18n as i18n

    i18n._UI_LOCALE = None  # type: ignore[attr-defined]
    i18n._MESSAGES = None  # type: ignore[attr-defined]
    i18n._MESSAGES_EN = None  # type: ignore[attr-defined]


class _DummyShell:
    def __init__(self) -> None:
        self.current_live = None
        self.console = Console(file=io.StringIO(), force_terminal=False, width=120)
        self._remembered_commands: list[str] = []
        self.llm_session = type(
            "_DummyLLMSession",
            (),
            {
                "cancellation_token": type(
                    "_Token",
                    (),
                    {"cancel": staticmethod(lambda *args, **kwargs: None)},
                )()
            },
        )()

    def _stop_animation(self) -> None:
        return

    def _finalize_content_preview(self) -> None:
        return

    def _compute_ask_user_max_visible(
        self,
        total_options: int,
        term_rows: int,
        allow_custom_input: bool,
        max_visible_cap: int = 12,
    ) -> int:
        _ = term_rows, allow_custom_input, max_visible_cap
        return max(1, min(total_options, 3))

    def _read_terminal_size(self) -> tuple[int, int]:
        return (24, 80)

    def _is_ui_resize_enabled(self) -> bool:
        return False

    def _remember_approved_command(self, command: str) -> None:
        self._remembered_commands.append(command)


def test_handle_interaction_required_sets_interaction_response():
    shell = _DummyShell()
    request = AskUserRequestBuilder.from_tool_args(
        kind="choice_or_text",
        prompt="Pick one",
        options=[
            {"value": "opt1", "label": "Option 1"},
            {"value": "opt2", "label": "Option 2"},
        ],
        default="opt1",
        custom={"label": "Other", "placeholder": "This is intentionally very long to avoid squeezing input space"},
    )
    event = LLMEvent(
        event_type=LLMEventType.INTERACTION_REQUIRED,
        data={"interaction_request": request.to_dict()},
        timestamp=time.time(),
    )

    class _DummyApp:
        def __init__(self, *args, **kwargs) -> None:
            class _Input:
                @staticmethod
                def flush() -> None:
                    return

                @staticmethod
                def flush_keys() -> None:
                    return

            self.input = _Input()

        def run(self, in_thread: bool = True) -> str:
            _ = in_thread
            return "opt2"

    with patch("prompt_toolkit.Application", _DummyApp):
        result = handle_interaction_required(shell, event)

    assert result == LLMCallbackResult.CONTINUE
    response_payload = event.data.get("interaction_response")
    assert isinstance(response_payload, dict)
    assert response_payload.get("interaction_id") == request.id
    assert response_payload.get("answer", {}).get("value") == "opt2"


def test_handle_interaction_required_reads_interaction_request_payload():
    shell = _DummyShell()
    request = AskUserRequestBuilder.from_tool_args(
        kind="choice_or_text",
        prompt="Pick one",
        options=[
            {"value": "opt1", "label": "Option 1"},
            {"value": "opt2", "label": "Option 2", "description": "Second option"},
        ],
        default="opt1",
    )
    event = LLMEvent(
        event_type=LLMEventType.INTERACTION_REQUIRED,
        data={"interaction_request": request.to_dict()},
        timestamp=time.time(),
    )

    class _DummyApp:
        def __init__(self, *args, **kwargs) -> None:
            class _Input:
                @staticmethod
                def flush() -> None:
                    return

                @staticmethod
                def flush_keys() -> None:
                    return

            self.input = _Input()

        def run(self, in_thread: bool = True) -> str:
            _ = in_thread
            return "opt2"

    with patch("prompt_toolkit.Application", _DummyApp):
        result = handle_interaction_required(shell, event)

    assert result == LLMCallbackResult.CONTINUE
    response_payload = event.data.get("interaction_response")
    assert isinstance(response_payload, dict)
    assert response_payload.get("interaction_id") == request.id
    assert response_payload.get("status") == "submitted"


def test_handle_interaction_required_supports_text_input():
    shell = _DummyShell()
    request = AskUserRequestBuilder.from_tool_args(
        kind="text_input",
        prompt="Type a fruit",
        placeholder="Enter fruit name",
    )
    event = LLMEvent(
        event_type=LLMEventType.INTERACTION_REQUIRED,
        data={"interaction_request": request.to_dict()},
        timestamp=time.time(),
    )

    class _DummyApp:
        def __init__(self, *args, **kwargs) -> None:
            class _Input:
                @staticmethod
                def flush() -> None:
                    return

                @staticmethod
                def flush_keys() -> None:
                    return

            self.input = _Input()

        def run(self, in_thread: bool = True) -> str:
            _ = in_thread
            return "dragonfruit"

    with patch("prompt_toolkit.Application", _DummyApp):
        result = handle_interaction_required(shell, event)

    assert result == LLMCallbackResult.CONTINUE
    response_payload = event.data.get("interaction_response")
    assert isinstance(response_payload, dict)
    assert response_payload.get("interaction_id") == request.id
    assert response_payload.get("answer", {}).get("type") == "text"
    assert response_payload.get("answer", {}).get("value") == "dragonfruit"


def test_handle_interaction_required_prefills_text_input_default():
    shell = _DummyShell()
    request = AskUserRequestBuilder.from_tool_args(
        kind="text_input",
        prompt="Type a fruit",
        placeholder="Enter fruit name",
        default="kiwi",
    )
    event = LLMEvent(
        event_type=LLMEventType.INTERACTION_REQUIRED,
        data={"interaction_request": request.to_dict()},
        timestamp=time.time(),
    )

    class _CapturingBuffer:
        instances: list["_CapturingBuffer"] = []

        def __init__(self, *args, **kwargs) -> None:
            _ = args, kwargs
            self.text = ""
            self.__class__.instances.append(self)

    class _DummyApp:
        def __init__(self, *args, **kwargs) -> None:
            class _Input:
                @staticmethod
                def flush() -> None:
                    return

                @staticmethod
                def flush_keys() -> None:
                    return

            self.input = _Input()

        def run(self, in_thread: bool = True) -> str:
            _ = in_thread
            return _CapturingBuffer.instances[-1].text

    with patch("prompt_toolkit.buffer.Buffer", _CapturingBuffer), patch(
        "prompt_toolkit.Application", _DummyApp
    ):
        result = handle_interaction_required(shell, event)

    assert result == LLMCallbackResult.CONTINUE
    response_payload = event.data.get("interaction_response")
    assert isinstance(response_payload, dict)
    assert response_payload.get("interaction_id") == request.id
    assert response_payload.get("answer", {}).get("value") == "kiwi"


@pytest.mark.timeout(5)
def test_get_user_confirmation_flushes_pending_input(monkeypatch):
    shell = _DummyShell()
    flushed: list[tuple[int, int]] = []

    monkeypatch.setattr("sys.stdin.fileno", lambda: 9)
    monkeypatch.setattr("sys.stdin.read", lambda _count: "y")
    monkeypatch.setattr("sys.stdout.flush", lambda: None)
    monkeypatch.setattr(
        "termios.tcgetattr",
        lambda _fd: [0, 0, 0, 0, 0, 0],
    )
    monkeypatch.setattr(
        "termios.tcsetattr",
        lambda _fd, _when, _settings: None,
    )
    monkeypatch.setattr(
        "termios.tcflush",
        lambda fd, queue: flushed.append((fd, queue)),
    )
    monkeypatch.setattr("tty.setraw", lambda _fd: None)

    result = get_user_confirmation(shell, remember_command="echo hi", allow_remember=True)

    assert result == LLMCallbackResult.APPROVE
    assert flushed == [(9, termios.TCIFLUSH)]


def test_render_interaction_modal_supports_digit_shortcuts():
    shell = _DummyShell()
    request = AskUserRequestBuilder.from_tool_args(
        kind="choice_or_text",
        prompt="Pick one",
        options=[
            {"value": "opt1", "label": "Option 1"},
            {"value": "opt2", "label": "Option 2"},
            {"value": "opt3", "label": "Option 3"},
        ],
        default="opt1",
    )

    class _FakeKeyBindings:
        def __init__(self) -> None:
            self.handlers: dict[str, tuple[object, object]] = {}

        def add(self, *keys, filter=None, eager=False):
            _ = eager

            def decorator(func):
                self.handlers[str(keys[0])] = (func, filter)
                return func

            return decorator

    class _DummyApp:
        def __init__(self, *args, **kwargs) -> None:
            self.layout = kwargs["layout"]
            self.key_bindings = kwargs["key_bindings"]
            self._result = None

            class _Input:
                @staticmethod
                def flush() -> None:
                    return

                @staticmethod
                def flush_keys() -> None:
                    return

            self.input = _Input()

        def exit(self, result=None) -> None:
            self._result = result

        def invalidate(self) -> None:
            return

        def run(self, in_thread: bool = True) -> str | None:
            _ = in_thread
            handler, filter_obj = self.key_bindings.handlers["2"]
            if filter_obj is not None:
                assert filter_obj()
            event = type("_Event", (), {"app": self})()
            handler(event)

            enter_handler, _enter_filter = self.key_bindings.handlers["enter"]
            enter_handler(event)
            return self._result

    with patch("prompt_toolkit.key_binding.KeyBindings", _FakeKeyBindings), patch(
        "prompt_toolkit.Application", _DummyApp
    ):
        response = render_interaction_modal(shell, request)

    assert response.status.value == "submitted"
    assert response.answer is not None
    assert response.answer.value == "opt2"


def test_render_interaction_modal_ctrl_c_cancels():
    shell = _DummyShell()
    request = AskUserRequestBuilder.from_tool_args(
        kind="choice_or_text",
        prompt="Pick one",
        options=[
            {"value": "opt1", "label": "Option 1"},
            {"value": "opt2", "label": "Option 2"},
        ],
        default="opt1",
    )

    class _FakeKeyBindings:
        def __init__(self) -> None:
            self.handlers: dict[str, tuple[object, object]] = {}

        def add(self, *keys, filter=None, eager=False):
            _ = filter, eager

            def decorator(func):
                self.handlers[str(keys[0])] = (func, filter)
                return func

            return decorator

    class _DummyApp:
        def __init__(self, *args, **kwargs) -> None:
            self.layout = kwargs["layout"]
            self.key_bindings = kwargs["key_bindings"]
            self._result = None

            class _Input:
                @staticmethod
                def flush() -> None:
                    return

                @staticmethod
                def flush_keys() -> None:
                    return

            self.input = _Input()

        def exit(self, result=None) -> None:
            self._result = result

        def invalidate(self) -> None:
            return

        def run(self, in_thread: bool = True) -> str | None:
            _ = in_thread
            handler, _filter_obj = self.key_bindings.handlers["c-c"]
            event = type("_Event", (), {"app": self})()
            handler(event)
            return self._result

    with patch("prompt_toolkit.key_binding.KeyBindings", _FakeKeyBindings), patch(
        "prompt_toolkit.Application", _DummyApp
    ):
        response = render_interaction_modal(shell, request)

    assert response.status.value == "cancelled"
    assert response.answer is None


@pytest.mark.timeout(5)
def test_render_interaction_modal_escape_cancels_and_writes_newline():
    shell = _DummyShell()
    request = PlanApprovalRequestBuilder.from_payload(
        prompt="Review this plan.",
        artifact_preview="# Plan\n\nStep 1",
    )

    class _FakeKeyBindings:
        def __init__(self) -> None:
            self.handlers: dict[str, tuple[object, object]] = {}

        def add(self, *keys, filter=None, eager=False):
            _ = filter, eager

            def decorator(func):
                self.handlers[str(keys[0])] = (func, filter)
                return func

            return decorator

    class _DummyApp:
        def __init__(self, *args, **kwargs) -> None:
            self.layout = kwargs["layout"]
            self.key_bindings = kwargs["key_bindings"]
            self._result = None

            class _Input:
                @staticmethod
                def flush() -> None:
                    return

                @staticmethod
                def flush_keys() -> None:
                    return

            self.input = _Input()

        def exit(self, result=None) -> None:
            self._result = result

        def invalidate(self) -> None:
            return

        def run(self, in_thread: bool = True) -> str | None:
            _ = in_thread
            handler, _filter_obj = self.key_bindings.handlers["escape"]
            event = type("_Event", (), {"app": self})()
            handler(event)
            return self._result

    stdout = io.StringIO()
    with patch("sys.stdout", stdout), patch(
        "prompt_toolkit.key_binding.KeyBindings", _FakeKeyBindings
    ), patch("prompt_toolkit.Application", _DummyApp):
        response = render_interaction_modal(shell, request)

    assert response.status.value == "cancelled"
    assert response.answer is None
    assert stdout.getvalue() == "\n"


def test_render_interaction_modal_typing_switches_to_custom_input():
    shell = _DummyShell()
    request = AskUserRequestBuilder.from_tool_args(
        kind="choice_or_text",
        prompt="Pick one or type",
        options=[
            {"value": "opt1", "label": "Option 1"},
            {"value": "opt2", "label": "Option 2"},
        ],
        default="opt1",
        custom={"label": "Other", "placeholder": "Type here"},
    )

    class _FakeBuffer:
        def __init__(self, *args, **kwargs) -> None:
            _ = args, kwargs
            self.text = ""

        def insert_text(self, value: str) -> None:
            self.text += value

    class _FakeKeyBindings:
        def __init__(self) -> None:
            self.handlers: dict[str, tuple[object, object]] = {}

        def add(self, *keys, filter=None, eager=False):
            _ = eager

            def decorator(func):
                self.handlers[str(keys[0])] = (func, filter)
                return func

            return decorator

    class _DummyApp:
        def __init__(self, *args, **kwargs) -> None:
            self.layout = kwargs["layout"]
            self.key_bindings = kwargs["key_bindings"]
            self._result = None

            class _Input:
                @staticmethod
                def flush() -> None:
                    return

                @staticmethod
                def flush_keys() -> None:
                    return

            self.input = _Input()

        def exit(self, result=None) -> None:
            self._result = result

        def invalidate(self) -> None:
            return

        def run(self, in_thread: bool = True) -> str | None:
            _ = in_thread
            any_handler, any_filter = self.key_bindings.handlers["<any>"]
            if any_filter is not None:
                assert any_filter()
            any_event = type("_Event", (), {"app": self, "data": "m"})()
            any_handler(any_event)

            enter_handler, _enter_filter = self.key_bindings.handlers["enter"]
            enter_event = type("_Event", (), {"app": self})()
            enter_handler(enter_event)
            return self._result

    with patch("prompt_toolkit.buffer.Buffer", _FakeBuffer), patch(
        "prompt_toolkit.key_binding.KeyBindings", _FakeKeyBindings
    ), patch("prompt_toolkit.Application", _DummyApp):
        response = render_interaction_modal(shell, request)

    assert response.status.value == "submitted"
    assert response.answer is not None
    assert response.answer.type.value == "text"
    assert response.answer.value == "m"


@pytest.mark.timeout(5)
def test_render_interaction_modal_supports_plan_approval_layout():
    shell = _DummyShell()
    request = PlanApprovalRequestBuilder.from_payload(
        prompt="Review this implementation plan.",
        summary="Add shell status bar and top-level mode toggle.",
        artifact_path="/tmp/plan.md",
        artifact_preview="# Plan\n\n1. Add toolbar\n2. Bind Shift+Tab",
    )

    class _DummyApp:
        def __init__(self, *args, **kwargs) -> None:
            self.layout = kwargs["layout"]
            self.key_bindings = kwargs["key_bindings"]
            self._result = None

            class _Input:
                @staticmethod
                def flush() -> None:
                    return

                @staticmethod
                def flush_keys() -> None:
                    return

            self.input = _Input()

        def exit(self, result=None) -> None:
            self._result = result

        def invalidate(self) -> None:
            return

        def run(self, in_thread: bool = True) -> str | None:
            _ = in_thread
            return "approve"

    with patch("prompt_toolkit.Application", _DummyApp):
        response = render_interaction_modal(shell, request)

    assert response.status.value == "submitted"
    assert response.answer is not None
    assert response.answer.value == "approve"


def test_display_security_panel_shows_fallback_rule_details(monkeypatch):
    monkeypatch.setenv("LANG", "zh_CN.UTF-8")
    _reset_i18n_cache()

    shell = _DummyShell()

    display_security_panel(
        shell,
        {
            "tool_name": "bash_exec",
            "command": "sudo rm /etc/aish/123",
            "security_analysis": {
                "risk_level": "HIGH",
                "sandbox": {"enabled": False, "reason": "sandbox_disabled_by_policy"},
                "fallback_rule_matched": True,
                "matched_rule": {"id": "H-001", "name": "系统配置目录保护"},
                "matched_paths": ["/etc/aish/123"],
                "reasons": ["系统配置目录，误修改会导致严重故障"],
                "impact_description": "系统配置目录，误修改会导致严重故障",
                "suggested_alternatives": ["如确需修改 /etc 下文件，建议由人工完成变更。"],
            },
        },
        panel_mode="blocked",
    )

    output = shell.console.file.getvalue()
    assert "风险等级" in output
    assert "原因" in output
    assert "系统配置目录，误修改会导致严重故障" in output


def test_display_security_panel_for_fallback_rule_confirm_hides_generic_fallback_hint(
    monkeypatch,
):
    monkeypatch.setenv("LANG", "zh_CN.UTF-8")
    _reset_i18n_cache()

    shell = _DummyShell()

    display_security_panel(
        shell,
        {
            "tool_name": "bash_exec",
            "command": "rm -rf /home/lixin/123",
            "security_analysis": {
                "risk_level": "MEDIUM",
                "sandbox": {"enabled": False, "reason": "sandbox_disabled_by_policy"},
                "fallback_rule_matched": True,
                "reasons": ["用户业务数据变更需人工确认"],
            },
        },
        panel_mode="confirm",
    )

    output = shell.console.file.getvalue()
    assert "用户业务数据变更需人工确认" in output
    assert "未能完成命令风险评估" not in output


def test_handle_tool_confirmation_required_uses_panel_payload():
    shell = _DummyShell()
    captured: list[tuple[object, object]] = []
    shell._get_user_confirmation = lambda remember_command=None, allow_remember=False: (
        captured.append((remember_command, allow_remember)) or LLMCallbackResult.APPROVE
    )
    shell._display_security_panel = lambda data, panel_mode="confirm": captured.append(
        ("panel", panel_mode, data.get("panel", {}))
    )

    event = LLMEvent(
        event_type=LLMEventType.TOOL_CONFIRMATION_REQUIRED,
        data={
            "tool_name": "bash_exec",
            "panel": {
                "mode": "confirm",
                "target": "echo hi",
                "allow_remember": True,
                "remember_key": "echo hi",
            },
        },
        timestamp=time.time(),
    )

    result = handle_tool_confirmation_required(shell, event)

    assert result == LLMCallbackResult.APPROVE
    assert ("echo hi", True) in captured
