from __future__ import annotations

import base64
import json
import os
import platform
import shutil
import subprocess
import time
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable, Optional
from urllib.parse import urlsplit

import httpx

from ...i18n import t
from .oauth import (OAuthPkceCodes, OAuthProviderSpec, OAuthTokens,
                    build_authorize_url as build_oauth_authorize_url,
                    exchange_authorization_code_for_tokens,
                    generate_pkce as generate_oauth_pkce,
                    generate_state as generate_oauth_state,
                    login_with_browser as login_with_oauth_browser,
                    login_with_device_code as login_with_oauth_device_code)
from .interface import ProviderAuthConfig, ProviderMetadata, ProviderUsageStatus

OPENAI_CODEX_PROVIDER = "openai-codex"
OPENAI_CODEX_DEFAULT_MODEL = "gpt-5.4"
OPENAI_CODEX_DEFAULT_BASE_URL = "https://chatgpt.com/backend-api/codex"
OPENAI_CODEX_AUTH_ISSUER = "https://auth.openai.com"
OPENAI_CODEX_AUTHORIZE_URL = f"{OPENAI_CODEX_AUTH_ISSUER}/oauth/authorize"
OPENAI_CODEX_REFRESH_URL = f"{OPENAI_CODEX_AUTH_ISSUER}/oauth/token"
OPENAI_CODEX_DEVICE_AUTH_BASE_URL = f"{OPENAI_CODEX_AUTH_ISSUER}/api/accounts"
OPENAI_CODEX_DEVICE_VERIFICATION_URL = f"{OPENAI_CODEX_AUTH_ISSUER}/codex/device"
OPENAI_CODEX_CLIENT_ID = "app_EMoamEEZ73f0CkXaXp7hrann"
OPENAI_CODEX_OAUTH_SCOPE = (
    "openid profile email offline_access api.connectors.read api.connectors.invoke"
)
OPENAI_CODEX_ORIGINATOR = "codex_cli_rs"
OPENAI_CODEX_USER_AGENT = (
    f"{OPENAI_CODEX_ORIGINATOR}/0.0.0 "
    f"(aish; Python/{platform.python_version()}; "
    f"{platform.system()} {platform.machine() or 'unknown'})"
)
OPENAI_CODEX_REFRESH_LEEWAY_SECONDS = 60
OPENAI_CODEX_DEFAULT_CALLBACK_PORT = 1455
OPENAI_CODEX_BROWSER_LOGIN_TIMEOUT_SECONDS = 300.0
OPENAI_CODEX_DEVICE_CODE_TIMEOUT_SECONDS = 900.0
OPENAI_CODEX_MAX_REQUEST_ATTEMPTS = 5

OPENAI_CODEX_OAUTH_PROVIDER = OAuthProviderSpec(
    provider_id=OPENAI_CODEX_PROVIDER,
    display_name="OpenAI Codex",
    client_id=OPENAI_CODEX_CLIENT_ID,
    scope=OPENAI_CODEX_OAUTH_SCOPE,
    authorize_url=OPENAI_CODEX_AUTHORIZE_URL,
    token_url=OPENAI_CODEX_REFRESH_URL,
    authorize_extra_query=(
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("originator", OPENAI_CODEX_ORIGINATOR),
    ),
    default_callback_port=OPENAI_CODEX_DEFAULT_CALLBACK_PORT,
    browser_login_timeout_seconds=OPENAI_CODEX_BROWSER_LOGIN_TIMEOUT_SECONDS,
    device_code_timeout_seconds=OPENAI_CODEX_DEVICE_CODE_TIMEOUT_SECONDS,
    device_redirect_uri=f"{OPENAI_CODEX_AUTH_ISSUER}/deviceauth/callback",
)


class OpenAICodexAuthError(RuntimeError):
    pass


class _OpenAICodexRetryableRequestError(OpenAICodexAuthError):
    pass


@dataclass
class OpenAICodexAuthState:
    auth_path: Path
    access_token: str
    refresh_token: str | None
    account_id: str
    expires_at: int | None

    def needs_refresh(
        self, *, leeway_seconds: int = OPENAI_CODEX_REFRESH_LEEWAY_SECONDS
    ) -> bool:
        if self.expires_at is None:
            return False
        return int(time.time()) >= (self.expires_at - leeway_seconds)


OpenAICodexPkceCodes = OAuthPkceCodes
OpenAICodexOAuthTokens = OAuthTokens


@dataclass
class OpenAICodexDeviceCode:
    verification_url: str
    user_code: str
    device_auth_id: str
    interval: float


@dataclass
class OpenAICodexDeviceCodeAuthorization:
    authorization_code: str
    code_verifier: str
    code_challenge: str


def is_openai_codex_model(model: str | None) -> bool:
    return bool(
        model and model.strip().lower().startswith(f"{OPENAI_CODEX_PROVIDER}/")
    )


def strip_openai_codex_prefix(model: str) -> str:
    if is_openai_codex_model(model):
        return model.split("/", 1)[1].strip()
    return model.strip()


def resolve_openai_codex_base_url(api_base: str | None) -> str:
    trimmed = (api_base or "").strip().rstrip("/")
    if not trimmed:
        return OPENAI_CODEX_DEFAULT_BASE_URL

    parsed = urlsplit(trimmed)
    if parsed.scheme not in {"http", "https"} or not parsed.netloc:
        return OPENAI_CODEX_DEFAULT_BASE_URL

    host = parsed.netloc.lower()
    if host not in {"chatgpt.com", "chat.openai.com"}:
        return OPENAI_CODEX_DEFAULT_BASE_URL

    return f"{parsed.scheme}://{parsed.netloc}/backend-api/codex"


