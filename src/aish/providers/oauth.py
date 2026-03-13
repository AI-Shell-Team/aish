from __future__ import annotations

import base64
import hashlib
import os
import secrets
import threading
import time
import webbrowser
from dataclasses import dataclass
from html import escape
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any, Callable, Protocol, TypeVar
from urllib.parse import parse_qs, urlencode, urlsplit

import httpx


DEVICE_CODE_GRANT_TYPE = "urn:ietf:params:oauth:grant-type:device_code"


@dataclass(frozen=True)
class OAuthPkceCodes:
    code_verifier: str
    code_challenge: str


@dataclass(frozen=True)
class OAuthTokens:
    access_token: str
    refresh_token: str | None = None
    id_token: str | None = None
    token_type: str | None = None
    scope: str | None = None
    expires_in: int | None = None


@dataclass(frozen=True)
class OAuthDeviceCode:
    device_code: str
    user_code: str
    verification_url: str
    verification_url_complete: str | None = None
    expires_in: int | None = None
    interval: float = 5.0


@dataclass(frozen=True)
class OAuthDeviceFlowTokens:
    access_token: str
    refresh_token: str | None = None
    id_token: str | None = None
    token_type: str | None = None
    scope: str | None = None
    expires_in: int | None = None


@dataclass(frozen=True)
class OAuthProviderSpec:
    provider_id: str
    display_name: str
    client_id: str
    scope: str
    authorize_url: str
    token_url: str | None = None
    device_authorization_url: str | None = None
    authorize_extra_query: tuple[tuple[str, str], ...] = ()
    default_callback_port: int = 0
    browser_login_timeout_seconds: float = 300.0
    device_code_timeout_seconds: float = 900.0
    device_redirect_uri: str | None = None


@dataclass
class OAuthBrowserCallbackResult:
    code: str | None = None
    error: str | None = None
    error_description: str | None = None


class OAuthPkceLike(Protocol):
    @property
    def code_verifier(self) -> str: ...

    @property
    def code_challenge(self) -> str: ...


class OAuthDeviceCodeLike(Protocol):
    verification_url: str
    user_code: str


class OAuthDeviceAuthorizationLike(Protocol):
    authorization_code: str
    code_verifier: str
    code_challenge: str


TAuthState = TypeVar("TAuthState")
TTokens = TypeVar("TTokens")
TPersistedTokens = TypeVar("TPersistedTokens", contravariant=True)
TPkce = TypeVar("TPkce", bound=OAuthPkceLike)
TDeviceCode = TypeVar("TDeviceCode", bound=OAuthDeviceCodeLike)
TDeviceAuthorization = TypeVar(
    "TDeviceAuthorization", bound=OAuthDeviceAuthorizationLike
)


class PersistTokensFunc(Protocol[TPersistedTokens]):
    def __call__(
        self, auth_path: str | os.PathLike[str], *, tokens: TPersistedTokens
    ) -> None: ...


def generate_pkce() -> OAuthPkceCodes:
    verifier = base64.urlsafe_b64encode(os.urandom(64)).rstrip(b"=").decode("utf-8")
    challenge = base64.urlsafe_b64encode(
        hashlib.sha256(verifier.encode("utf-8")).digest()
    ).rstrip(b"=").decode("utf-8")
    return OAuthPkceCodes(code_verifier=verifier, code_challenge=challenge)


def generate_state() -> str:
    return secrets.token_urlsafe(32)


def build_authorize_url(
    provider: OAuthProviderSpec,
    *,
    redirect_uri: str,
    code_challenge: str,
    state: str,
    authorize_url: str | None = None,
    client_id: str | None = None,
    extra_query: list[tuple[str, str]] | None = None,
) -> str:
    query: list[tuple[str, str]] = [
        ("response_type", "code"),
        ("client_id", client_id or provider.client_id),
        ("redirect_uri", redirect_uri),
        ("scope", provider.scope),
        ("code_challenge", code_challenge),
        ("code_challenge_method", "S256"),
        ("state", state),
    ]
    query.extend(provider.authorize_extra_query)
    if extra_query:
        query.extend(extra_query)
    return f"{(authorize_url or provider.authorize_url).rstrip('/')}?{urlencode(query)}"


