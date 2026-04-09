import base64
import json
from unittest.mock import AsyncMock, patch

import httpx
import pytest

from aish.config import ConfigModel
from aish.llm import LLMSession
from aish.state import ContextManager
from aish.llm.providers.registry import LiteLLMProviderAdapter
from aish.llm.providers.openai_codex import (_collect_openai_codex_stream_response,
                                             _extract_http_error_message,
                                             OpenAICodexAuthError,
                                             OpenAICodexDeviceCode,
                                             OpenAICodexOAuthTokens,
                                             OpenAICodexPkceCodes,
                                             build_openai_codex_authorize_url,
                                             build_openai_codex_request,
                                             convert_openai_codex_response_to_chat_completion,
                                             exchange_openai_codex_code_for_tokens,
                                             load_openai_codex_auth,
                                             persist_openai_codex_tokens,
                                             poll_openai_codex_device_code_authorization,
                                             request_openai_codex_device_code,
                                             resolve_openai_codex_base_url)
from aish.skills import SkillManager


def _make_jwt(payload: dict) -> str:
    def encode_part(value: dict) -> str:
        raw = json.dumps(value, separators=(",", ":")).encode("utf-8")
        return base64.urlsafe_b64encode(raw).rstrip(b"=").decode("utf-8")

    header = {"alg": "none", "typ": "JWT"}
    return f"{encode_part(header)}.{encode_part(payload)}.sig"


def test_load_openai_codex_auth_extracts_account_id_from_official_id_token_shape(
    tmp_path,
):
    auth_path = tmp_path / "auth.json"
    id_token = _make_jwt(
        {
            "exp": 2_000_000_000,
            "https://api.openai.com/auth": {"chatgpt_account_id": "acct_123"},
        }
    )
    auth_path.write_text(
        json.dumps(
            {
                "auth_mode": "chatgpt",
                "tokens": {
                    "id_token": id_token,
                    "access_token": _make_jwt({"exp": 2_000_000_100}),
                    "refresh_token": "refresh_123",
                }
            }
        ),
        encoding="utf-8",
    )

    auth = load_openai_codex_auth(auth_path)

    assert auth.auth_path == auth_path
    assert auth.account_id == "acct_123"
    assert auth.refresh_token == "refresh_123"
    assert auth.expires_at == 2_000_000_100


def test_build_openai_codex_authorize_url_includes_pkce_and_originator():
    url = build_openai_codex_authorize_url(
        redirect_uri="http://localhost:1455/auth/callback",
        code_challenge="challenge_123",
        state="state_123",
    )

    assert "response_type=code" in url
    assert "client_id=app_EMoamEEZ73f0CkXaXp7hrann" in url
    assert "redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback" in url
    assert "code_challenge=challenge_123" in url
    assert "code_challenge_method=S256" in url
    assert "originator=codex_cli_rs" in url
    assert "codex_cli_simplified_flow=true" in url


@pytest.mark.parametrize(
    ("api_base", "expected"),
    [
        (None, "https://chatgpt.com/backend-api/codex"),
        ("https://chatgpt.com", "https://chatgpt.com/backend-api/codex"),
        (
            "https://chatgpt.com/backend-api",
            "https://chatgpt.com/backend-api/codex",
        ),
        (
            "https://chatgpt.com/backend-api/responses",
            "https://chatgpt.com/backend-api/codex",
        ),
        (
            "https://chat.openai.com/backend-api",
            "https://chat.openai.com/backend-api/codex",
        ),
        (
            "https://example.com/backend-api",
            "https://chatgpt.com/backend-api/codex",
        ),
    ],
)
def test_resolve_openai_codex_base_url_normalizes_official_codex_backend(
    api_base, expected
):
    assert resolve_openai_codex_base_url(api_base) == expected


def test_persist_openai_codex_tokens_writes_chatgpt_auth_json(tmp_path):
    auth_path = tmp_path / "auth.json"
    id_token = _make_jwt(
        {
            "https://api.openai.com/auth": {"chatgpt_account_id": "acct_123"},
        }
    )

    persist_openai_codex_tokens(
        auth_path,
        tokens=OpenAICodexOAuthTokens(
            id_token=id_token,
            access_token=_make_jwt({"exp": 2_000_000_000}),
            refresh_token="refresh_123",
        ),
    )

    payload = json.loads(auth_path.read_text(encoding="utf-8"))
    assert payload["auth_mode"] == "chatgpt"
    assert "OPENAI_API_KEY" not in payload
    assert payload["tokens"]["id_token"] == id_token
    assert payload["tokens"]["account_id"] == "acct_123"

    auth = load_openai_codex_auth(auth_path)
    assert auth.account_id == "acct_123"