def resolve_openai_codex_auth_path(
    explicit_path: str | os.PathLike[str] | None = None,
) -> Path:
    if explicit_path:
        return Path(explicit_path).expanduser()

    env_path = os.getenv("AISH_CODEX_AUTH_PATH")
    if env_path:
        return Path(env_path).expanduser()

    codex_home = os.getenv("CODEX_HOME")
    if codex_home:
        return Path(codex_home).expanduser() / "auth.json"

    return Path.home() / ".codex" / "auth.json"


def load_openai_codex_auth(
    auth_path: str | os.PathLike[str] | None = None,
) -> OpenAICodexAuthState:
    path = resolve_openai_codex_auth_path(auth_path)
    if not path.exists():
        raise OpenAICodexAuthError(t("cli.openai_codex.auth_not_found"))

    try:
        payload = json.loads(path.read_text(encoding="utf-8"))
    except Exception as exc:
        raise OpenAICodexAuthError(
            t("cli.openai_codex.auth_read_failed", path=path)
        ) from exc

    tokens = payload.get("tokens")
    if not isinstance(tokens, dict):
        raise OpenAICodexAuthError(t("cli.openai_codex.auth_missing_tokens"))

    access_token = _coerce_str(tokens.get("access_token"))
    refresh_token = _coerce_str(tokens.get("refresh_token")) or None
    access_token_claims = _decode_jwt_claims(access_token)
    id_token_claims = _coerce_id_token_claims(tokens.get("id_token"))

    account_id = (
        _coerce_str(tokens.get("account_id"))
        or _extract_account_id_from_claims(id_token_claims)
        or _extract_account_id_from_claims(access_token_claims)
    )
    if not access_token or not account_id:
        raise OpenAICodexAuthError(t("cli.openai_codex.auth_incomplete"))

    expires_at = _coerce_int(access_token_claims.get("exp")) or _coerce_int(
        id_token_claims.get("exp")
    )
    return OpenAICodexAuthState(
        auth_path=path,
        access_token=access_token,
        refresh_token=refresh_token,
        account_id=account_id,
        expires_at=expires_at,
    )


def generate_openai_codex_pkce() -> OpenAICodexPkceCodes:
    return generate_oauth_pkce()


def generate_openai_codex_state() -> str:
    return generate_oauth_state()


def build_openai_codex_authorize_url(
    *,
    redirect_uri: str,
    code_challenge: str,
    state: str,
    issuer: str = OPENAI_CODEX_AUTH_ISSUER,
    client_id: str = OPENAI_CODEX_CLIENT_ID,
    allowed_workspace_id: str | None = None,
) -> str:
    extra_query: list[tuple[str, str]] = []
    if allowed_workspace_id:
        extra_query.append(("allowed_workspace_id", allowed_workspace_id))
    return build_oauth_authorize_url(
        OPENAI_CODEX_OAUTH_PROVIDER,
        redirect_uri=redirect_uri,
        code_challenge=code_challenge,
        state=state,
        authorize_url=f"{issuer.rstrip('/')}/oauth/authorize",
        client_id=client_id,
        extra_query=extra_query,
    )


def exchange_openai_codex_code_for_tokens(
    *,
    code: str,
    redirect_uri: str,
    pkce: OpenAICodexPkceCodes,
    issuer: str = OPENAI_CODEX_AUTH_ISSUER,
    client_id: str = OPENAI_CODEX_CLIENT_ID,
    client: httpx.Client | None = None,
) -> OpenAICodexOAuthTokens:
    tokens = exchange_authorization_code_for_tokens(
        provider=OPENAI_CODEX_OAUTH_PROVIDER,
        code=code,
        redirect_uri=redirect_uri,
        pkce=pkce,
        client=client,
        client_id=client_id,
        token_url=f"{issuer.rstrip('/')}/oauth/token",
        error_factory=OpenAICodexAuthError,
    )

    if not tokens.id_token or not tokens.refresh_token:
        raise OpenAICodexAuthError(
            "OpenAI Codex token exchange returned incomplete credentials."
        )

    return tokens


def request_openai_codex_device_code(
    *,
    issuer: str = OPENAI_CODEX_AUTH_ISSUER,
    client_id: str = OPENAI_CODEX_CLIENT_ID,
    client: httpx.Client | None = None,
) -> OpenAICodexDeviceCode:
    owns_client = client is None
    if client is None:
        client = httpx.Client(timeout=30.0)

    try:
        response = client.post(
            f"{issuer.rstrip('/')}/api/accounts/deviceauth/usercode",
            json={"client_id": client_id},
            headers={"Content-Type": "application/json"},
        )
    except Exception as exc:
        raise OpenAICodexAuthError(
            f"OpenAI Codex device code request failed: {exc}"
        ) from exc
    finally:
        if owns_client:
            client.close()

    if response.status_code == httpx.codes.NOT_FOUND:
        raise OpenAICodexAuthError(
            "OpenAI Codex device-code login is not available on this auth server."
        )
    if response.is_error:
        detail = _extract_http_error_message(response)
        raise OpenAICodexAuthError(
            f"OpenAI Codex device code request failed: {response.status_code} {detail}"
        )

    try:
        payload = response.json()
    except Exception as exc:
        raise OpenAICodexAuthError(
            "OpenAI Codex device code request returned invalid JSON."
        ) from exc

    user_code = _coerce_str(payload.get("user_code") or payload.get("usercode"))
    device_auth_id = _coerce_str(payload.get("device_auth_id"))
    interval = _coerce_non_negative_float(payload.get("interval"), default=5.0)
    if not user_code or not device_auth_id:
        raise OpenAICodexAuthError(
            "OpenAI Codex device code request returned incomplete data."
        )

    return OpenAICodexDeviceCode(
        verification_url=f"{issuer.rstrip('/')}/codex/device",
        user_code=user_code,
        device_auth_id=device_auth_id,
        interval=interval,
    )