def login_with_browser(
    *,
    provider: OAuthProviderSpec,
    auth_path: str | os.PathLike[str] | None,
    resolve_auth_path: Callable[[str | os.PathLike[str] | None], Path],
    load_auth_state: Callable[[str | os.PathLike[str] | None], TAuthState],
    build_authorize_url: Callable[[str, str, str], str],
    exchange_code_for_tokens: Callable[..., TTokens],
    persist_tokens: PersistTokensFunc[TTokens],
    pkce_factory: Callable[[], TPkce] = generate_pkce,
    state_factory: Callable[[], str] = generate_state,
    callback_port: int | None = None,
    timeout: float | None = None,
    open_browser: bool = True,
    notify: Callable[[str], None] | None = None,
    error_factory: Callable[[str], Exception] = RuntimeError,
    format_callback_error: Callable[[str, str | None], str] | None = None,
) -> TAuthState:
    notify = notify or print
    resolved_auth_path = resolve_auth_path(auth_path)
    pkce = pkce_factory()
    state = state_factory()
    server = _bind_callback_server(
        display_name=provider.display_name,
        expected_state=state,
        callback_port=(
            provider.default_callback_port if callback_port is None else callback_port
        ),
        error_factory=error_factory,
        format_callback_error=format_callback_error,
    )
    server_thread = threading.Thread(target=server.serve_forever, daemon=True)
    server_thread.start()

    redirect_uri = f"http://localhost:{server.server_address[1]}/auth/callback"
    auth_url = build_authorize_url(redirect_uri, pkce.code_challenge, state)

    notify(f"Open this URL to sign in with {provider.display_name}:\n{auth_url}")
    if open_browser:
        try:
            opened = webbrowser.open(auth_url)
        except Exception as exc:
            opened = False
            notify(f"Could not open a browser automatically: {exc}")
        if not opened:
            notify("Browser auto-open failed. Open the URL above manually.")

    try:
        result = _wait_for_browser_callback(
            server,
            provider_name=provider.display_name,
            timeout=(
                provider.browser_login_timeout_seconds if timeout is None else timeout
            ),
            error_factory=error_factory,
        )
        if result.error:
            formatter = format_callback_error or (
                lambda error_code, error_description: _format_oauth_callback_error(
                    provider.display_name,
                    error_code,
                    error_description,
                )
            )
            raise error_factory(formatter(result.error, result.error_description))

        if not result.code:
            raise error_factory(
                f"{provider.display_name} browser login did not return an authorization code."
            )

        with httpx.Client(timeout=30.0) as client:
            tokens = exchange_code_for_tokens(
                code=result.code,
                redirect_uri=redirect_uri,
                pkce=pkce,
                client=client,
            )
            persist_tokens(resolved_auth_path, tokens=tokens)
        return load_auth_state(resolved_auth_path)
    finally:
        _stop_callback_server(server, server_thread)


def exchange_authorization_code_for_tokens(
    *,
    provider: OAuthProviderSpec,
    code: str,
    redirect_uri: str,
    pkce: OAuthPkceLike,
    client: httpx.Client | None = None,
    client_id: str | None = None,
    token_url: str | None = None,
    extra_form_fields: dict[str, str] | None = None,
    extra_headers: dict[str, str] | None = None,
    timeout: float = 30.0,
    error_factory: Callable[[str], Exception] = RuntimeError,
) -> OAuthTokens:
    resolved_token_url = token_url or provider.token_url
    if not resolved_token_url:
        raise error_factory(
            f"{provider.display_name} token exchange is missing a token endpoint."
        )

    form_fields = {
        "grant_type": "authorization_code",
        "code": code,
        "redirect_uri": redirect_uri,
        "client_id": client_id or provider.client_id,
        "code_verifier": pkce.code_verifier,
    }
    if extra_form_fields:
        form_fields.update(extra_form_fields)

    return _post_for_oauth_tokens(
        provider=provider,
        url=resolved_token_url,
        form_fields=form_fields,
        extra_headers=extra_headers,
        client=client,
        timeout=timeout,
        operation_name="token exchange",
        error_factory=error_factory,
    )