def test_request_openai_codex_device_code_parses_response():
    transport = httpx.MockTransport(
        lambda request: httpx.Response(
            200,
            json={
                "device_auth_id": "device-auth-123",
                "user_code": "CODE-12345",
                "interval": "0",
            },
        )
    )
    with httpx.Client(transport=transport) as client:
        device_code = request_openai_codex_device_code(client=client)

    assert device_code == OpenAICodexDeviceCode(
        verification_url="https://auth.openai.com/codex/device",
        user_code="CODE-12345",
        device_auth_id="device-auth-123",
        interval=0.0,
    )


def test_poll_openai_codex_device_code_authorization_retries_until_success():
    responses = iter(
        [
            httpx.Response(404),
            httpx.Response(
                200,
                json={
                    "authorization_code": "auth-code-123",
                    "code_verifier": "verifier-123",
                    "code_challenge": "challenge-123",
                },
            ),
        ]
    )
    transport = httpx.MockTransport(lambda request: next(responses))
    with httpx.Client(transport=transport) as client:
        authorization = poll_openai_codex_device_code_authorization(
            device_code=OpenAICodexDeviceCode(
                verification_url="https://auth.openai.com/codex/device",
                user_code="CODE-12345",
                device_auth_id="device-auth-123",
                interval=0.0,
            ),
            timeout=1.0,
            client=client,
        )

    assert authorization.authorization_code == "auth-code-123"
    assert authorization.code_verifier == "verifier-123"
    assert authorization.code_challenge == "challenge-123"


def test_exchange_openai_codex_code_for_tokens_uses_form_post():
    def handler(request: httpx.Request) -> httpx.Response:
        body = request.content.decode("utf-8")
        assert "grant_type=authorization_code" in body
        assert "code=code-123" in body
        assert "client_id=app_EMoamEEZ73f0CkXaXp7hrann" in body
        assert "code_verifier=verifier-123" in body
        return httpx.Response(
            200,
            json={
                "id_token": "id-token-123",
                "access_token": "access-token-123",
                "refresh_token": "refresh-token-123",
            },
        )

    transport = httpx.MockTransport(handler)
    with httpx.Client(transport=transport) as client:
        tokens = exchange_openai_codex_code_for_tokens(
            code="code-123",
            redirect_uri="http://localhost:1455/auth/callback",
            pkce=OpenAICodexPkceCodes(
                code_verifier="verifier-123",
                code_challenge="challenge-123",
            ),
            client=client,
        )

    assert tokens.id_token == "id-token-123"
    assert tokens.access_token == "access-token-123"
    assert tokens.refresh_token == "refresh-token-123"


def test_openai_codex_token_alias_supports_optional_standard_fields():
    tokens = OpenAICodexOAuthTokens(access_token="access-token-123")

    assert tokens.access_token == "access-token-123"
    assert tokens.refresh_token is None
    assert tokens.id_token is None


def test_extract_http_error_message_simplifies_cloudflare_html():
    response = httpx.Response(
        403,
        headers={"content-type": "text/html", "cf-ray": "ray-123"},
        text=(
            "<html><body>"
            "Enable JavaScript and cookies to continue"
            "<script>window._cf_chl_opt = {}</script>"
            "</body></html>"
        ),
    )

    detail = _extract_http_error_message(response)

    assert "Cloudflare blocked the OpenAI Codex request" in detail
    assert "cf-ray: ray-123" in detail


def test_build_openai_codex_request_converts_chat_messages_and_tools():
    request = build_openai_codex_request(
        model="openai-codex/gpt-5.4",
        messages=[
            {"role": "system", "content": "Follow repo conventions."},
            {"role": "user", "content": "Inspect the workspace."},
            {
                "role": "assistant",
                "content": "I will call a tool.",
                "tool_calls": [
                    {
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "bash_exec",
                            "arguments": '{"command":"pwd"}',
                        },
                    }
                ],
            },
            {
                "role": "tool",
                "tool_call_id": "call_1",
                "name": "bash_exec",
                "content": "/home/hao/CCC/aish",
            },
        ],
        tools=[
            {
                "type": "function",
                "function": {
                    "name": "bash_exec",
                    "description": "Run a shell command",
                    "parameters": {
                        "type": "object",
                        "properties": {"command": {"type": "string"}},
                    },
                },
            }
        ],
    )

    assert request["model"] == "gpt-5.4"
    assert request["instructions"] == "Follow repo conventions."
    assert request["stream"] is True
    assert request["input"][0]["role"] == "user"
    assert request["input"][0]["content"][0]["type"] == "input_text"
    assert request["input"][1]["role"] == "assistant"
    assert request["input"][2] == {
        "type": "function_call",
        "call_id": "call_1",
        "name": "bash_exec",
        "arguments": '{"command":"pwd"}',
    }
    assert request["input"][3] == {
        "type": "function_call_output",
        "call_id": "call_1",
        "output": "/home/hao/CCC/aish",
    }
    assert request["tools"] == [
        {
            "type": "function",
            "name": "bash_exec",
            "description": "Run a shell command",
            "parameters": {
                "type": "object",
                "properties": {"command": {"type": "string"}},
            },
        }
    ]


