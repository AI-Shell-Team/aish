from unittest.mock import AsyncMock, patch

import pytest

from aish.config import ConfigModel
from aish.llm import LLMSession
from aish.llm.providers.interface import ProviderAuthConfig, ProviderMetadata
from aish.llm.providers.registry import resolve_provider_metadata
from aish.state import ContextManager
from aish.skills import SkillManager


class _FakeProvider:
    provider_id = "fake-provider"
    model_prefix = "fake-provider"
    display_name = "Fake Provider"
    uses_litellm = False
    supports_streaming = False
    should_trim_messages = False
    metadata = ProviderMetadata(provider_id="fake-provider", display_name="Fake Provider")
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

    def get_usage_status(self, config: ConfigModel):
        return None

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


def test_resolve_provider_metadata_prefers_registered_provider():
    metadata = resolve_provider_metadata("openai-codex/gpt-5.4")

    assert metadata.provider_id == "openai-codex"
    assert metadata.display_name == "OpenAI Codex"
    assert metadata.dashboard_url == "https://codex.ai/settings"


def test_resolve_provider_metadata_infers_generic_provider_from_model_prefix():
    metadata = resolve_provider_metadata("openai/gpt-4o")

    assert metadata.provider_id == "openai"
    assert metadata.display_name == "OpenAI"
    assert metadata.api_key_env_var == "OPENAI_API_KEY"


def test_resolve_provider_metadata_prefers_api_base_for_openai_compatible_gateways():
    metadata = resolve_provider_metadata(
        "openai/gpt-4o",
        api_base="https://openrouter.ai/api/v1",
    )

    assert metadata.provider_id == "openrouter"
    assert metadata.display_name == "OpenRouter"


def test_resolve_provider_metadata_uses_configured_api_base_for_local_provider():
    metadata = resolve_provider_metadata(
        "ollama/llama3.2",
        api_base="http://192.168.1.20:11434",
    )

    assert metadata.provider_id == "ollama"
    assert metadata.display_name == "Ollama"
    assert metadata.dashboard_url == "http://192.168.1.20:11434"