def poll_openai_codex_device_code_authorization(
    *,
    device_code: OpenAICodexDeviceCode,
    timeout: float = OPENAI_CODEX_DEVICE_CODE_TIMEOUT_SECONDS,
    issuer: str = OPENAI_CODEX_AUTH_ISSUER,
    client: httpx.Client | None = None,
) -> OpenAICodexDeviceCodeAuthorization:
    owns_client = client is None
    if client is None:
        client = httpx.Client(timeout=30.0)

    deadline = time.monotonic() + timeout
    try:
        while True:
            if time.monotonic() >= deadline:
                raise OpenAICodexAuthError(
                    "Timed out waiting for OpenAI Codex device-code approval."
                )

            try:
                response = client.post(
                    f"{issuer.rstrip('/')}/api/accounts/deviceauth/token",
                    json={
                        "device_auth_id": device_code.device_auth_id,
                        "user_code": device_code.user_code,
                    },
                    headers={"Content-Type": "application/json"},
                )
            except Exception as exc:
                raise OpenAICodexAuthError(
                    f"OpenAI Codex device-code polling failed: {exc}"
                ) from exc

            if response.is_success:
                try:
                    payload = response.json()
                except Exception as exc:
                    raise OpenAICodexAuthError(
                        "OpenAI Codex device-code polling returned invalid JSON."
                    ) from exc

                authorization_code = _coerce_str(payload.get("authorization_code"))
                code_verifier = _coerce_str(payload.get("code_verifier"))
                code_challenge = _coerce_str(payload.get("code_challenge"))
                if authorization_code and code_verifier and code_challenge:
                    return OpenAICodexDeviceCodeAuthorization(
                        authorization_code=authorization_code,
                        code_verifier=code_verifier,
                        code_challenge=code_challenge,
                    )
                raise OpenAICodexAuthError(
                    "OpenAI Codex device-code polling returned incomplete data."
                )

            if response.status_code not in {
                httpx.codes.FORBIDDEN,
                httpx.codes.NOT_FOUND,
            }:
                detail = _extract_http_error_message(response)
                raise OpenAICodexAuthError(
                    f"OpenAI Codex device-code polling failed: {response.status_code} {detail}"
                )

            sleep_for = min(device_code.interval, max(0.0, deadline - time.monotonic()))
            if sleep_for > 0:
                time.sleep(sleep_for)
    finally:
        if owns_client:
            client.close()


def login_openai_codex_with_browser(
    *,
    auth_path: str | os.PathLike[str] | None = None,
    issuer: str = OPENAI_CODEX_AUTH_ISSUER,
    client_id: str = OPENAI_CODEX_CLIENT_ID,
    callback_port: int = OPENAI_CODEX_DEFAULT_CALLBACK_PORT,
    timeout: float = OPENAI_CODEX_BROWSER_LOGIN_TIMEOUT_SECONDS,
    open_browser: bool = True,
    notify: Callable[[str], None] | None = None,
) -> OpenAICodexAuthState:
    return login_with_oauth_browser(
        provider=OPENAI_CODEX_OAUTH_PROVIDER,
        auth_path=auth_path,
        resolve_auth_path=resolve_openai_codex_auth_path,
        load_auth_state=load_openai_codex_auth,
        build_authorize_url=lambda redirect_uri, code_challenge, state: build_openai_codex_authorize_url(
            redirect_uri=redirect_uri,
            code_challenge=code_challenge,
            state=state,
            issuer=issuer,
            client_id=client_id,
        ),
        exchange_code_for_tokens=lambda *, code, redirect_uri, pkce, client: exchange_openai_codex_code_for_tokens(
            code=code,
            redirect_uri=redirect_uri,
            pkce=pkce,
            issuer=issuer,
            client_id=client_id,
            client=client,
        ),
        persist_tokens=persist_openai_codex_tokens,
        pkce_factory=generate_openai_codex_pkce,
        state_factory=generate_openai_codex_state,
        callback_port=callback_port,
        timeout=timeout,
        open_browser=open_browser,
        notify=notify,
        error_factory=OpenAICodexAuthError,
        format_callback_error=_format_oauth_callback_error,
    )


def login_openai_codex_with_device_code(
    *,
    auth_path: str | os.PathLike[str] | None = None,
    issuer: str = OPENAI_CODEX_AUTH_ISSUER,
    client_id: str = OPENAI_CODEX_CLIENT_ID,
    timeout: float = OPENAI_CODEX_DEVICE_CODE_TIMEOUT_SECONDS,
    notify: Callable[[str], None] | None = None,
) -> OpenAICodexAuthState:
    return login_with_oauth_device_code(
        provider=OPENAI_CODEX_OAUTH_PROVIDER,
        auth_path=auth_path,
        resolve_auth_path=resolve_openai_codex_auth_path,
        load_auth_state=load_openai_codex_auth,
        request_device_code=lambda *, client: request_openai_codex_device_code(
            issuer=issuer,
            client_id=client_id,
            client=client,
        ),
        poll_device_code_authorization=lambda *, device_code, timeout, client: poll_openai_codex_device_code_authorization(
            device_code=device_code,
            timeout=timeout,
            issuer=issuer,
            client=client,
        ),
        exchange_code_for_tokens=lambda *, code, redirect_uri, pkce, client: exchange_openai_codex_code_for_tokens(
            code=code,
            redirect_uri=redirect_uri,
            pkce=pkce,
            issuer=issuer,
            client_id=client_id,
            client=client,
        ),
        persist_tokens=persist_openai_codex_tokens,
        pkce_from_device_authorization=lambda authorization: OpenAICodexPkceCodes(
            code_verifier=authorization.code_verifier,
            code_challenge=authorization.code_challenge,
        ),
        timeout=timeout,
        device_redirect_uri=f"{issuer.rstrip('/')}/deviceauth/callback",
        notify=notify,
        error_factory=OpenAICodexAuthError,
    )