@pytest.mark.anyio
async def test_collect_openai_codex_stream_response_aggregates_sse_items():
    response = httpx.Response(
        200,
        headers={"content-type": "text/event-stream"},
        text=(
            'event: response.output_item.done\n'
            'data: {"type":"response.output_item.done","item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Hello"}]}}\n'
            "\n"
            'event: response.completed\n'
            'data: {"type":"response.completed","response":{"id":"resp_123"}}\n'
            "\n"
        ),
    )

    payload = await _collect_openai_codex_stream_response(response)

    assert payload["id"] == "resp_123"
    assert payload["output"] == [
        {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "Hello"}],
        }
    ]


@pytest.mark.anyio
async def test_create_openai_codex_chat_completion_accepts_sse_without_content_type(
    tmp_path,
):
    auth_path = tmp_path / "auth.json"
    id_token = _make_jwt(
        {
            "https://api.openai.com/auth": {"chatgpt_account_id": "acct_123"},
        }
    )
    persist_openai_codex_tokens(
        auth_path,
        tokens=OpenAICodexOAuthTokens(
            id_token=id_token,
            access_token=_make_jwt({"exp": 2_000_000_000}),
            refresh_token="refresh_123",
        ),
    )

    async def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(
            200,
            headers={"transfer-encoding": "chunked"},
            text=(
                'event: response.output_item.done\n'
                'data: {"type":"response.output_item.done","item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"你好"}]}}\n'
                "\n"
                'event: response.completed\n'
                'data: {"type":"response.completed","response":{"id":"resp_123"}}\n'
                "\n"
            ),
        )

    transport = httpx.MockTransport(handler)
    real_async_client = httpx.AsyncClient

    with patch(
        "aish.llm.providers.openai_codex.httpx.AsyncClient",
        side_effect=lambda *args, **kwargs: real_async_client(transport=transport),
    ):
        from aish.llm.providers.openai_codex import create_openai_codex_chat_completion

        result = await create_openai_codex_chat_completion(
            model="openai-codex/gpt-5.4",
            messages=[{"role": "user", "content": "你好"}],
            auth_path=auth_path,
        )

    assert result["choices"][0]["message"]["content"] == "你好"


@pytest.mark.anyio
async def test_create_openai_codex_chat_completion_retries_transient_backend_failure(
    tmp_path,
):
    auth_path = tmp_path / "auth.json"
    id_token = _make_jwt(
        {
            "https://api.openai.com/auth": {"chatgpt_account_id": "acct_123"},
        }
    )
    persist_openai_codex_tokens(
        auth_path,
        tokens=OpenAICodexOAuthTokens(
            id_token=id_token,
            access_token=_make_jwt({"exp": 2_000_000_000}),
            refresh_token="refresh_123",
        ),
    )

    attempts = 0

    async def handler(request: httpx.Request) -> httpx.Response:
        nonlocal attempts
        attempts += 1
        if attempts == 1:
            return httpx.Response(
                200,
                headers={"content-type": "text/event-stream"},
                text=(
                    "event: response.failed\n"
                    'data: {"type":"response.failed","response":{"error":{"message":"An error occurred while processing your request. You can retry your request, or contact us through our help center at help.openai.com if the error persists. Please include the request ID req_retry_123 in your message."}}}\n'
                    "\n"
                ),
            )

        return httpx.Response(
            200,
            headers={"content-type": "text/event-stream"},
            text=(
                'event: response.output_item.done\n'
                'data: {"type":"response.output_item.done","item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"retry ok"}]}}\n'
                "\n"
                'event: response.completed\n'
                'data: {"type":"response.completed","response":{"id":"resp_retry_ok"}}\n'
                "\n"
            ),
        )

    transport = httpx.MockTransport(handler)
    real_async_client = httpx.AsyncClient

    with patch(
        "aish.llm.providers.openai_codex.httpx.AsyncClient",
        side_effect=lambda *args, **kwargs: real_async_client(transport=transport),
    ):
        from aish.llm.providers.openai_codex import create_openai_codex_chat_completion

        result = await create_openai_codex_chat_completion(
            model="openai-codex/gpt-5.4",
            messages=[{"role": "user", "content": "你好"}],
            auth_path=auth_path,
        )

    assert attempts == 2
    assert result["choices"][0]["message"]["content"] == "retry ok"


