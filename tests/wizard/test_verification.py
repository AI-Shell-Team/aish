from __future__ import annotations

import pytest

from aish.wizard.verification import _check_tool_support


class _AutoOnlyToolLiteLLM:
    def __init__(self) -> None:
        self.calls: list[dict] = []

    def supports_function_calling(self, *, model: str):
        return None

    def get_supported_openai_params(self, model: str):
        return None

    async def acompletion(self, **kwargs):
        self.calls.append(kwargs)
        return {
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "tool_calls": [
                            {
                                "id": "call_123",
                                "type": "function",
                                "function": {"name": "ping", "arguments": "{}"},
                            }
                        ],
                    }
                }
            ]
        }


@pytest.mark.anyio
async def test_check_tool_support_uses_auto_mode_only():
    litellm = _AutoOnlyToolLiteLLM()

    result = await _check_tool_support(
        litellm,
        model="openai/gpt-5.1",
        api_base="http://gateway.example/v1",
        api_key="test-key",
        timeout_seconds=0.1,
    )

    assert result.supports is True
    assert [call.get("model") for call in litellm.calls] == ["openai/gpt-5.1"]
    assert "tool_choice" not in litellm.calls[0]


@pytest.mark.anyio
async def test_check_tool_support_returns_clear_error_for_bare_model_with_api_base():
    litellm = _AutoOnlyToolLiteLLM()

    result = await _check_tool_support(
        litellm,
        model="gpt-5.1",
        api_base="http://gateway.example/v1",
        api_key="test-key",
        timeout_seconds=0.1,
    )

    assert result.supports is None
    assert result.error is not None
    assert "openai/gpt-5.1" in result.error
    assert "provider" in result.error.lower() or "前缀" in result.error
    assert litellm.calls == []