def login_openai_codex_with_codex_cli(
    *,
    auth_path: str | os.PathLike[str] | None = None,
    notify: Callable[[str], None] | None = None,
) -> OpenAICodexAuthState:
    notify = notify or print
    codex_bin = shutil.which("codex")
    if not codex_bin:
        raise OpenAICodexAuthError(
            "The `codex` CLI is not installed. Install `@openai/codex` or use "
            "`--auth-flow browser` / `--auth-flow device-code`."
        )

    try:
        subprocess.run([codex_bin, "login"], check=True)
    except subprocess.CalledProcessError as exc:
        raise OpenAICodexAuthError(
            f"`codex login` failed with exit code {exc.returncode}."
        ) from exc
    except KeyboardInterrupt as exc:
        raise OpenAICodexAuthError("`codex login` was interrupted.") from exc

    notify("Loaded auth state from the `codex` CLI login.")
    return load_openai_codex_auth(auth_path)


async def refresh_openai_codex_auth(
    auth: OpenAICodexAuthState,
    *,
    client: httpx.AsyncClient | None = None,
) -> OpenAICodexAuthState:
    if not auth.refresh_token:
        raise OpenAICodexAuthError(
            "OpenAI Codex session expired and no refresh token is available. "
            "Re-run `aish models auth --provider openai-codex`."
        )

    owns_client = client is None
    if client is None:
        client = httpx.AsyncClient(timeout=30.0)

    try:
        response = await client.post(
            OPENAI_CODEX_REFRESH_URL,
            json={
                "client_id": OPENAI_CODEX_CLIENT_ID,
                "grant_type": "refresh_token",
                "refresh_token": auth.refresh_token,
            },
            headers={"Content-Type": "application/json"},
        )
    except Exception as exc:
        raise OpenAICodexAuthError(
            f"Failed to refresh OpenAI Codex session: {exc}"
        ) from exc
    finally:
        if owns_client:
            await client.aclose()

    if response.status_code == httpx.codes.UNAUTHORIZED:
        raise OpenAICodexAuthError(
            "OpenAI Codex session is no longer valid. Re-run "
            "`aish models auth --provider openai-codex`."
        )
    if response.is_error:
        raise OpenAICodexAuthError(
            f"Failed to refresh OpenAI Codex session: {response.status_code} {response.text}"
        )

    try:
        refresh_payload = response.json()
    except Exception as exc:
        raise OpenAICodexAuthError(
            "OpenAI Codex refresh returned invalid JSON."
        ) from exc

    access_token = _coerce_str(refresh_payload.get("access_token"))
    if not access_token:
        raise OpenAICodexAuthError(
            "OpenAI Codex refresh did not return an access token."
        )

    refresh_token = _coerce_str(refresh_payload.get("refresh_token")) or auth.refresh_token
    id_token_claims = _coerce_id_token_claims(refresh_payload.get("id_token"))
    account_id = (
        _extract_account_id_from_claims(id_token_claims)
        or _extract_account_id_from_claims(_decode_jwt_claims(access_token))
        or auth.account_id
    )
    expires_at = _coerce_int(_decode_jwt_claims(access_token).get("exp"))
    _persist_openai_codex_auth(
        auth.auth_path,
        access_token=access_token,
        refresh_token=refresh_token,
        account_id=account_id,
        id_token=refresh_payload.get("id_token"),
    )
    return OpenAICodexAuthState(
        auth_path=auth.auth_path,
        access_token=access_token,
        refresh_token=refresh_token,
        account_id=account_id,
        expires_at=expires_at,
    )


def build_openai_codex_request(
    *,
    model: str,
    messages: list[dict[str, Any]],
    tools: list[dict[str, Any]] | None,
    tool_choice: str = "auto",
) -> dict[str, Any]:
    instructions: list[str] = []
    input_items: list[dict[str, Any]] = []

    for message in messages:
        role = str(message.get("role") or "").strip().lower()
        if role == "system":
            content = _coerce_message_text(message.get("content"))
            if content:
                instructions.append(content)
            continue

        if role == "user":
            content = _coerce_message_text(message.get("content"))
            if content:
                input_items.append(
                    {
                        "type": "message",
                        "role": "user",
                        "content": [{"type": "input_text", "text": content}],
                    }
                )
            continue

        if role == "assistant":
            content = _coerce_message_text(message.get("content"))
            if content:
                input_items.append(
                    {
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": content}],
                    }
                )

            for tool_call in message.get("tool_calls") or []:
                if not isinstance(tool_call, dict):
                    continue
                function = tool_call.get("function") or {}
                if not isinstance(function, dict):
                    continue
                name = _coerce_str(function.get("name"))
                call_id = _coerce_str(tool_call.get("id"))
                arguments = function.get("arguments")
                if not name or not call_id:
                    continue
                if not isinstance(arguments, str):
                    arguments = json.dumps(arguments or {}, ensure_ascii=False)
                input_items.append(
                    {
                        "type": "function_call",
                        "call_id": call_id,
                        "name": name,
                        "arguments": arguments,
                    }
                )
            continue

        if role == "tool":
            tool_call_id = _coerce_str(message.get("tool_call_id"))
            if not tool_call_id:
                continue
            content = _coerce_message_text(message.get("content"))
            input_items.append(
                {
                    "type": "function_call_output",
                    "call_id": tool_call_id,
                    "output": content,
                }
            )

    return {
        "model": strip_openai_codex_prefix(model),
        "instructions": "\n\n".join(part for part in instructions if part).strip(),
        "input": input_items,
        "tools": _convert_tools_for_openai_codex(tools or []),
        "tool_choice": tool_choice,
        "parallel_tool_calls": True,
        "store": False,
        "stream": True,
        "include": [],
    }


