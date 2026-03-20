import sys
from unittest.mock import AsyncMock, patch

import pytest

from aish.config import ConfigModel
from aish.context_manager import ContextManager
from aish.llm import LLMCallbackResult, LLMSession
from aish.skills import SkillManager
from aish.tools.ask_user import AskUserTool
from aish.tools.result import ToolResult


def test_ask_user_tool_selected(monkeypatch):
    monkeypatch.setattr(sys.stdin, "isatty", lambda: True, raising=False)
    monkeypatch.setattr(sys.stdout, "isatty", lambda: True, raising=False)

    def request_choice(data):
        assert data["prompt"] == "pick one"
        assert data["default"] == "a"
        return "b", "selected"

    tool = AskUserTool(request_choice=request_choice)
    result = tool(
        prompt="pick one",
        options=[
            {"value": "a", "label": "A"},
            {"value": "b", "label": "B"},
        ],
        default="a",
    )

    assert result.ok is True
    payload = result.data
    assert result.output == "User selected: B"
    assert payload["value"] == "b"
    assert payload["label"] == "B"
    assert payload["status"] == "selected"


def test_ask_user_tool_cancelled_pauses(monkeypatch):
    monkeypatch.setattr(sys.stdin, "isatty", lambda: True, raising=False)
    monkeypatch.setattr(sys.stdout, "isatty", lambda: True, raising=False)

    def request_choice(_data):
        return None, "cancelled"

    tool = AskUserTool(request_choice=request_choice)
    result = tool(
        prompt="pick one",
        options=[
            {"value": "a", "label": "A"},
            {"value": "b", "label": "B"},
        ],
        default="a",
    )

    assert result.ok is False
    assert result.meta.get("kind") == "user_input_required"
    assert result.meta.get("reason") == "cancelled"
    assert "continue with default" in result.output


def test_ask_user_tool_unavailable_pauses():
    # Ensure deterministic "unavailable" across environments.
    monkeypatch = pytest.MonkeyPatch()
    monkeypatch.setattr(sys.stdin, "isatty", lambda: False, raising=False)
    monkeypatch.setattr(sys.stdout, "isatty", lambda: False, raising=False)
    tool = AskUserTool(request_choice=lambda _data: ("a", "selected"))
    result = tool(
        prompt="pick one",
        options=[
            {"value": "a", "label": "A"},
            {"value": "b", "label": "B"},
        ],
        default="a",
    )
    assert result.ok is False
    assert result.meta.get("kind") == "user_input_required"
    assert result.meta.get("reason") == "unavailable"
    monkeypatch.undo()


def test_ask_user_tool_custom_input_allowed(monkeypatch):
    monkeypatch.setattr(sys.stdin, "isatty", lambda: True, raising=False)
    monkeypatch.setattr(sys.stdout, "isatty", lambda: True, raising=False)

    def request_choice(_data):
        return "mango", "selected"

    tool = AskUserTool(request_choice=request_choice)
    result = tool(
        prompt="pick one",
        options=[
            {"value": "a", "label": "A"},
            {"value": "b", "label": "B"},
        ],
        allow_custom_input=True,
    )

    assert result.ok is True
    payload = result.data
    assert result.output == "User input: mango"
    assert payload["value"] == "mango"
    assert payload["status"] == "custom"


@pytest.mark.anyio
async def test_handle_tool_calls_ask_user_user_input_required_breaks(monkeypatch):
    config = ConfigModel(model="test-model", api_key="test-key")
    session = LLMSession(config=config, skill_manager=SkillManager())
    context_manager = ContextManager()

    tool_calls = [
        {
            "id": "call_1",
            "type": "function",
            "function": {"name": "ask_user", "arguments": "{}"},
        },
        {
            "id": "call_2",
            "type": "function",
            "function": {"name": "bash_exec", "arguments": '{"code":"echo hi"}'},
        },
    ]

    async def fake_pre_execute_tool(tool_name, _tool_args):
        if tool_name == "ask_user":
            return (
                LLMCallbackResult.APPROVE,
                ToolResult(
                    ok=False,
                    output="paused",
                    meta={"kind": "user_input_required", "reason": "cancelled"},
                    stop_tool_chain=True,
                ),
            )
        raise AssertionError("should not execute tool calls after ask_user pause")

    with patch.object(
        session, "pre_execute_tool", new=AsyncMock(side_effect=fake_pre_execute_tool)
    ):
        tool_call_cancelled, output, _messages = await session._handle_tool_calls(
            tool_calls=tool_calls,
            context_manager=context_manager,
            system_message=None,
            output="",
        )

    assert tool_call_cancelled is True
    assert output == "paused"


