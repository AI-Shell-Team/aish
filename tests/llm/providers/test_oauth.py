import httpx
import pytest
from unittest.mock import patch

from aish.llm.providers.oauth import (DEVICE_CODE_GRANT_TYPE, OAuthDeviceCode,
                                      OAuthPkceCodes, OAuthProviderSpec,
                                      exchange_authorization_code_for_tokens,
                                      poll_device_code_tokens, refresh_tokens,
                                      request_device_code)


TEST_PROVIDER = OAuthProviderSpec(
    provider_id="test-oauth",
    display_name="Test OAuth",
    client_id="client-123",
    scope="openid profile offline_access",
    authorize_url="https://example.com/oauth/authorize",
    token_url="https://example.com/oauth/token",
    device_authorization_url="https://example.com/oauth/device/code",
)


def test_exchange_authorization_code_for_tokens_uses_standard_form_fields():
    def handler(request: httpx.Request) -> httpx.Response:
        body = request.content.decode("utf-8")
        assert request.url == "https://example.com/oauth/token"
        assert "grant_type=authorization_code" in body
        assert "code=code-123" in body
        assert "redirect_uri=http%3A%2F%2Flocalhost%2Fcallback" in body
        assert "client_id=client-123" in body
        assert "code_verifier=verifier-123" in body
        return httpx.Response(
            200,
            json={
                "access_token": "access-token-123",
                "refresh_token": "refresh-token-123",
                "id_token": "id-token-123",
                "token_type": "Bearer",
                "expires_in": 3600,
            },
        )

    transport = httpx.MockTransport(handler)
    with httpx.Client(transport=transport) as client:
        tokens = exchange_authorization_code_for_tokens(
            provider=TEST_PROVIDER,
            code="code-123",
            redirect_uri="http://localhost/callback",
            pkce=OAuthPkceCodes(
                code_verifier="verifier-123",
                code_challenge="challenge-123",
            ),
            client=client,
        )

    assert tokens.access_token == "access-token-123"
    assert tokens.refresh_token == "refresh-token-123"
    assert tokens.id_token == "id-token-123"
    assert tokens.token_type == "Bearer"
    assert tokens.expires_in == 3600


def test_refresh_tokens_allows_missing_refresh_token_in_response():
    def handler(request: httpx.Request) -> httpx.Response:
        body = request.content.decode("utf-8")
        assert "grant_type=refresh_token" in body
        assert "refresh_token=refresh-token-123" in body
        return httpx.Response(
            200,
            json={
                "access_token": "new-access-token-123",
                "token_type": "Bearer",
            },
        )

    transport = httpx.MockTransport(handler)
    with httpx.Client(transport=transport) as client:
        tokens = refresh_tokens(
            provider=TEST_PROVIDER,
            refresh_token="refresh-token-123",
            client=client,
        )

    assert tokens.access_token == "new-access-token-123"
    assert tokens.refresh_token is None
    assert tokens.id_token is None


def test_request_device_code_parses_rfc8628_response():
    transport = httpx.MockTransport(
        lambda request: httpx.Response(
            200,
            json={
                "device_code": "device-code-123",
                "user_code": "ABCD-EFGH",
                "verification_uri": "https://example.com/activate",
                "verification_uri_complete": "https://example.com/activate?user_code=ABCD-EFGH",
                "expires_in": 900,
                "interval": 2,
            },
        )
    )

    with httpx.Client(transport=transport) as client:
        device_code = request_device_code(provider=TEST_PROVIDER, client=client)

    assert device_code == OAuthDeviceCode(
        device_code="device-code-123",
        user_code="ABCD-EFGH",
        verification_url="https://example.com/activate",
        verification_url_complete="https://example.com/activate?user_code=ABCD-EFGH",
        expires_in=900,
        interval=2.0,
    )


def test_poll_device_code_tokens_handles_authorization_pending_then_success():
    responses = iter(
        [
            httpx.Response(
                400,
                json={"error": "authorization_pending"},
            ),
            httpx.Response(
                200,
                json={
                    "access_token": "access-token-123",
                    "refresh_token": "refresh-token-123",
                    "token_type": "Bearer",
                },
            ),
        ]
    )

    def handler(request: httpx.Request) -> httpx.Response:
        body = request.content.decode("utf-8")
        assert DEVICE_CODE_GRANT_TYPE.replace(":", "%3A") in body
        assert "device_code=device-code-123" in body
        assert "client_id=client-123" in body
        return next(responses)

    transport = httpx.MockTransport(handler)
    with httpx.Client(transport=transport) as client:
        tokens = poll_device_code_tokens(
            provider=TEST_PROVIDER,
            device_code=OAuthDeviceCode(
                device_code="device-code-123",
                user_code="ABCD-EFGH",
                verification_url="https://example.com/activate",
                interval=0.0,
            ),
            client=client,
            timeout=1.0,
        )

    assert tokens.access_token == "access-token-123"
    assert tokens.refresh_token == "refresh-token-123"


def test_poll_device_code_tokens_handles_slow_down_then_success():
    responses = iter(
        [
            httpx.Response(400, json={"error": "slow_down"}),
            httpx.Response(200, json={"access_token": "access-token-123"}),
        ]
    )

    transport = httpx.MockTransport(lambda request: next(responses))
    with patch("aish.llm.providers.oauth.time.sleep"):
        with httpx.Client(transport=transport) as client:
            tokens = poll_device_code_tokens(
                provider=TEST_PROVIDER,
                device_code=OAuthDeviceCode(
                    device_code="device-code-123",
                    user_code="ABCD-EFGH",
                    verification_url="https://example.com/activate",
                    interval=0.0,
                ),
                client=client,
                timeout=1.0,
            )

    assert tokens.access_token == "access-token-123"


def test_poll_device_code_tokens_raises_on_access_denied():
    transport = httpx.MockTransport(
        lambda request: httpx.Response(
            400,
            json={
                "error": "access_denied",
                "error_description": "user denied access",
            },
        )
    )

    with httpx.Client(transport=transport) as client:
        with pytest.raises(RuntimeError, match="user denied access"):
            poll_device_code_tokens(
                provider=TEST_PROVIDER,
                device_code=OAuthDeviceCode(
                    device_code="device-code-123",
                    user_code="ABCD-EFGH",
                    verification_url="https://example.com/activate",
                    interval=0.0,
                ),
                client=client,
                timeout=1.0,
            )