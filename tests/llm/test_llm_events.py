from unittest.mock import AsyncMock, patch

import pytest

from aish.config import ConfigModel
from aish.llm import LLMCallbackResult, LLMEventType, LLMSession
from aish.state import ContextManager
from aish.skills import SkillManager


@pytest.mark.anyio
async def test_completion_non_stream_emits_op_and_generation_events():
    config = ConfigModel(model="test-model", api_key="test-key")
    session = LLMSession(config=config, skill_manager=SkillManager())

    events = []

    def event_callback(event):
        events.append(event)
        return LLMCallbackResult.CONTINUE

    session.event_callback = event_callback

    async def fake_acompletion(**kwargs):
        return {
            "choices": [
                {
                    "message": {"role": "assistant", "content": "hello"},
                    "finish_reason": "stop",
                }
            ]
        }

    with patch.object(session, "_get_acompletion", return_value=fake_acompletion):
        result = await session.completion(
            prompt="hi", system_message="sys", stream=False
        )

    assert result == "hello"

    event_types = [event.event_type for event in events]
    assert event_types == [
        LLMEventType.OP_START,
        LLMEventType.GENERATION_START,
        LLMEventType.CONTENT_DELTA,
        LLMEventType.GENERATION_END,
        LLMEventType.OP_END,
    ]

    turn_id = events[0].data.get("turn_id")
    assert turn_id
    assert all(event.data.get("turn_id") == turn_id for event in events)
    assert events[-1].data.get("result") == result


@pytest.mark.anyio
async def test_completion_emit_events_false_suppresses_all_events():
    config = ConfigModel(model="test-model", api_key="test-key")
    session = LLMSession(config=config, skill_manager=SkillManager())

    events = []

    def event_callback(event):
        events.append(event)
        return LLMCallbackResult.CONTINUE

    session.event_callback = event_callback

    async def fake_acompletion(**kwargs):
        return {
            "choices": [
                {
                    "message": {"role": "assistant", "content": "hello"},
                    "finish_reason": "stop",
                }
            ]
        }

    with patch.object(session, "_get_acompletion", return_value=fake_acompletion):
        result = await session.completion(
            prompt="hi", system_message="sys", stream=False, emit_events=False
        )

    assert result == "hello"
    assert events == []


@pytest.mark.anyio
async def test_process_input_single_generation_emits_op_generation_and_content_events():
    config = ConfigModel(model="test-model", api_key="test-key")
    session = LLMSession(config=config, skill_manager=SkillManager())

    events = []

    def event_callback(event):
        events.append(event)
        return LLMCallbackResult.CONTINUE

    session.event_callback = event_callback

    async def fake_acompletion(**kwargs):
        return {
            "choices": [
                {
                    "message": {"role": "assistant", "content": "hello"},
                    "finish_reason": "stop",
                }
            ]
        }

    context_manager = ContextManager()

    with (
        patch.object(session, "_get_acompletion", return_value=fake_acompletion),
        patch.object(session, "_trim_messages", side_effect=lambda msgs: msgs),
        patch.object(session, "_get_tools_spec", return_value=[]),
    ):
        result = await session.process_input(
            prompt="hi",
            context_manager=context_manager,
            system_message="sys",
        )

    assert result == "hello"

    event_types = [event.event_type for event in events]
    assert event_types == [
        LLMEventType.OP_START,
        LLMEventType.GENERATION_START,
        LLMEventType.CONTENT_DELTA,
        LLMEventType.GENERATION_END,
        LLMEventType.OP_END,
    ]

    turn_id = events[0].data.get("turn_id")
    assert turn_id
    assert all(event.data.get("turn_id") == turn_id for event in events)
    assert events[-1].data.get("result") == result


@pytest.mark.anyio
async def test_process_input_tool_call_content_is_marked_non_final():
    config = ConfigModel(model="test-model", api_key="test-key")
    session = LLMSession(config=config, skill_manager=SkillManager())

    events = []

    def event_callback(event):
        events.append(event)
        return LLMCallbackResult.CONTINUE

    session.event_callback = event_callback

    async def fake_acompletion(**kwargs):
        return {
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": "I will run a tool.",
                        "tool_calls": [
                            {
                                "id": "call_1",
                                "type": "function",
                                "function": {"name": "bash_exec", "arguments": "{}"},
                            }
                        ],
                    },
                    "finish_reason": "tool_calls",
                }
            ]
        }

    context_manager = ContextManager()

    with (
        patch.object(session, "_get_acompletion", return_value=fake_acompletion),
        patch.object(session, "_trim_messages", side_effect=lambda msgs: msgs),
        patch.object(session, "_get_tools_spec", return_value=[]),
        patch.object(
            session, "_handle_tool_calls", new_callable=AsyncMock
        ) as mock_tool,
    ):
        mock_tool.return_value = (True, "", [])
        result = await session.process_input(
            prompt="hi",
            context_manager=context_manager,
            system_message="sys",
        )

    assert result == ""

    content_events = [e for e in events if e.event_type == LLMEventType.CONTENT_DELTA]
    assert len(content_events) == 1
    assert content_events[0].data.get("is_final") is False


@pytest.mark.anyio
async def test_process_input_litellm_error_uses_raw_message():
    config = ConfigModel(model="test-model", api_key="test-key")
    session = LLMSession(config=config, skill_manager=SkillManager())

    events = []

    def event_callback(event):
        events.append(event)
        return LLMCallbackResult.CONTINUE

    session.event_callback = event_callback

    # Create a litellm-like exception without importing litellm.
    class AuthenticationError(Exception):
        __module__ = "litellm.exceptions"

    async def fake_acompletion(**kwargs):
        raise AuthenticationError("invalid api key: sk-THIS_SHOULD_NOT_LEAK")

    context_manager = ContextManager()

    with (
        patch.object(session, "_get_acompletion", return_value=fake_acompletion),
        patch.object(session, "_trim_messages", side_effect=lambda msgs: msgs),
        patch.object(session, "_get_tools_spec", return_value=[]),
    ):
        result = await session.process_input(
            prompt="hi",
            context_manager=context_manager,
            system_message="sys",
        )

    assert result == ""

    # Expect an ERROR event with litellm_error and the native provider message.
    error_events = [e for e in events if e.event_type == LLMEventType.ERROR]
    assert len(error_events) == 1
    err = error_events[0]
    assert err.data.get("error_type") == "litellm_error"
    assert err.data.get("error_message") == "invalid api key: sk-THIS_SHOULD_NOT_LEAK"
    # Ensure secrets are redacted in debug details.
    details = err.data.get("error_details")
    if details is not None:
        text = str(details)
        assert "THIS_SHOULD_NOT_LEAK" not in text
        assert "sk-THIS_SHOULD_NOT_LEAK" not in text
