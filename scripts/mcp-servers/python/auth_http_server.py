#!/usr/bin/env python3
"""MCP fixture server that requires `Authorization: Bearer <token>` on every request.

Used to check that the aish client surfaces an authentication failure in a clear,
bounded way instead of hanging or crashing.  Any request without the expected
bearer token gets `401` with a JSON-RPC error body; authorized requests are
served by the Streamable HTTP transport with a single `echo` tool.

    python auth_http_server.py --transport http --port N [--path /mcp] [--token T]

The token comes from --token or $MCP_FIXTURE_TOKEN.
"""

from __future__ import annotations

import argparse
import contextlib
import hmac
import json
import os
import signal
import sys
from typing import Any

import mcp.types as types
from mcp.server.lowlevel import Server

SERVER_NAME = "aish-fixture-auth"
SERVER_VERSION = "1.0.0"
UNAUTHORIZED_CODE = -32001


def _object_schema(properties: dict[str, Any] | None = None, required: list[str] | None = None) -> dict[str, Any]:
    schema: dict[str, Any] = {
        "type": "object",
        "properties": properties or {},
        "additionalProperties": False,
    }
    if required:
        schema["required"] = required
    return schema


def build_server() -> Server:
    server: Server = Server(
        SERVER_NAME,
        version=SERVER_VERSION,
        instructions="aish MCP fixture server: bearer-token protected echo.",
    )

    @server.list_tools()
    async def list_tools() -> list[types.Tool]:
        return [
            types.Tool(
                name="echo",
                description="Return the given text unchanged.",
                inputSchema=_object_schema({"text": {"type": "string"}}, ["text"]),
                annotations=types.ToolAnnotations(readOnlyHint=True, idempotentHint=True),
            )
        ]

    @server.call_tool()
    async def call_tool(name: str, arguments: dict[str, Any]) -> types.CallToolResult:
        if name != "echo":
            return types.CallToolResult(
                content=[types.TextContent(type="text", text=f"unknown tool: {name}")], isError=True
            )
        return types.CallToolResult(content=[types.TextContent(type="text", text=arguments["text"])])

    return server


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--transport", choices=["http"], default="http")
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--path", default="/mcp", help="Streamable HTTP path (default /mcp)")
    parser.add_argument("--ready-file", default=None, help="file to append 'READY <transport> <url|pid>' to")
    parser.add_argument("--root", default=None, help="state directory (default $MCP_FIXTURE_ROOT)")
    parser.add_argument("--token", default=None, help="bearer token (default $MCP_FIXTURE_TOKEN)")
    return parser.parse_args(argv)


def write_ready(ready_file: str | None, line: str) -> None:
    if not ready_file:
        return
    with open(ready_file, "a", encoding="utf-8") as handle:
        handle.write(line + "\n")
        handle.flush()


async def send_json(send, status: int, payload: dict[str, Any], extra_headers: list[tuple[bytes, bytes]] | None = None):
    body = json.dumps(payload).encode("utf-8")
    headers = [(b"content-type", b"application/json"), (b"content-length", str(len(body)).encode())]
    headers.extend(extra_headers or [])
    await send({"type": "http.response.start", "status": status, "headers": headers})
    await send({"type": "http.response.body", "body": body})


class AuthHttpApp:
    """ASGI wrapper: bearer-token gate in front of the Streamable HTTP transport."""

    def __init__(self, manager, token: str, path: str, ready_file: str | None, ready_url: str):
        self.manager = manager
        self.token = token
        self.path = "/" + path.strip("/") if path.strip("/") else "/"
        self.ready_file = ready_file
        self.ready_url = ready_url
        self._stack = contextlib.AsyncExitStack()

    def _authorized(self, scope) -> bool:
        for key, value in scope.get("headers", []):
            if key == b"authorization":
                presented = value.decode("latin-1")
                prefix = "bearer "
                if presented.lower().startswith(prefix):
                    return hmac.compare_digest(presented[len(prefix) :].strip(), self.token)
        return False

    async def __call__(self, scope, receive, send):
        if scope["type"] == "lifespan":
            await self._handle_lifespan(receive, send)
            return
        if scope["type"] != "http":
            return
        if not self._authorized(scope):
            await send_json(
                send,
                401,
                {
                    "jsonrpc": "2.0",
                    "id": None,
                    "error": {"code": UNAUTHORIZED_CODE, "message": "Unauthorized: missing or invalid bearer token"},
                },
                extra_headers=[(b"www-authenticate", b"Bearer")],
            )
            return
        if scope["path"] != self.path:
            await send_json(
                send,
                404,
                {"jsonrpc": "2.0", "id": None, "error": {"code": -32600, "message": f"not found: {scope['path']}"}},
            )
            return
        await self.manager.handle_request(scope, receive, send)

    async def _handle_lifespan(self, receive, send):
        while True:
            message = await receive()
            if message["type"] == "lifespan.startup":
                try:
                    await self._stack.enter_async_context(self.manager.run())
                except BaseException as exc:  # pragma: no cover - startup failure path
                    await send({"type": "lifespan.startup.failed", "message": repr(exc)})
                    continue
                write_ready(self.ready_file, f"READY http {self.ready_url}")
                await send({"type": "lifespan.startup.complete"})
            elif message["type"] == "lifespan.shutdown":
                await self._stack.aclose()
                await send({"type": "lifespan.shutdown.complete"})
                return


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    token = args.token or os.environ.get("MCP_FIXTURE_TOKEN")
    if not token:
        print("auth_http_server: --token or $MCP_FIXTURE_TOKEN is required", file=sys.stderr)
        return 2

    from mcp.server.streamable_http_manager import StreamableHTTPSessionManager

    manager = StreamableHTTPSessionManager(app=build_server(), json_response=False, stateless=False)
    ready_url = f"http://127.0.0.1:{args.port}{args.path}"
    app = AuthHttpApp(manager, token=token, path=args.path, ready_file=args.ready_file, ready_url=ready_url)

    import uvicorn

    # uvicorn handles SIGTERM itself and, after its graceful shutdown, re-raises
    # the captured signal.  This no-op handler keeps that re-raise from killing
    # the process, so the server ends with status 0 instead of 143.
    signal.signal(signal.SIGTERM, lambda *_: None)
    uvicorn.run(app, host="127.0.0.1", port=args.port, log_config=None, log_level="warning", lifespan="on")
    return 0


if __name__ == "__main__":
    sys.exit(main())
