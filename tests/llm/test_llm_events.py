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
async def test_process_input_streaming_final_answer_after_tool_calls_emits_final_content():
    config = ConfigModel(model="test-model", api_key="test-key")
    session = LLMSession(config=config, skill_manager=SkillManager())

    events = []

    def event_callback(event):
        events.append(event)
        return LLMCallbackResult.CONTINUE

    session.event_callback = event_callback

    async def first_stream():
        yield {
            "choices": [
                {
                    "delta": {
                        "content": "I will inspect the workspace.",
                        "tool_calls": [
                            {
                                "index": 0,
                                "id": "call_1",
                                "type": "function",
                                "function": {
                                    "name": "read_file",
                                    "arguments": '{"file_path":"README.md"}',
                                },
                            }
                        ],
                    },
                    "finish_reason": "tool_calls",
                }
            ]
        }

    async def second_stream():
        yield {
            "choices": [
                {
                    "delta": {"content": "Inspection complete."},
                    "finish_reason": "stop",
                }
            ]
        }

    responses = [first_stream(), second_stream()]

    async def fake_create_completion_response(**kwargs):
        _ = kwargs
        return responses.pop(0)

    def fake_stream_chunk_builder(*, chunks, messages):
        _ = messages
        first_delta = chunks[0]["choices"][0]["delta"]
        tool_calls = first_delta.get("tool_calls")
        if tool_calls:
            return {
                "choices": [
                    {
                        "message": {
                            "role": "assistant",
                            "content": "I will inspect the workspace.",
                            "tool_calls": [
                                {
                                    "id": "call_1",
                                    "type": "function",
                                    "function": {
                                        "name": "read_file",
                                        "arguments": '{"file_path":"README.md"}',
                                    },
                                }
                            ],
                        }
                    }
                ]
            }
        return {
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": "Inspection complete.",
                    }
                }
            ]
        }

    context_manager = ContextManager()

    with (
        patch.object(
            session,
            "_ensure_initialized_with_retry",
            new=AsyncMock(return_value=None),
        ),
        patch.object(
            session,
            "_create_completion_response",
            side_effect=fake_create_completion_response,
        ),
        patch.object(session, "_trim_messages", side_effect=lambda msgs: msgs),
        patch.object(session, "_get_tools_spec", return_value=[]),
        patch.object(session, "_get_litellm", return_value=object()),
        patch.object(
            session,
            "_get_stream_chunk_builder",
            return_value=fake_stream_chunk_builder,
        ),
        patch.object(
            session, "_handle_tool_calls", new_callable=AsyncMock
        ) as mock_tool,
    ):
        mock_tool.return_value = (False, "", [])
        result = await session.process_input(
            prompt="hi",
            context_manager=context_manager,
            system_message="sys",
            stream=True,
        )

    assert result == "Inspection complete."

    content_events = [e for e in events if e.event_type == LLMEventType.CONTENT_DELTA]
    assert len(content_events) == 2
    assert content_events[0].data.get("delta") == "I will inspect the workspace."
    assert content_events[0].data.get("is_final") is False
    assert content_events[1].data.get("delta") == "Inspection complete."
    assert content_events[1].data.get("is_final") is True


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