def refresh_tokens(
    *,
    provider: OAuthProviderSpec,
    refresh_token: str,
    client: httpx.Client | None = None,
    client_id: str | None = None,
    token_url: str | None = None,
    extra_form_fields: dict[str, str] | None = None,
    extra_headers: dict[str, str] | None = None,
    timeout: float = 30.0,
    error_factory: Callable[[str], Exception] = RuntimeError,
) -> OAuthTokens:
    resolved_token_url = token_url or provider.token_url
    if not resolved_token_url:
        raise error_factory(
            f"{provider.display_name} refresh flow is missing a token endpoint."
        )

    form_fields = {
        "grant_type": "refresh_token",
        "refresh_token": refresh_token,
        "client_id": client_id or provider.client_id,
    }
    if extra_form_fields:
        form_fields.update(extra_form_fields)

    return _post_for_oauth_tokens(
        provider=provider,
        url=resolved_token_url,
        form_fields=form_fields,
        extra_headers=extra_headers,
        client=client,
        timeout=timeout,
        operation_name="refresh",
        error_factory=error_factory,
    )


def request_device_code(
    *,
    provider: OAuthProviderSpec,
    client: httpx.Client | None = None,
    client_id: str | None = None,
    device_authorization_url: str | None = None,
    extra_form_fields: dict[str, str] | None = None,
    extra_headers: dict[str, str] | None = None,
    timeout: float = 30.0,
    error_factory: Callable[[str], Exception] = RuntimeError,
) -> OAuthDeviceCode:
    resolved_device_url = device_authorization_url or provider.device_authorization_url
    if not resolved_device_url:
        raise error_factory(
            f"{provider.display_name} device-code flow is missing a device authorization endpoint."
        )

    owns_client = client is None
    if client is None:
        client = httpx.Client(timeout=timeout)

    try:
        response = client.post(
            resolved_device_url,
            data={
                "client_id": client_id or provider.client_id,
                "scope": provider.scope,
                **(extra_form_fields or {}),
            },
            headers={
                "Content-Type": "application/x-www-form-urlencoded",
                **(extra_headers or {}),
            },
        )
    except Exception as exc:
        raise error_factory(
            f"{provider.display_name} device code request failed: {exc}"
        ) from exc
    finally:
        if owns_client:
            client.close()

    if response.is_error:
        detail = _extract_error_message(response)
        raise error_factory(
            f"{provider.display_name} device code request failed: {response.status_code} {detail}"
        )

    payload = _parse_json_response(
        response,
        error_factory,
        f"{provider.display_name} device code request returned invalid JSON.",
    )
    device_code = _coerce_str(payload.get("device_code"))
    user_code = _coerce_str(payload.get("user_code"))
    verification_url = _coerce_str(
        payload.get("verification_uri") or payload.get("verification_url")
    )
    verification_url_complete = _coerce_str(payload.get("verification_uri_complete"))
    expires_in = _coerce_int(payload.get("expires_in"))
    interval = _coerce_non_negative_float(payload.get("interval"), default=5.0)
    if not device_code or not user_code or not verification_url:
        raise error_factory(
            f"{provider.display_name} device code request returned incomplete data."
        )

    return OAuthDeviceCode(
        device_code=device_code,
        user_code=user_code,
        verification_url=verification_url,
        verification_url_complete=verification_url_complete or None,
        expires_in=expires_in,
        interval=interval,
    )


