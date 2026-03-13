from unittest.mock import AsyncMock, patch

import pytest

from aish.config import ConfigModel
from aish.context_manager import ContextManager
from aish.llm import LLMSession
from aish.providers.interface import ProviderAuthConfig
from aish.skills import SkillManager


class _FakeProvider:
    provider_id = "fake-provider"
    model_prefix = "fake-provider"
    display_name = "Fake Provider"
    uses_litellm = False
    supports_streaming = False
    should_trim_messages = False
    auth_config = ProviderAuthConfig(
        auth_path_config_key="codex_auth_path",
        default_model="model-x",
        load_auth_state=lambda auth_path: None,
        login_handlers={},
    )

    def __init__(self):
        self.create_completion_mock = AsyncMock(
            return_value={
                "choices": [
                    {
                        "message": {"role": "assistant", "content": "hello from provider"},
                        "finish_reason": "stop",
                    }
                ]
            }
        )

    def matches_model(self, model: str | None) -> bool:
        return True

    async def create_completion(self, **kwargs):
        return await self.create_completion_mock(**kwargs)

    async def validate_model_switch(self, *, model: str, config: ConfigModel):
        return None


@pytest.mark.anyio
async def test_llm_routes_completion_through_provider_registry():
    provider = _FakeProvider()
    session = LLMSession(
        config=ConfigModel(model="fake-provider/model-x"),
        skill_manager=SkillManager(),
    )
    context_manager = ContextManager()

    with (
        patch("aish.llm.get_provider_for_model", return_value=provider),
        patch.object(
            session,
            "_get_acompletion",
            side_effect=AssertionError("LiteLLM should not be used"),
        ),
        patch.object(session, "_get_tools_spec", return_value=[]),
    ):
        result = await session.process_input(
            prompt="hi",
            context_manager=context_manager,
            system_message="sys",
            stream=True,
        )

    assert result == "hello from provider"
    provider.create_completion_mock.assert_awaited_once()
    assert provider.create_completion_mock.await_args.kwargs["model"] == "fake-provider/model-x"