def convert_openai_codex_response_to_chat_completion(
    payload: dict[str, Any],
) -> dict[str, Any]:
    output = payload.get("output")
    if not isinstance(output, list):
        output = []

    content_parts: list[str] = []
    tool_calls: list[dict[str, Any]] = []

    for item in output:
        if not isinstance(item, dict):
            continue
        item_type = str(item.get("type") or "").strip().lower()
        if item_type == "message":
            text = _extract_response_message_text(item.get("content"))
            if text:
                content_parts.append(text)
            continue
        if item_type == "function_call":
            name = _coerce_str(item.get("name"))
            call_id = _coerce_str(item.get("call_id")) or _coerce_str(item.get("id"))
            arguments = item.get("arguments")
            if not name or not call_id:
                continue
            if not isinstance(arguments, str):
                arguments = json.dumps(arguments or {}, ensure_ascii=False)
            tool_calls.append(
                {
                    "id": call_id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": arguments,
                    },
                }
            )

    message: dict[str, Any] = {
        "role": "assistant",
        "content": "\n".join(part for part in content_parts if part) or "",
    }
    if tool_calls:
        message["tool_calls"] = tool_calls

    return {
        "choices": [
            {
                "message": message,
                "finish_reason": "tool_calls" if tool_calls else "stop",
            }
        ]
    }


async def create_openai_codex_chat_completion(
    *,
    model: str,
    messages: list[dict[str, Any]],
    tools: list[dict[str, Any]] | None = None,
    tool_choice: str = "auto",
    api_base: str | None = None,
    auth_path: str | os.PathLike[str] | None = None,
    timeout: float = 300.0,
) -> dict[str, Any]:
    url = f"{resolve_openai_codex_base_url(api_base)}/responses"
    request_body = build_openai_codex_request(
        model=model,
        messages=messages,
        tools=tools,
        tool_choice=tool_choice,
    )

    async with httpx.AsyncClient(timeout=timeout) as client:
        auth = load_openai_codex_auth(auth_path)
        if auth.needs_refresh() and auth.refresh_token:
            auth = await refresh_openai_codex_auth(auth, client=client)

        auth_refresh_attempted = False
        last_transport_error: httpx.TransportError | None = None

        for attempt in range(OPENAI_CODEX_MAX_REQUEST_ATTEMPTS):
            try:
                async with client.stream(
                    "POST",
                    url,
                    json=request_body,
                    headers=_build_headers(auth),
                ) as response:
                    if (
                        response.status_code == httpx.codes.UNAUTHORIZED
                        and auth.refresh_token
                        and not auth_refresh_attempted
                    ):
                        auth_refresh_attempted = True
                        auth = await refresh_openai_codex_auth(auth, client=client)
                        continue
                    if response.is_error:
                        await response.aread()
                        detail = _extract_http_error_message(response)
                        message = (
                            f"OpenAI Codex request failed: {response.status_code} {detail}"
                        )
                        raise _build_openai_codex_request_error(message)

                    content_type = _coerce_str(response.headers.get("content-type")).lower()
                    if not content_type or "text/event-stream" in content_type:
                        try:
                            payload = await _collect_openai_codex_stream_response(response)
                        except OpenAICodexAuthError as exc:
                            raise _build_openai_codex_request_error(str(exc)) from exc
                    else:
                        raw_body = await response.aread()
                        body_text = raw_body.decode("utf-8", errors="replace")
                        if _looks_like_sse_text(body_text):
                            try:
                                payload = _collect_openai_codex_stream_text(body_text)
                            except OpenAICodexAuthError as exc:
                                raise _build_openai_codex_request_error(
                                    str(exc)
                                ) from exc
                        else:
                            try:
                                payload = json.loads(body_text)
                            except Exception as exc:
                                raise _OpenAICodexRetryableRequestError(
                                    "OpenAI Codex returned invalid JSON."
                                ) from exc
                    return convert_openai_codex_response_to_chat_completion(payload)
            except httpx.TransportError as exc:
                last_transport_error = exc
                if attempt + 1 >= OPENAI_CODEX_MAX_REQUEST_ATTEMPTS:
                    break
                continue
            except _OpenAICodexRetryableRequestError:
                if attempt + 1 >= OPENAI_CODEX_MAX_REQUEST_ATTEMPTS:
                    raise
                continue

    if last_transport_error is not None:
        raise OpenAICodexAuthError(
            f"OpenAI Codex request failed after {OPENAI_CODEX_MAX_REQUEST_ATTEMPTS} attempts: {last_transport_error}"
        ) from last_transport_error

    raise OpenAICodexAuthError("OpenAI Codex request failed after retries.")