def poll_device_code_tokens(
    *,
    provider: OAuthProviderSpec,
    device_code: OAuthDeviceCode,
    client: httpx.Client | None = None,
    client_id: str | None = None,
    token_url: str | None = None,
    extra_form_fields: dict[str, str] | None = None,
    extra_headers: dict[str, str] | None = None,
    timeout: float | None = None,
    error_factory: Callable[[str], Exception] = RuntimeError,
) -> OAuthTokens:
    resolved_token_url = token_url or provider.token_url
    if not resolved_token_url:
        raise error_factory(
            f"{provider.display_name} device-code polling is missing a token endpoint."
        )

    owns_client = client is None
    if client is None:
        client = httpx.Client(timeout=30.0)

    deadline = time.monotonic() + (
        provider.device_code_timeout_seconds if timeout is None else timeout
    )
    poll_interval = max(0.0, device_code.interval)

    try:
        while True:
            if time.monotonic() >= deadline:
                raise error_factory(
                    f"Timed out waiting for {provider.display_name} device-code approval."
                )

            try:
                response = client.post(
                    resolved_token_url,
                    data={
                        "grant_type": DEVICE_CODE_GRANT_TYPE,
                        "device_code": device_code.device_code,
                        "client_id": client_id or provider.client_id,
                        **(extra_form_fields or {}),
                    },
                    headers={
                        "Content-Type": "application/x-www-form-urlencoded",
                        **(extra_headers or {}),
                    },
                )
            except Exception as exc:
                raise error_factory(
                    f"{provider.display_name} device-code polling failed: {exc}"
                ) from exc

            if response.is_success:
                payload = _parse_json_response(
                    response,
                    error_factory,
                    f"{provider.display_name} device-code polling returned invalid JSON.",
                )
                return _parse_oauth_tokens_payload(
                    payload,
                    provider_name=provider.display_name,
                    operation_name="device-code polling",
                    error_factory=error_factory,
                )

            payload = _try_parse_json(response)
            error_code = _coerce_str(payload.get("error")) if payload else ""
            if error_code == "authorization_pending":
                _sleep_for_poll_interval(poll_interval, deadline)
                continue
            if error_code == "slow_down":
                poll_interval += 5.0
                _sleep_for_poll_interval(poll_interval, deadline)
                continue
            if error_code in {"expired_token", "access_denied"}:
                detail = _coerce_str(payload.get("error_description")) or error_code
                raise error_factory(
                    f"{provider.display_name} device-code polling failed: {detail}"
                )

            detail = _extract_error_message(response)
            raise error_factory(
                f"{provider.display_name} device-code polling failed: {response.status_code} {detail}"
            )
    finally:
        if owns_client:
            client.close()


def login_with_standard_device_code(
    *,
    provider: OAuthProviderSpec,
    auth_path: str | os.PathLike[str] | None,
    resolve_auth_path: Callable[[str | os.PathLike[str] | None], Path],
    load_auth_state: Callable[[str | os.PathLike[str] | None], TAuthState],
    persist_tokens: PersistTokensFunc[OAuthTokens],
    client_id: str | None = None,
    device_authorization_url: str | None = None,
    token_url: str | None = None,
    request_extra_form_fields: dict[str, str] | None = None,
    token_extra_form_fields: dict[str, str] | None = None,
    request_extra_headers: dict[str, str] | None = None,
    token_extra_headers: dict[str, str] | None = None,
    timeout: float | None = None,
    notify: Callable[[str], None] | None = None,
    error_factory: Callable[[str], Exception] = RuntimeError,
) -> TAuthState:
    notify = notify or print
    resolved_auth_path = resolve_auth_path(auth_path)

    with httpx.Client(timeout=30.0) as client:
        device_code = request_device_code(
            provider=provider,
            client=client,
            client_id=client_id,
            device_authorization_url=device_authorization_url,
            extra_form_fields=request_extra_form_fields,
            extra_headers=request_extra_headers,
            error_factory=error_factory,
        )
        verification_url = (
            device_code.verification_url_complete or device_code.verification_url
        )
        notify(
            f"{provider.display_name} device-code login\n"
            f"1. Open: {verification_url}\n"
            f"2. Enter code: {device_code.user_code}\n"
            "3. Return here after approving access."
        )
        tokens = poll_device_code_tokens(
            provider=provider,
            device_code=device_code,
            client=client,
            client_id=client_id,
            token_url=token_url,
            extra_form_fields=token_extra_form_fields,
            extra_headers=token_extra_headers,
            timeout=timeout,
            error_factory=error_factory,
        )

    persist_tokens(resolved_auth_path, tokens=tokens)
    return load_auth_state(resolved_auth_path)


