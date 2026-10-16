#!/usr/bin/env python3
"""MCP fixture server speaking the legacy HTTP+SSE transport (protocol 2024-11-05).

One GET on `/sse` opens the event stream and advertises `/messages` as the
client-to-server endpoint; POSTs there carry the JSON-RPC messages.  The aish
client supports only stdio and Streamable HTTP, so this fixture exists to prove
that connecting to it fails in a clear, bounded way rather than hanging.

    python legacy_sse_server.py --transport sse --port N [--ready-file PATH]
"""

from __future__ import annotations

import argparse
import json
import os
import signal
import sys
import tempfile
from pathlib import Path
from typing import Any

import mcp.types as types
from mcp.server.lowlevel import Server
from mcp.server.sse import SseServerTransport

SERVER_NAME = "aish-fixture-legacy-sse"
SERVER_VERSION = "1.0.0"

SSE_PATH = "/sse"
MESSAGES_PATH = "/messages"


def build_server() -> Server:
    server: Server = Server(
        SERVER_NAME,
        version=SERVER_VERSION,
        instructions="aish MCP fixture server: legacy HTTP+SSE transport.",
    )

    @server.list_tools()
    async def list_tools() -> list[types.Tool]:
        return [
            types.Tool(
                name="echo",
                description="Return the given text unchanged.",
                inputSchema={
                    "type": "object",
                    "properties": {"text": {"type": "string"}},
                    "additionalProperties": False,
                    "required": ["text"],
                },
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
    parser.add_argument("--transport", choices=["sse"], default="sse")
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--ready-file", default=None, help="file to append 'READY <transport> <url|pid>' to")
    parser.add_argument("--root", default=None, help="state directory (default $MCP_FIXTURE_ROOT)")
    return parser.parse_args(argv)


def resolve_root(explicit: str | None) -> Path:
    root = Path(explicit or os.environ.get("MCP_FIXTURE_ROOT") or tempfile.mkdtemp(prefix="mcp-fixture-"))
    root.mkdir(parents=True, exist_ok=True)
    return root


def write_ready(ready_file: str | None, line: str) -> None:
    if not ready_file:
        return
    with open(ready_file, "a", encoding="utf-8") as handle:
        handle.write(line + "\n")
        handle.flush()


async def send_json(send, status: int, payload: dict[str, Any]):
    body = json.dumps(payload).encode("utf-8")
    headers = [(b"content-type", b"application/json"), (b"content-length", str(len(body)).encode())]
    await send({"type": "http.response.start", "status": status, "headers": headers})
    await send({"type": "http.response.body", "body": body})


class LegacySseApp:
    """ASGI app wiring GET /sse and POST /messages onto the low-level server."""

    def __init__(self, server: Server, init_options, ready_file: str | None, ready_url: str):
        self.server = server
        self.init_options = init_options
        self.ready_file = ready_file
        self.ready_url = ready_url
        self.sse = SseServerTransport(MESSAGES_PATH)

    async def __call__(self, scope, receive, send):
        if scope["type"] == "lifespan":
            await self._handle_lifespan(receive, send)
            return
        if scope["type"] != "http":
            return
        method, path = scope["method"], scope["path"]
        if path == SSE_PATH and method == "GET":
            async with self.sse.connect_sse(scope, receive, send) as (read_stream, write_stream):
                await self.server.run(read_stream, write_stream, self.init_options)
            return
        if path == MESSAGES_PATH and method == "POST":
            await self.sse.handle_post_message(scope, receive, send)
            return
        await send_json(
            send,
            404,
            {"jsonrpc": "2.0", "id": None, "error": {"code": -32600, "message": f"not found: {method} {path}"}},
        )

    async def _handle_lifespan(self, receive, send):
        while True:
            message = await receive()
            if message["type"] == "lifespan.startup":
                write_ready(self.ready_file, f"READY sse {self.ready_url}")
                await send({"type": "lifespan.startup.complete"})
            elif message["type"] == "lifespan.shutdown":
                await send({"type": "lifespan.shutdown.complete"})
                return


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    # Accepted for CLI uniformity; this server keeps no state files.
    resolve_root(args.root)
    server = build_server()
    init_options = server.create_initialization_options()
    ready_url = f"http://127.0.0.1:{args.port}{SSE_PATH}"
    app = LegacySseApp(server, init_options, args.ready_file, ready_url)

    import uvicorn

    # uvicorn handles SIGTERM itself and, after its graceful shutdown, re-raises
    # the captured signal.  This no-op handler keeps that re-raise from killing
    # the process, so the server ends with status 0 instead of 143.
    signal.signal(signal.SIGTERM, lambda *_: None)
    uvicorn.run(app, host="127.0.0.1", port=args.port, log_config=None, log_level="warning", lifespan="on")
    return 0


if __name__ == "__main__":
    sys.exit(main())