def persist_openai_codex_tokens(
    auth_path: str | os.PathLike[str],
    *,
    tokens: OpenAICodexOAuthTokens,
) -> None:
    account_id = (
        _extract_account_id_from_claims(_decode_jwt_claims(tokens.id_token))
        or _extract_account_id_from_claims(_decode_jwt_claims(tokens.access_token))
    )
    if not account_id:
        raise OpenAICodexAuthError(
            "OpenAI Codex login succeeded, but the account id was missing from the returned tokens."
        )

    _persist_openai_codex_auth(
        Path(auth_path),
        access_token=tokens.access_token,
        refresh_token=tokens.refresh_token,
        account_id=account_id,
        id_token=tokens.id_token,
    )


def _extract_http_error_message(response: httpx.Response) -> str:
    content_type = _coerce_str(response.headers.get("content-type")).lower()
    text = response.text.strip()
    if _looks_like_html_error(content_type, text):
        detail = _summarize_html_error(text)
        return _append_http_error_metadata(detail, response)

    try:
        payload = response.json()
    except Exception:
        return _append_http_error_metadata(text or "unknown error", response)

    if isinstance(payload, dict):
        error_description = _coerce_str(payload.get("error_description"))
        if error_description:
            return error_description
        error_value = payload.get("error")
        if isinstance(error_value, dict):
            error_message = _coerce_str(error_value.get("message"))
            error_code = _coerce_str(error_value.get("code"))
            if error_message:
                return error_message
            if error_code:
                return error_code
        error_text = _coerce_str(error_value)
        if error_text:
            return error_text

    return _append_http_error_metadata(text or "unknown error", response)


async def _collect_openai_codex_stream_response(
    response: httpx.Response,
) -> dict[str, Any]:
    events: list[tuple[str, dict[str, Any]]] = []
    async for event_type, payload in _iter_openai_codex_sse_events(response):
        events.append((event_type, payload))
    return _build_openai_codex_stream_payload(events)


def _collect_openai_codex_stream_text(text: str) -> dict[str, Any]:
    event_type: str | None = None
    data_lines: list[str] = []
    events: list[tuple[str, dict[str, Any]]] = []

    for raw_line in text.splitlines():
        line = raw_line.rstrip("\r")
        if not line:
            parsed = _parse_openai_codex_sse_event(event_type, data_lines)
            if parsed is not None:
                events.append(parsed)
            event_type = None
            data_lines = []
            continue

        if line.startswith(":"):
            continue

        field, separator, value = line.partition(":")
        if not separator:
            continue
        value = value.lstrip()
        if field == "event":
            event_type = value
        elif field == "data":
            data_lines.append(value)

    parsed = _parse_openai_codex_sse_event(event_type, data_lines)
    if parsed is not None:
        events.append(parsed)

    return _build_openai_codex_stream_payload(events)


def _build_openai_codex_stream_payload(
    events: list[tuple[str, dict[str, Any]]]
) -> dict[str, Any]:
    output_items: list[dict[str, Any]] = []
    text_deltas: list[str] = []
    response_id: str | None = None
    usage: Any = None
    completed = False

    for event_type, payload in events:
        if event_type == "response.output_item.done":
            item = payload.get("item")
            if isinstance(item, dict):
                output_items.append(item)
            continue

        if event_type == "response.output_text.delta":
            delta = _coerce_str(payload.get("delta"))
            if delta:
                text_deltas.append(delta)
            continue

        if event_type == "response.failed":
            raise OpenAICodexAuthError(
                _extract_openai_codex_stream_failure_message(payload)
            )

        if event_type == "response.incomplete":
            reason = (
                payload.get("response", {})
                if isinstance(payload.get("response"), dict)
                else {}
            )
            incomplete_details = (
                reason.get("incomplete_details")
                if isinstance(reason.get("incomplete_details"), dict)
                else {}
            )
            incomplete_reason = _coerce_str(incomplete_details.get("reason")) or "unknown"
            raise OpenAICodexAuthError(
                f"OpenAI Codex returned an incomplete response: {incomplete_reason}"
            )

        if event_type == "response.completed":
            response_payload = payload.get("response")
            if isinstance(response_payload, dict):
                response_id = _coerce_str(response_payload.get("id")) or response_id
                usage = response_payload.get("usage")
                if not output_items:
                    completed_output = response_payload.get("output")
                    if isinstance(completed_output, list):
                        output_items = [
                            item for item in completed_output if isinstance(item, dict)
                        ]
            completed = True
            break

    if not completed:
        raise OpenAICodexAuthError(
            "OpenAI Codex stream ended before response.completed."
        )

    if not output_items and text_deltas:
        output_items.append(
            {
                "type": "message",
                "role": "assistant",
                "content": [
                    {"type": "output_text", "text": "".join(text_deltas)},
                ],
            }
        )

    payload: dict[str, Any] = {"output": output_items}
    if response_id:
        payload["id"] = response_id
    if usage is not None:
        payload["usage"] = usage
    return payload


def _looks_like_sse_text(text: str) -> bool:
    stripped = text.lstrip()
    return stripped.startswith("event: ") or "\nevent: " in stripped


async def _iter_openai_codex_sse_events(
    response: httpx.Response,
):
    event_type: str | None = None
    data_lines: list[str] = []

    async for raw_line in response.aiter_lines():
        line = raw_line.rstrip("\r")
        if not line:
            parsed = _parse_openai_codex_sse_event(event_type, data_lines)
            if parsed is not None:
                yield parsed
            event_type = None
            data_lines = []
            continue

        if line.startswith(":"):
            continue

        field, separator, value = line.partition(":")
        if not separator:
            continue
        value = value.lstrip()
        if field == "event":
            event_type = value
        elif field == "data":
            data_lines.append(value)

    parsed = _parse_openai_codex_sse_event(event_type, data_lines)
    if parsed is not None:
        yield parsed


