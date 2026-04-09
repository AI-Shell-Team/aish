import sys
from unittest.mock import AsyncMock, patch

import pytest

from aish.config import ConfigModel
from aish.terminal.interaction import (InteractionAnswer, InteractionAnswerType,
                                       InteractionKind, InteractionRequest,
                                       InteractionResponse, InteractionStatus)
from aish.llm import (LLMCallbackResult, LLMSession, ToolDispatchOutcome,
                      ToolDispatchStatus)
from aish.state import ContextManager
from aish.skills import SkillManager
from aish.tools.ask_user import AskUserTool
from aish.tools.base import (ToolBase, ToolExecutionContext, ToolPanelSpec,
                             ToolPreflightAction, ToolPreflightResult)
from aish.tools.result import ToolResult


def test_ask_user_tool_selected(monkeypatch):
    monkeypatch.setattr(sys.stdin, "isatty", lambda: True, raising=False)
    monkeypatch.setattr(sys.stdout, "isatty", lambda: True, raising=False)

    def request_interaction(request: InteractionRequest) -> InteractionResponse:
        assert request.prompt == "pick one"
        assert request.default == "a"
        return InteractionResponse(
            interaction_id=request.id,
            status=InteractionStatus.SUBMITTED,
            answer=InteractionAnswer(
                type=InteractionAnswerType.OPTION,
                value="b",
                label="B",
            ),
        )

    tool = AskUserTool(request_interaction=request_interaction)
    result = tool(
        kind="choice_or_text",
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

    def request_interaction(request: InteractionRequest) -> InteractionResponse:
        return InteractionResponse(
            interaction_id=request.id,
            status=InteractionStatus.CANCELLED,
            reason="cancelled",
        )

    tool = AskUserTool(request_interaction=request_interaction)
    result = tool(
        kind="choice_or_text",
        prompt="pick one",
        options=[
            {"value": "a", "label": "A", "description": "Alpha option"},
            {"value": "b", "label": "B"},
        ],
        default="a",
    )

    assert result.ok is False
    assert result.meta.get("kind") == "user_input_required"
    assert result.meta.get("reason") == "cancelled"
    assert "continue with default" in result.output
    assert "Alpha option" in result.output


def test_ask_user_tool_preserves_option_descriptions(monkeypatch):
    monkeypatch.setattr(sys.stdin, "isatty", lambda: True, raising=False)
    monkeypatch.setattr(sys.stdout, "isatty", lambda: True, raising=False)

    captured: dict[str, object] = {}

    def request_interaction(request: InteractionRequest) -> InteractionResponse:
        captured.update(request.to_dict())
        return InteractionResponse(
            interaction_id=request.id,
            status=InteractionStatus.SUBMITTED,
            answer=InteractionAnswer(
                type=InteractionAnswerType.OPTION,
                value="a",
                label="A",
            ),
        )

    tool = AskUserTool(request_interaction=request_interaction)
    result = tool(
        kind="choice_or_text",
        prompt="pick one",
        options=[
            {"value": "a", "label": "A", "description": "Alpha option"},
            {"value": "b", "label": "B"},
        ],
        default="a",
    )

    assert result.ok is True
    assert captured["options"] == [
        {"value": "a", "label": "A", "description": "Alpha option"},
        {"value": "b", "label": "B"},
    ]


def test_ask_user_tool_respects_interaction_response_status():
    tool = AskUserTool(
        request_interaction=lambda request: InteractionResponse(
            interaction_id=request.id,
            status=InteractionStatus.UNAVAILABLE,
            reason="unavailable",
        )
    )
    result = tool(
        kind="choice_or_text",
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


def test_ask_user_tool_custom_input_allowed(monkeypatch):
    monkeypatch.setattr(sys.stdin, "isatty", lambda: True, raising=False)
    monkeypatch.setattr(sys.stdout, "isatty", lambda: True, raising=False)

    def request_interaction(request: InteractionRequest) -> InteractionResponse:
        return InteractionResponse(
            interaction_id=request.id,
            status=InteractionStatus.SUBMITTED,
            answer=InteractionAnswer(
                type=InteractionAnswerType.TEXT,
                value="mango",
            ),
        )

    tool = AskUserTool(request_interaction=request_interaction)
    result = tool(
        kind="choice_or_text",
        prompt="pick one",
        options=[
            {"value": "a", "label": "A"},
            {"value": "b", "label": "B"},
        ],
        custom={"label": "Other"},
    )

    assert result.ok is True
    payload = result.data
    assert result.output == "User input: mango"
    assert payload["value"] == "mango"
    assert payload["status"] == "custom"
    assert payload["answer_type"] == "text"


def test_ask_user_tool_choice_or_text_accepts_custom(monkeypatch):
    monkeypatch.setattr(sys.stdin, "isatty", lambda: True, raising=False)
    monkeypatch.setattr(sys.stdout, "isatty", lambda: True, raising=False)

    seen: dict[str, object] = {}

    def request_interaction(request: InteractionRequest) -> InteractionResponse:
        seen["kind"] = request.kind
        seen["has_custom"] = request.custom is not None
        return InteractionResponse(
            interaction_id=request.id,
            status=InteractionStatus.SUBMITTED,
            answer=InteractionAnswer(
                type=InteractionAnswerType.OPTION,
                value="a",
                label="A",
            ),
        )

    tool = AskUserTool(request_interaction=request_interaction)
    result = tool(
        kind="choice_or_text",
        prompt="pick one",
        options=[
            {"value": "a", "label": "A"},
            {"value": "b", "label": "B"},
        ],
    )

    assert result.ok is True
    assert seen["kind"] == InteractionKind.CHOICE_OR_TEXT
    assert seen["has_custom"] is True


def test_ask_user_tool_text_input_mode(monkeypatch):
    monkeypatch.setattr(sys.stdin, "isatty", lambda: True, raising=False)
    monkeypatch.setattr(sys.stdout, "isatty", lambda: True, raising=False)

    def request_interaction(request: InteractionRequest) -> InteractionResponse:
        assert request.kind == InteractionKind.TEXT_INPUT
        assert request.options == []
        assert request.placeholder == "Enter fruit name"
        return InteractionResponse(
            interaction_id=request.id,
            status=InteractionStatus.SUBMITTED,
            answer=InteractionAnswer(
                type=InteractionAnswerType.TEXT,
                value="dragonfruit",
            ),
        )

    tool = AskUserTool(request_interaction=request_interaction)
    result = tool(
        kind="text_input",
        prompt="Type a fruit",
        placeholder="Enter fruit name",
    )

    assert result.ok is True
    payload = result.data
    assert result.output == "User input: dragonfruit"
    assert payload["value"] == "dragonfruit"
    assert payload["status"] == "custom"
    assert payload["answer_type"] == "text"


def test_request_interaction_prefers_standard_response(monkeypatch):
    monkeypatch.setattr(sys.stdin, "isatty", lambda: True, raising=False)
    monkeypatch.setattr(sys.stdout, "isatty", lambda: True, raising=False)

    config = ConfigModel(model="test-model", api_key="test-key")

    def event_callback(event):
        event.data["interaction_response"] = {
            "interaction_id": "interaction_1",
            "status": "submitted",
            "answer": {
                "type": "option",
                "value": "b",
                "label": "B",
            },
        }
        return LLMCallbackResult.CONTINUE

    session = LLMSession(
        config=config,
        skill_manager=SkillManager(),
        event_callback=event_callback,
    )

    response = session.request_interaction(
        InteractionRequest.from_dict(
            {
                "id": "interaction_1",
                "kind": "choice_or_text",
                "prompt": "pick one",
                "required": True,
                "allow_cancel": True,
                "source": {"type": "tool", "name": "ask_user"},
                "metadata": {},
                "options": [
                    {"value": "a", "label": "A"},
                    {"value": "b", "label": "B"},
                ],
            }
        )
    )

    assert response.status.value == "submitted"
    assert response.answer is not None
    assert response.answer.type == InteractionAnswerType.OPTION
    assert response.answer.value == "b"


def test_request_interaction_returns_cancelled_without_response(monkeypatch):
    monkeypatch.setattr(sys.stdin, "isatty", lambda: True, raising=False)
    monkeypatch.setattr(sys.stdout, "isatty", lambda: True, raising=False)

    config = ConfigModel(model="test-model", api_key="test-key")

    def event_callback(event):
        return LLMCallbackResult.CONTINUE

    session = LLMSession(
        config=config,
        skill_manager=SkillManager(),
        event_callback=event_callback,
    )

    response = session.request_interaction(
        InteractionRequest.from_dict(
            {
                "id": "interaction_2",
                "kind": "choice_or_text",
                "prompt": "pick one",
                "required": True,
                "allow_cancel": True,
                "source": {"type": "tool", "name": "ask_user"},
                "metadata": {},
                "options": [{"value": "a", "label": "A"}],
            }
        )
    )

    assert response.status == InteractionStatus.CANCELLED


@pytest.mark.anyio
async def test_handle_tool_calls_ask_user_user_input_required_breaks(monkeypatch):
    config = ConfigModel(model="test-model", api_key="test-key")
    session = LLMSession(config=config, skill_manager=SkillManager())
    context_manager = ContextManager()

    tool_calls = [
        {
            "id": "call_1",
            "type": "function",
            "function": {
                "name": "ask_user",
                "arguments": '{"kind":"choice_or_text","prompt":"pick one","options":[{"value":"a","label":"A"}]}'
            },
        },
        {
            "id": "call_2",
            "type": "function",
            "function": {"name": "bash_exec", "arguments": '{"code":"echo hi"}'},
        },
    ]

    async def fake_pre_execute_tool(tool_name, _tool_args):
        if tool_name == "ask_user":
            return ToolDispatchOutcome(
                status=ToolDispatchStatus.EXECUTED,
                result=ToolResult(
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
        return ToolDispatchOutcome(
            status=ToolDispatchStatus.EXECUTED,
            result=ToolResult(ok=True, output="diagnostic result"),
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
        return ToolDispatchOutcome(
            status=ToolDispatchStatus.SHORT_CIRCUIT,
            result=ToolResult(
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

    class _DummyBashTool(ToolBase):
        def __init__(self):
            super().__init__(
                name="bash_exec",
                description="dummy bash",
                parameters={"type": "object", "properties": {"code": {"type": "string"}}},
            )
            self.called = False

        def prepare_invocation(
            self, tool_args: dict[str, object], context: ToolExecutionContext
        ) -> ToolPreflightResult:
            _ = context
            return ToolPreflightResult(
                action=ToolPreflightAction.SHORT_CIRCUIT,
                panel=ToolPanelSpec(
                    mode="blocked",
                    target=str(tool_args.get("code")),
                    analysis={
                        "risk_level": "HIGH",
                        "sandbox": {
                            "enabled": False,
                            "reason": "sandbox_disabled_by_policy",
                        },
                        "fallback_rule_matched": True,
                        "reasons": ["系统配置目录，误修改会导致严重故障"],
                    },
                ),
                result=ToolResult(
                    ok=False,
                    output="blocked",
                    meta={"kind": "security_blocked"},
                    stop_tool_chain=True,
                ),
            )

        async def __call__(self, code: str):
            self.called = True
            return ToolResult(ok=False, output=code)

    session.tools["bash_exec"] = _DummyBashTool()
    emitted: list[tuple[object, dict]] = []
    monkeypatch.setattr(session, "emit_event", lambda event_type, data=None: emitted.append((event_type, data or {})))

    outcome = await session.pre_execute_tool("bash_exec", {"code": "sudo rm /etc/aish/123"})

    assert outcome.status == ToolDispatchStatus.SHORT_CIRCUIT
    assert emitted
    assert emitted[0][1].get("panel", {}).get("mode") == "blocked"
    assert outcome.result.meta.get("kind") == "security_blocked"
    assert session.tools["bash_exec"].called is False


@pytest.mark.anyio
async def test_pre_execute_tool_info_panel_executes_tool(monkeypatch):
    config = ConfigModel(model="test-model", api_key="test-key")
    session = LLMSession(config=config, skill_manager=SkillManager())

    class _InfoTool(ToolBase):
        def __init__(self):
            super().__init__(
                name="info_tool",
                description="info tool",
                parameters={"type": "object", "properties": {"value": {"type": "string"}}},
            )

        def prepare_invocation(
            self, tool_args: dict[str, object], context: ToolExecutionContext
        ) -> ToolPreflightResult:
            _ = context
            return ToolPreflightResult(
                action=ToolPreflightAction.EXECUTE,
                panel=ToolPanelSpec(mode="info", target=str(tool_args.get("value"))),
            )

        def __call__(self, value: str):
            return ToolResult(ok=True, output=f"echo:{value}")

    session.tools["info_tool"] = _InfoTool()
    emitted: list[tuple[object, dict]] = []
    monkeypatch.setattr(
        session,
        "emit_event",
        lambda event_type, data=None: emitted.append((event_type, data or {})),
    )

    outcome = await session.pre_execute_tool("info_tool", {"value": "hello"})

    assert outcome.status == ToolDispatchStatus.EXECUTED
    assert outcome.result.output == "echo:hello"
    assert emitted
    assert emitted[0][1].get("panel", {}).get("mode") == "info"


@pytest.mark.anyio
async def test_pre_execute_tool_legacy_hooks_use_get_pre_execute_subject(monkeypatch):
    config = ConfigModel(model="test-model", api_key="test-key")
    session = LLMSession(config=config, skill_manager=SkillManager())

    class _LegacyTool(ToolBase):
        def __init__(self):
            super().__init__(
                name="legacy_tool",
                description="legacy tool",
                parameters={
                    "type": "object",
                    "properties": {
                        "dangerous": {"type": "string"},
                        "file_path": {"type": "string"},
                    },
                },
            )
            self.seen_subject = None

        def get_pre_execute_subject(self, tool_args: dict[str, object]) -> object:
            return tool_args.get("dangerous")

        def need_confirm_before_exec(self, subject: object) -> bool:
            self.seen_subject = subject
            return True

        def get_confirmation_info(self, subject: object) -> dict:
            return {
                "target": "/tmp/demo.txt",
                "preview": f"subject={subject}",
            }

        def __call__(self, dangerous: str, file_path: str):
            return ToolResult(ok=True, output=f"{dangerous}:{file_path}")

    session.tools["legacy_tool"] = _LegacyTool()
    monkeypatch.setattr(session, "request_confirmation", lambda *_args, **_kwargs: LLMCallbackResult.APPROVE)

    outcome = await session.pre_execute_tool(
        "legacy_tool",
        {"dangerous": "rm -rf", "file_path": "/tmp/demo.txt"},
    )

    assert outcome.status == ToolDispatchStatus.EXECUTED
    assert session.tools["legacy_tool"].seen_subject == "rm -rf"
    assert outcome.result.output == "rm -rf:/tmp/demo.txt"


@pytest.mark.anyio
async def test_pre_execute_tool_write_file_confirmation_uses_panel_target_preview(
    monkeypatch,
):
    config = ConfigModel(model="test-model", api_key="test-key")
    session = LLMSession(config=config, skill_manager=SkillManager())
    captured: dict[str, object] = {}

    def _request_confirmation(_event_type, data, **_kwargs):
        captured.update(data)
        return LLMCallbackResult.DENY

    monkeypatch.setattr(session, "request_confirmation", _request_confirmation)

    outcome = await session.pre_execute_tool(
        "write_file",
        {"file_path": "/tmp/demo.txt", "content": "hello world"},
    )

    assert outcome.status == ToolDispatchStatus.REJECTED
    assert captured.get("panel", {}).get("target") == "/tmp/demo.txt"
    assert captured.get("panel", {}).get("preview") == "hello world"


@pytest.mark.anyio
async def test_pre_execute_tool_write_file_confirmation_keeps_full_long_preview(
    monkeypatch,
):
    config = ConfigModel(model="test-model", api_key="test-key")
    session = LLMSession(config=config, skill_manager=SkillManager())
    captured: dict[str, object] = {}
    long_content = "A" * 150 + "TAIL"

    def _request_confirmation(_event_type, data, **_kwargs):
        captured.update(data)
        return LLMCallbackResult.DENY

    monkeypatch.setattr(session, "request_confirmation", _request_confirmation)

    outcome = await session.pre_execute_tool(
        "write_file",
        {"file_path": "/tmp/demo.txt", "content": long_content},
    )

    assert outcome.status == ToolDispatchStatus.REJECTED
    assert captured.get("panel", {}).get("preview") == long_content


@pytest.mark.anyio
async def test_pre_execute_tool_edit_file_confirmation_uses_panel_target_preview(
    monkeypatch,
):
    config = ConfigModel(model="test-model", api_key="test-key")
    session = LLMSession(config=config, skill_manager=SkillManager())
    captured: dict[str, object] = {}

    def _request_confirmation(_event_type, data, **_kwargs):
        captured.update(data)
        return LLMCallbackResult.DENY

    monkeypatch.setattr(session, "request_confirmation", _request_confirmation)

    outcome = await session.pre_execute_tool(
        "edit_file",
        {
            "file_path": "/tmp/demo.txt",
            "old_string": "old",
            "new_string": "new",
            "replace_all": True,
        },
    )

    assert outcome.status == ToolDispatchStatus.REJECTED
    assert captured.get("panel", {}).get("target") == "/tmp/demo.txt"
    assert captured.get("panel", {}).get("preview") == "Replace all: old -> new"