@pytest.mark.anyio
async def test_create_openai_codex_chat_completion_does_not_retry_refresh_failures(
    tmp_path,
):
    auth_path = tmp_path / "auth.json"
    id_token = _make_jwt(
        {
            "https://api.openai.com/auth": {"chatgpt_account_id": "acct_123"},
        }
    )
    persist_openai_codex_tokens(
        auth_path,
        tokens=OpenAICodexOAuthTokens(
            id_token=id_token,
            access_token=_make_jwt({"exp": 2_000_000_000}),
            refresh_token="refresh_123",
        ),
    )

    attempts = 0

    async def handler(request: httpx.Request) -> httpx.Response:
        nonlocal attempts
        attempts += 1
        return httpx.Response(
            401,
            headers={"content-type": "application/json"},
            json={"error": {"message": "unauthorized"}},
        )

    transport = httpx.MockTransport(handler)
    real_async_client = httpx.AsyncClient

    with (
        patch(
            "aish.llm.providers.openai_codex.httpx.AsyncClient",
            side_effect=lambda *args, **kwargs: real_async_client(transport=transport),
        ),
        patch(
            "aish.llm.providers.openai_codex.refresh_openai_codex_auth",
            new=AsyncMock(
                side_effect=OpenAICodexAuthError(
                    "OpenAI Codex returned invalid JSON."
                )
            ),
        ) as mock_refresh,
    ):
        from aish.llm.providers.openai_codex import create_openai_codex_chat_completion

        with pytest.raises(OpenAICodexAuthError, match="returned invalid JSON"):
            await create_openai_codex_chat_completion(
                model="openai-codex/gpt-5.4",
                messages=[{"role": "user", "content": "你好"}],
                auth_path=auth_path,
            )

    assert attempts == 1
    assert mock_refresh.await_count == 1


@pytest.mark.anyio
async def test_create_openai_codex_chat_completion_retries_transport_errors_up_to_five_attempts(
    tmp_path,
):
    auth_path = tmp_path / "auth.json"
    id_token = _make_jwt(
        {
            "https://api.openai.com/auth": {"chatgpt_account_id": "acct_123"},
        }
    )
    persist_openai_codex_tokens(
        auth_path,
        tokens=OpenAICodexOAuthTokens(
            id_token=id_token,
            access_token=_make_jwt({"exp": 2_000_000_000}),
            refresh_token="refresh_123",
        ),
    )

    attempts = 0

    async def handler(request: httpx.Request) -> httpx.Response:
        nonlocal attempts
        attempts += 1
        if attempts < 5:
            raise httpx.ReadTimeout("timed out", request=request)

        return httpx.Response(
            200,
            headers={"content-type": "text/event-stream"},
            text=(
                'event: response.output_item.done\n'
                'data: {"type":"response.output_item.done","item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"network recovered"}]}}\n'
                "\n"
                'event: response.completed\n'
                'data: {"type":"response.completed","response":{"id":"resp_recovered"}}\n'
                "\n"
            ),
        )

    transport = httpx.MockTransport(handler)
    real_async_client = httpx.AsyncClient

    with patch(
        "aish.llm.providers.openai_codex.httpx.AsyncClient",
        side_effect=lambda *args, **kwargs: real_async_client(transport=transport),
    ):
        from aish.llm.providers.openai_codex import create_openai_codex_chat_completion

        result = await create_openai_codex_chat_completion(
            model="openai-codex/gpt-5.4",
            messages=[{"role": "user", "content": "你好"}],
            auth_path=auth_path,
        )

    assert attempts == 5
    assert result["choices"][0]["message"]["content"] == "network recovered"