def _parse_openai_codex_sse_event(
    event_type: str | None, data_lines: list[str]
) -> tuple[str, dict[str, Any]] | None:
    if not data_lines:
        return None

    data = "\n".join(data_lines).strip()
    if not data or data == "[DONE]":
        return None

    try:
        payload = json.loads(data)
    except Exception:
        return None
    if not isinstance(payload, dict):
        return None

    resolved_type = _coerce_str(payload.get("type")) or _coerce_str(event_type)
    if not resolved_type:
        return None
    return resolved_type, payload


def _extract_openai_codex_stream_failure_message(payload: dict[str, Any]) -> str:
    response_payload = payload.get("response")
    if isinstance(response_payload, dict):
        error = response_payload.get("error")
        if isinstance(error, dict):
            message = _coerce_str(error.get("message"))
            if message:
                return message
            code = _coerce_str(error.get("code"))
            if code:
                return code
    return "OpenAI Codex stream failed."


def _is_retryable_openai_codex_failure_message(message: str) -> bool:
    lowered = message.strip().lower()
    if not lowered:
        return False
    retryable_markers = (
        "an error occurred while processing your request",
        "please include the request id",
        "contact us through our help center",
        "internal server error",
        "server_error",
        "returned invalid json",
        "stream ended before response.completed",
        "returned an incomplete response",
    )
    return any(marker in lowered for marker in retryable_markers)


def _build_openai_codex_request_error(message: str) -> OpenAICodexAuthError:
    if _is_retryable_openai_codex_failure_message(message):
        return _OpenAICodexRetryableRequestError(message)
    return OpenAICodexAuthError(message)


def _format_oauth_callback_error(
    error_code: str, error_description: str | None
) -> str:
    if (
        error_code == "access_denied"
        and error_description
        and "missing_codex_entitlement" in error_description.lower()
    ):
        return (
            "Codex is not enabled for this workspace. Ask the workspace administrator "
            "to grant Codex access."
        )
    if error_description:
        return f"OpenAI Codex sign-in failed: {error_description}"
    return f"OpenAI Codex sign-in failed: {error_code}"


def _coerce_message_text(value: Any) -> str:
    if value is None:
        return ""
    if isinstance(value, str):
        return value
    if isinstance(value, list):
        chunks: list[str] = []
        for item in value:
            if isinstance(item, str):
                chunks.append(item)
            elif isinstance(item, dict):
                text = item.get("text") or item.get("content")
                if isinstance(text, str):
                    chunks.append(text)
        return "\n".join(chunk for chunk in chunks if chunk)
    return str(value)


def _extract_response_message_text(content: Any) -> str:
    if not isinstance(content, list):
        return ""
    chunks: list[str] = []
    for item in content:
        if not isinstance(item, dict):
            continue
        item_type = str(item.get("type") or "").strip().lower()
        if item_type in {"output_text", "input_text"}:
            text = item.get("text")
            if isinstance(text, str) and text:
                chunks.append(text)
    return "\n".join(chunks)


def _convert_tools_for_openai_codex(
    tools: list[dict[str, Any]],
) -> list[dict[str, Any]]:
    converted: list[dict[str, Any]] = []
    for tool in tools:
        if not isinstance(tool, dict):
            continue
        if tool.get("type") != "function":
            continue
        function = tool.get("function")
        if not isinstance(function, dict):
            continue
        name = _coerce_str(function.get("name"))
        if not name:
            continue
        converted.append(
            {
                "type": "function",
                "name": name,
                "description": _coerce_str(function.get("description")),
                "parameters": function.get("parameters")
                or {"type": "object", "properties": {}},
            }
        )
    return converted


def _build_headers(auth: OpenAICodexAuthState) -> dict[str, str]:
    return {
        "Authorization": f"Bearer {auth.access_token}",
        "ChatGPT-Account-ID": auth.account_id,
        "Content-Type": "application/json",
        "User-Agent": OPENAI_CODEX_USER_AGENT,
        "originator": OPENAI_CODEX_ORIGINATOR,
    }


def _decode_jwt_claims(token: str) -> dict[str, Any]:
    parts = token.split(".")
    if len(parts) < 2:
        return {}
    payload = parts[1]
    padding = "=" * (-len(payload) % 4)
    try:
        decoded = base64.urlsafe_b64decode(f"{payload}{padding}".encode("utf-8"))
        claims = json.loads(decoded.decode("utf-8"))
    except Exception:
        return {}
    return claims if isinstance(claims, dict) else {}


def _coerce_id_token_claims(value: Any) -> dict[str, Any]:
    if isinstance(value, str):
        return _decode_jwt_claims(value)
    if isinstance(value, dict):
        return value
    return {}


def _extract_account_id_from_claims(claims: dict[str, Any]) -> str:
    auth_claims = claims.get("https://api.openai.com/auth")
    if not isinstance(auth_claims, dict):
        auth_claims = {}
    return (
        _coerce_str(claims.get("chatgpt_account_id"))
        or _coerce_str(claims.get("account_id"))
        or _coerce_str(auth_claims.get("chatgpt_account_id"))
        or _coerce_str(auth_claims.get("account_id"))
    )


def _looks_like_html_error(content_type: str, body: str) -> bool:
    if "text/html" in content_type:
        return True
    lowered = body.lstrip().lower()
    return lowered.startswith("<!doctype html") or lowered.startswith("<html")