def login_with_device_code(
    *,
    provider: OAuthProviderSpec,
    auth_path: str | os.PathLike[str] | None,
    resolve_auth_path: Callable[[str | os.PathLike[str] | None], Path],
    load_auth_state: Callable[[str | os.PathLike[str] | None], TAuthState],
    request_device_code: Callable[..., TDeviceCode],
    poll_device_code_authorization: Callable[..., TDeviceAuthorization],
    exchange_code_for_tokens: Callable[..., TTokens],
    persist_tokens: PersistTokensFunc[TTokens],
    pkce_from_device_authorization: Callable[[TDeviceAuthorization], OAuthPkceLike] = (
        lambda authorization: OAuthPkceCodes(
            code_verifier=authorization.code_verifier,
            code_challenge=authorization.code_challenge,
        )
    ),
    timeout: float | None = None,
    device_redirect_uri: str | None = None,
    notify: Callable[[str], None] | None = None,
    error_factory: Callable[[str], Exception] = RuntimeError,
) -> TAuthState:
    notify = notify or print
    resolved_auth_path = resolve_auth_path(auth_path)
    resolved_redirect_uri = device_redirect_uri or provider.device_redirect_uri
    if not resolved_redirect_uri:
        raise error_factory(
            f"{provider.display_name} device-code login is missing a redirect URI."
        )

    with httpx.Client(timeout=30.0) as client:
        device_code = request_device_code(client=client)
        notify(
            f"{provider.display_name} device-code login\n"
            f"1. Open: {device_code.verification_url}\n"
            f"2. Enter code: {device_code.user_code}\n"
            "3. Return here after approving access."
        )
        authorization = poll_device_code_authorization(
            device_code=device_code,
            timeout=(
                provider.device_code_timeout_seconds if timeout is None else timeout
            ),
            client=client,
        )
        tokens = exchange_code_for_tokens(
            code=authorization.authorization_code,
            redirect_uri=resolved_redirect_uri,
            pkce=pkce_from_device_authorization(authorization),
            client=client,
        )

    persist_tokens(resolved_auth_path, tokens=tokens)
    return load_auth_state(resolved_auth_path)


class _OAuthCallbackServer(ThreadingHTTPServer):
    allow_reuse_address = True
    daemon_threads = True

    def __init__(
        self,
        server_address: tuple[str, int],
        *,
        expected_state: str,
        display_name: str,
        format_callback_error: Callable[[str, str | None], str] | None,
    ):
        super().__init__(server_address, _OAuthCallbackHandler)
        self.expected_state = expected_state
        self.display_name = display_name
        self.format_callback_error = format_callback_error
        self.callback_event = threading.Event()
        self.callback_result: OAuthBrowserCallbackResult | None = None

    def set_callback_result(self, result: OAuthBrowserCallbackResult) -> None:
        if self.callback_event.is_set():
            return
        self.callback_result = result
        self.callback_event.set()