@pytest.mark.anyio
async def test_handle_tool_calls_system_diagnose_agent_sets_session_output():
    config = ConfigModel(model="test-model", api_key="test-key")
    session = LLMSession(config=config, skill_manager=SkillManager())
    context_manager = ContextManager()

    tool_calls = [
        {
            "id": "call_1",
            "type": "function",
            "function": {
                "name": "system_diagnose_agent",
                "arguments": '{"query":"check logs"}',
            },
        }
    ]

    async def fake_pre_execute_tool(tool_name, _tool_args):
        assert tool_name == "system_diagnose_agent"
        return (
            LLMCallbackResult.APPROVE,
            ToolResult(ok=True, output="diagnostic result"),
        )

    with patch.object(
        session, "pre_execute_tool", new=AsyncMock(side_effect=fake_pre_execute_tool)
    ):
        tool_call_cancelled, output, _messages = await session._handle_tool_calls(
            tool_calls=tool_calls,
            context_manager=context_manager,
            system_message=None,
            output="",
        )

    assert tool_call_cancelled is False
    assert output == "diagnostic result"


@pytest.mark.anyio
async def test_handle_tool_calls_bash_security_blocked_clears_session_output():
    config = ConfigModel(model="test-model", api_key="test-key")
    session = LLMSession(config=config, skill_manager=SkillManager())
    context_manager = ContextManager()

    tool_calls = [
        {
            "id": "call_1",
            "type": "function",
            "function": {
                "name": "bash_exec",
                "arguments": '{"code":"rm -rf /tmp/x"}',
            },
        }
    ]

    async def fake_pre_execute_tool(tool_name, _tool_args):
        assert tool_name == "bash_exec"
        return (
            LLMCallbackResult.APPROVE,
            ToolResult(
                ok=False,
                output="<stderr>blocked</stderr>",
                meta={"kind": "security_blocked"},
                stop_tool_chain=True,
            ),
        )

    with patch.object(
        session, "pre_execute_tool", new=AsyncMock(side_effect=fake_pre_execute_tool)
    ):
        tool_call_cancelled, output, _messages = await session._handle_tool_calls(
            tool_calls=tool_calls,
            context_manager=context_manager,
            system_message=None,
            output="",
        )

    assert tool_call_cancelled is True
    assert output == ""


@pytest.mark.anyio
async def test_pre_execute_tool_emits_blocked_panel_for_policy_fallback_rule(monkeypatch):
    config = ConfigModel(model="test-model", api_key="test-key")
    session = LLMSession(config=config, skill_manager=SkillManager())

    class _DummyBashTool:
        description = "dummy bash"
        called = False

        def need_confirm_before_exec(self, _arg):
            return False

        def get_confirmation_info(self, arg):
            return {
                "command": arg,
                "security_decision": {"allow": False, "require_confirmation": False},
                "security_analysis": {
                    "risk_level": "HIGH",
                    "sandbox": {"enabled": False, "reason": "sandbox_disabled_by_policy"},
                    "fallback_rule_matched": True,
                    "reasons": ["系统配置目录，误修改会导致严重故障"],
                },
            }

        async def __call__(self, code: str):
            self.called = True
            return ToolResult(ok=False, output=code)

    session.tools["bash_exec"] = _DummyBashTool()
    emitted: list[tuple[object, dict]] = []
    monkeypatch.setattr(session, "emit_event", lambda event_type, data=None: emitted.append((event_type, data or {})))

    goon, _result = await session.pre_execute_tool("bash_exec", {"code": "sudo rm /etc/aish/123"})

    assert goon == LLMCallbackResult.CONTINUE
    assert emitted
    assert emitted[0][1].get("panel_mode") == "blocked"
    assert _result.meta.get("kind") == "security_blocked"
    assert session.tools["bash_exec"].called is False