def _summarize_html_error(body: str) -> str:
    lowered = body.lower()
    if (
        "cloudflare" in lowered
        or "enable javascript and cookies to continue" in lowered
        or "_cf_chl_opt" in lowered
        or "challenge-platform" in lowered
    ):
        return (
            "Cloudflare blocked the OpenAI Codex request. "
            "This usually means the wrong ChatGPT endpoint was used or "
            "this network/region requires an interactive challenge."
        )
    return "OpenAI Codex returned an HTML error page instead of JSON."


def _append_http_error_metadata(detail: str, response: httpx.Response) -> str:
    metadata: list[str] = []
    for header_name in ("cf-ray", "x-request-id", "request-id"):
        header_value = _coerce_str(response.headers.get(header_name))
        if header_value:
            metadata.append(f"{header_name}: {header_value}")
    if metadata:
        return f"{detail} ({', '.join(metadata)})"
    return detail


def _persist_openai_codex_auth(
    auth_path: Path,
    *,
    access_token: str,
    refresh_token: str | None,
    account_id: str,
    id_token: Any,
) -> None:
    try:
        payload = json.loads(auth_path.read_text(encoding="utf-8"))
    except Exception:
        payload = {}

    payload["auth_mode"] = "chatgpt"
    payload.pop("OPENAI_API_KEY", None)

    tokens = payload.get("tokens")
    if not isinstance(tokens, dict):
        tokens = {}
        payload["tokens"] = tokens

    tokens["access_token"] = access_token
    if refresh_token:
        tokens["refresh_token"] = refresh_token
    tokens["account_id"] = account_id

    if isinstance(id_token, str) and id_token.strip():
        tokens["id_token"] = id_token.strip()
    elif id_token is not None:
        tokens["id_token"] = id_token

    payload["last_refresh"] = datetime.now(timezone.utc).isoformat()
    auth_path.parent.mkdir(parents=True, exist_ok=True)
    auth_path.write_text(
        json.dumps(payload, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )


def _coerce_str(value: Any) -> str:
    return value.strip() if isinstance(value, str) else ""


def _coerce_int(value: Any) -> Optional[int]:
    try:
        parsed = int(value)
    except (TypeError, ValueError):
        return None
    return parsed if parsed > 0 else None


def _coerce_non_negative_float(value: Any, *, default: float) -> float:
    try:
        parsed = float(value)
    except (TypeError, ValueError):
        return default
    return parsed if parsed >= 0 else default


class OpenAICodexProviderAdapter:
    provider_id = OPENAI_CODEX_PROVIDER
    model_prefix = OPENAI_CODEX_PROVIDER
    display_name = OPENAI_CODEX_OAUTH_PROVIDER.display_name
    uses_litellm = False
    supports_streaming = False
    should_trim_messages = False
    metadata = ProviderMetadata(
        provider_id=OPENAI_CODEX_PROVIDER,
        display_name=OPENAI_CODEX_OAUTH_PROVIDER.display_name,
        dashboard_url="https://codex.ai/settings",
    )
    auth_config = ProviderAuthConfig(
        auth_path_config_key="codex_auth_path",
        default_model=OPENAI_CODEX_DEFAULT_MODEL,
        load_auth_state=lambda auth_path: load_openai_codex_auth(auth_path),
        login_handlers={
            "browser": lambda **kwargs: login_openai_codex_with_browser(**kwargs),
            "device-code": lambda **kwargs: login_openai_codex_with_device_code(
                **kwargs
            ),
            "codex-cli": lambda **kwargs: login_openai_codex_with_codex_cli(
                **kwargs
            ),
        },
    )

    def matches_model(self, model: str | None) -> bool:
        return is_openai_codex_model(model)

    def get_usage_status(self, config) -> ProviderUsageStatus | None:
        try:
            auth_state = load_openai_codex_auth(
                getattr(config, self.auth_config.auth_path_config_key, None)
            )
        except OpenAICodexAuthError as exc:
            return ProviderUsageStatus(
                summary=t("cli.models_usage.not_authenticated"),
                style="yellow",
                details=(str(exc),),
            )

        details: list[str] = [
            t("cli.models_usage.auth_file", path=auth_state.auth_path)
        ]
        if auth_state.expires_at is not None:
            expires_at = datetime.fromtimestamp(auth_state.expires_at, tz=timezone.utc)
            details.append(
                t(
                    "cli.models_usage.expires_at",
                    timestamp=expires_at.isoformat().replace('+00:00', 'Z'),
                )
            )
        return ProviderUsageStatus(
            summary=t("cli.models_usage.authenticated"),
            style="green",
            details=tuple(details),
        )

    async def create_completion(
        self,
        *,
        model: str,
        config,
        api_base: str | None,
        api_key: str | None,
        messages: list[dict[str, Any]],
        stream: bool,
        tools: list[dict[str, Any]] | None = None,
        tool_choice: str = "auto",
        fallback_completion=None,
        **kwargs: Any,
    ) -> dict[str, Any]:
        return await create_openai_codex_chat_completion(
            model=model,
            messages=messages,
            tools=tools,
            tool_choice=tool_choice,
            api_base=api_base,
            auth_path=getattr(config, self.auth_config.auth_path_config_key, None),
            timeout=float(kwargs.get("timeout", 300)),
        )

    async def validate_model_switch(self, *, model: str, config) -> str | None:
        try:
            load_openai_codex_auth(getattr(config, self.auth_config.auth_path_config_key, None))
        except OpenAICodexAuthError as exc:
            return str(exc)
        return None


OPENAI_CODEX_PROVIDER_ADAPTER = OpenAICodexProviderAdapter()