class _OAuthCallbackHandler(BaseHTTPRequestHandler):
    server: _OAuthCallbackServer

    def do_GET(self) -> None:  # noqa: N802
        parsed = urlsplit(self.path)
        if parsed.path == "/auth/callback":
            self._handle_auth_callback(parsed)
            return
        if parsed.path == "/cancel":
            self.server.set_callback_result(OAuthBrowserCallbackResult(error="cancelled"))
            self._send_response(
                200,
                "text/plain; charset=utf-8",
                "Login cancelled.".encode("utf-8"),
            )
            self._shutdown_async()
            return
        self._send_response(404, "text/plain; charset=utf-8", b"Not Found")

    def log_message(self, format: str, *args: object) -> None:
        return

    def _handle_auth_callback(self, parsed) -> None:
        params = parse_qs(parsed.query, keep_blank_values=False)
        state = _first_query_value(params, "state")
        if state != self.server.expected_state:
            message = f"State mismatch during {self.server.display_name} OAuth callback."
            self.server.set_callback_result(
                OAuthBrowserCallbackResult(
                    error="state_mismatch",
                    error_description=message,
                )
            )
            self._send_html(400, _render_error_html(self.server.display_name, message))
            self._shutdown_async()
            return

        error = _first_query_value(params, "error")
        error_description = _first_query_value(params, "error_description")
        if error:
            formatter = self.server.format_callback_error or (
                lambda error_code, description: _format_oauth_callback_error(
                    self.server.display_name,
                    error_code,
                    description,
                )
            )
            message = formatter(error, error_description)
            self.server.set_callback_result(
                OAuthBrowserCallbackResult(
                    error=error,
                    error_description=error_description,
                )
            )
            self._send_html(200, _render_error_html(self.server.display_name, message))
            self._shutdown_async()
            return

        code = _first_query_value(params, "code")
        if not code:
            message = (
                f"Missing authorization code in {self.server.display_name} OAuth callback."
            )
            self.server.set_callback_result(
                OAuthBrowserCallbackResult(
                    error="missing_authorization_code",
                    error_description=message,
                )
            )
            self._send_html(400, _render_error_html(self.server.display_name, message))
            self._shutdown_async()
            return

        self.server.set_callback_result(OAuthBrowserCallbackResult(code=code))
        self._send_html(200, _render_success_html(self.server.display_name))
        self._shutdown_async()

    def _shutdown_async(self) -> None:
        threading.Thread(target=self.server.shutdown, daemon=True).start()

    def _send_html(self, status_code: int, html: str) -> None:
        self._send_response(
            status_code,
            "text/html; charset=utf-8",
            html.encode("utf-8"),
        )

    def _send_response(
        self, status_code: int, content_type: str, body: bytes
    ) -> None:
        self.send_response(status_code)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(body)


def _bind_callback_server(
    *,
    display_name: str,
    expected_state: str,
    callback_port: int,
    error_factory: Callable[[str], Exception],
    format_callback_error: Callable[[str, str | None], str] | None,
) -> _OAuthCallbackServer:
    ports = [callback_port]
    if callback_port != 0:
        ports.append(0)

    last_error: OSError | None = None
    for port in ports:
        try:
            return _OAuthCallbackServer(
                ("127.0.0.1", port),
                expected_state=expected_state,
                display_name=display_name,
                format_callback_error=format_callback_error,
            )
        except OSError as exc:
            last_error = exc

    raise error_factory(f"Failed to bind {display_name} callback server: {last_error}")


def _wait_for_browser_callback(
    server: _OAuthCallbackServer,
    *,
    provider_name: str,
    timeout: float,
    error_factory: Callable[[str], Exception],
) -> OAuthBrowserCallbackResult:
    if not server.callback_event.wait(timeout):
        raise error_factory(f"Timed out waiting for the {provider_name} browser callback.")

    result = server.callback_result
    if result is None:
        raise error_factory(
            f"{provider_name} browser login ended without a callback result."
        )
    return result


def _stop_callback_server(
    server: _OAuthCallbackServer, server_thread: threading.Thread
) -> None:
    try:
        server.shutdown()
    except Exception:
        pass
    try:
        server.server_close()
    except Exception:
        pass
    server_thread.join(timeout=2.0)


def _render_success_html(display_name: str) -> str:
    escaped_name = escape(display_name)
    return f"""<!doctype html>
<html lang=\"en\">
<head>
  <meta charset=\"utf-8\">
  <title>{escaped_name} Login Complete</title>
  <style>
    body {{ font-family: sans-serif; margin: 3rem; color: #111; }}
    code {{ background: #f3f4f6; padding: 0.1rem 0.3rem; }}
  </style>
</head>
<body>
  <h1>Sign-in complete</h1>
  <p>You can close this tab and return to <code>aish</code>.</p>
</body>
</html>
"""