@pytest.mark.anyio
async def test_create_openai_codex_chat_completion_stops_after_five_retryable_failures(
    tmp_path,
):
    auth_path = tmp_path / "auth.json"
    id_token = _make_jwt(
        {
            "https://api.openai.com/auth": {"chatgpt_account_id": "acct_123"},
        }
    )
    persist_openai_codex_tokens(
        auth_path,
        tokens=OpenAICodexOAuthTokens(
            id_token=id_token,
            access_token=_make_jwt({"exp": 2_000_000_000}),
            refresh_token="refresh_123",
        ),
    )

    attempts = 0

    async def handler(request: httpx.Request) -> httpx.Response:
        nonlocal attempts
        attempts += 1
        return httpx.Response(
            200,
            headers={"content-type": "text/event-stream"},
            text=(
                "event: response.failed\n"
                'data: {"type":"response.failed","response":{"error":{"message":"An error occurred while processing your request. You can retry your request, or contact us through our help center at help.openai.com if the error persists. Please include the request ID req_retry_123 in your message."}}}\n'
                "\n"
            ),
        )

    transport = httpx.MockTransport(handler)
    real_async_client = httpx.AsyncClient

    with patch(
        "aish.llm.providers.openai_codex.httpx.AsyncClient",
        side_effect=lambda *args, **kwargs: real_async_client(transport=transport),
    ):
        from aish.llm.providers.openai_codex import create_openai_codex_chat_completion

        with pytest.raises(OpenAICodexAuthError, match="request ID req_retry_123"):
            await create_openai_codex_chat_completion(
                model="openai-codex/gpt-5.4",
                messages=[{"role": "user", "content": "你好"}],
                auth_path=auth_path,
            )

    assert attempts == 5


def test_convert_openai_codex_response_to_chat_completion_maps_tool_calls():
    response = convert_openai_codex_response_to_chat_completion(
        {
            "output": [
                {
                    "type": "message",
                    "content": [{"type": "output_text", "text": "Checking now."}],
                },
                {
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "bash_exec",
                    "arguments": '{"command":"pwd"}',
                },
            ]
        }
    )

    choice = response["choices"][0]
    assert choice["finish_reason"] == "tool_calls"
    assert choice["message"]["content"] == "Checking now."
    assert choice["message"]["tool_calls"] == [
        {
            "id": "call_1",
            "type": "function",
            "function": {
                "name": "bash_exec",
                "arguments": '{"command":"pwd"}',
            },
        }
    ]


@pytest.mark.anyio
async def test_litellm_provider_forwards_tools_and_tool_choice():
    provider = LiteLLMProviderAdapter()
    fallback_completion = AsyncMock(return_value={"choices": []})
    tools = [
        {
            "type": "function",
            "function": {
                "name": "bash_exec",
                "description": "Run a shell command",
                "parameters": {
                    "type": "object",
                    "properties": {"command": {"type": "string"}},
                },
            },
        }
    ]

    await provider.create_completion(
        model="openai/gpt-5.2-codex",
        config=ConfigModel(model="openai/gpt-5.2-codex"),
        api_base="https://api.openai.com/v1",
        api_key="test-key",
        messages=[{"role": "user", "content": "inspect processes"}],
        stream=True,
        tools=tools,
        tool_choice="auto",
        fallback_completion=fallback_completion,
    )

    assert fallback_completion.await_count == 1
    assert fallback_completion.await_args.kwargs["tools"] == tools
    assert fallback_completion.await_args.kwargs["tool_choice"] == "auto"


@pytest.mark.anyio
async def test_process_input_uses_openai_codex_path_when_stream_requested():
    session = LLMSession(
        config=ConfigModel(
            model="openai-codex/gpt-5.4",
            codex_auth_path="/tmp/codex-auth.json",
        ),
        skill_manager=SkillManager(),
    )
    context_manager = ContextManager()

    with (
        patch.object(
            session,
            "_get_acompletion",
            side_effect=AssertionError("LiteLLM should not be used for openai-codex"),
        ),
        patch.object(session, "_get_tools_spec", return_value=[]),
        patch(
            "aish.llm.providers.openai_codex.create_openai_codex_chat_completion",
            new=AsyncMock(
                return_value={
                    "choices": [
                        {
                            "message": {
                                "role": "assistant",
                                "content": "hello from codex",
                            },
                            "finish_reason": "stop",
                        }
                    ]
                }
            ),
        ) as mock_create,
    ):
        result = await session.process_input(
            prompt="hi",
            context_manager=context_manager,
            system_message="sys",
            stream=True,
        )

    assert result == "hello from codex"
    assert mock_create.await_count == 1
    assert mock_create.await_args.kwargs["model"] == "openai-codex/gpt-5.4"
    assert mock_create.await_args.kwargs["auth_path"] == "/tmp/codex-auth.json"