def _render_error_html(display_name: str, message: str) -> str:
    escaped_name = escape(display_name)
    escaped_message = escape(message)
    return f"""<!doctype html>
<html lang=\"en\">
<head>
  <meta charset=\"utf-8\">
  <title>{escaped_name} Login Failed</title>
  <style>
    body {{ font-family: sans-serif; margin: 3rem; color: #111; }}
    pre {{ background: #f3f4f6; padding: 1rem; white-space: pre-wrap; }}
  </style>
</head>
<body>
  <h1>Sign-in failed</h1>
  <pre>{escaped_message}</pre>
</body>
</html>
"""


def _format_oauth_callback_error(
    provider_name: str,
    error_code: str,
    error_description: str | None,
) -> str:
    if error_description:
        return f"{provider_name} sign-in failed: {error_description}"
    return f"{provider_name} sign-in failed: {error_code}"


def _first_query_value(params: dict[str, list[str]], key: str) -> str | None:
    values = params.get(key)
    if not values:
        return None
    value = values[0].strip()
    return value or None


def _post_for_oauth_tokens(
    *,
    provider: OAuthProviderSpec,
    url: str,
    form_fields: dict[str, str],
    extra_headers: dict[str, str] | None,
    client: httpx.Client | None,
    timeout: float,
    operation_name: str,
    error_factory: Callable[[str], Exception],
) -> OAuthTokens:
    owns_client = client is None
    if client is None:
        client = httpx.Client(timeout=timeout)

    try:
        response = client.post(
            url,
            data=form_fields,
            headers={
                "Content-Type": "application/x-www-form-urlencoded",
                **(extra_headers or {}),
            },
        )
    except Exception as exc:
        raise error_factory(
            f"{provider.display_name} {operation_name} failed: {exc}"
        ) from exc
    finally:
        if owns_client:
            client.close()

    if response.is_error:
        detail = _extract_error_message(response)
        raise error_factory(
            f"{provider.display_name} {operation_name} failed: {response.status_code} {detail}"
        )

    payload = _parse_json_response(
        response,
        error_factory,
        f"{provider.display_name} {operation_name} returned invalid JSON.",
    )
    return _parse_oauth_tokens_payload(
        payload,
        provider_name=provider.display_name,
        operation_name=operation_name,
        error_factory=error_factory,
    )


def _parse_oauth_tokens_payload(
    payload: dict[str, Any],
    *,
    provider_name: str,
    operation_name: str,
    error_factory: Callable[[str], Exception],
) -> OAuthTokens:
    access_token = _coerce_str(payload.get("access_token"))
    token_type = _coerce_str(payload.get("token_type")) or None
    scope = _coerce_str(payload.get("scope")) or None
    refresh_token = _coerce_str(payload.get("refresh_token")) or None
    id_token = _coerce_str(payload.get("id_token")) or None
    expires_in = _coerce_int(payload.get("expires_in"))
    if not access_token:
        raise error_factory(
            f"{provider_name} {operation_name} returned incomplete credentials."
        )
    return OAuthTokens(
        access_token=access_token,
        refresh_token=refresh_token,
        id_token=id_token,
        token_type=token_type,
        scope=scope,
        expires_in=expires_in,
    )


def _parse_json_response(
    response: httpx.Response,
    error_factory: Callable[[str], Exception],
    message: str,
) -> dict[str, Any]:
    payload = _try_parse_json(response)
    if payload is None:
        raise error_factory(message)
    return payload


def _try_parse_json(response: httpx.Response) -> dict[str, Any] | None:
    try:
        payload = response.json()
    except Exception:
        return None
    return payload if isinstance(payload, dict) else None


def _extract_error_message(response: httpx.Response) -> str:
    payload = _try_parse_json(response)
    if payload:
        error_description = _coerce_str(payload.get("error_description"))
        if error_description:
            return error_description
        error_code = _coerce_str(payload.get("error"))
        if error_code:
            return error_code
    text = response.text.strip()
    return text or "unknown error"


def _coerce_str(value: Any) -> str:
    return value.strip() if isinstance(value, str) else ""


def _coerce_int(value: Any) -> int | None:
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


def _sleep_for_poll_interval(interval: float, deadline: float) -> None:
    sleep_for = min(interval, max(0.0, deadline - time.monotonic()))
    if sleep_for > 0:
        time.sleep(sleep_for)