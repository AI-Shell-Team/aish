#!/usr/bin/env python3
"""MCP fixture server exposing the shared aish test tool set.

Part of the aish MCP-client test matrix (PR #610).  It is a plain low-level MCP
server so that every wire detail (annotations, `isError`, `structuredContent`)
is exactly what this file says it is.

Transports:
    python tools_server.py --transport stdio [--ready-file PATH]
    python tools_server.py --transport http --port N [--path /mcp]

Stdout only ever carries protocol frames for stdio runs; all logging this file
does itself goes to stderr.
"""

from __future__ import annotations

import argparse
import contextlib
import json
import os
import signal
import sys
import tempfile
from pathlib import Path
from typing import Any

import anyio
import mcp.types as types
from mcp.server.lowlevel import Server
from mcp.server.stdio import stdio_server

SERVER_NAME = "aish-fixture-tools"
SERVER_VERSION = "1.0.0"

# 1x1 RGBA PNG, verified chunk-by-chunk (IHDR/IDAT/IEND, CRCs checked).
TINY_PNG_B64 = (
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP4z8DwHwAFAAH/VscvDQAAAABJRU5ErkJggg=="
)

READ_ONLY = types.ToolAnnotations(readOnlyHint=True)
READ_ONLY_IDEMPOTENT = types.ToolAnnotations(readOnlyHint=True, idempotentHint=True)
DESTRUCTIVE = types.ToolAnnotations(destructiveHint=True)
DESTRUCTIVE_OPEN_WORLD = types.ToolAnnotations(destructiveHint=True, openWorldHint=True)

_FAIL_MESSAGE = "fixture failure: nothing to do"
_DEFAULT_UNICODE = "\u4f60\u597d\U0001f642"


def _object_schema(properties: dict[str, Any] | None = None, required: list[str] | None = None) -> dict[str, Any]:
    """Build a closed object schema; keeps the tool table readable."""
    schema: dict[str, Any] = {
        "type": "object",
        "properties": properties or {},
        "additionalProperties": False,
    }
    if required:
        schema["required"] = required
    return schema


def tool_table() -> list[types.Tool]:
    """The shared tool contract, identical to the Node fixture servers."""
    return [
        types.Tool(
            name="echo",
            description="Return the given text unchanged.",
            inputSchema=_object_schema({"text": {"type": "string"}}, ["text"]),
            annotations=READ_ONLY_IDEMPOTENT,
        ),
        types.Tool(
            name="add",
            description="Return the sum of two numbers as text.",
            inputSchema=_object_schema(
                {"a": {"type": "number"}, "b": {"type": "number"}}, ["a", "b"]
            ),
            annotations=READ_ONLY_IDEMPOTENT,
        ),
        types.Tool(
            name="slow",
            description="Sleep for the requested number of milliseconds, then report it.",
            inputSchema=_object_schema({"ms": {"type": "number"}}, ["ms"]),
            annotations=READ_ONLY,
        ),
        types.Tool(
            name="fail",
            description="Always fail with a tool-level error.",
            inputSchema=_object_schema(),
        ),
        types.Tool(
            name="big",
            description="Return a large payload of repeated 'A' lines (kb kibibytes, default 256).",
            inputSchema=_object_schema({"kb": {"type": "integer", "default": 256}}),
            annotations=READ_ONLY,
        ),
        types.Tool(
            name="image",
            description="Return one tiny PNG image content block plus a text block.",
            inputSchema=_object_schema(),
        ),
        types.Tool(
            name="structured",
            description="Return structured content {ok, count} plus a text block.",
            inputSchema=_object_schema(),
            outputSchema={
                "type": "object",
                "properties": {"ok": {"type": "boolean"}, "count": {"type": "integer"}},
                "required": ["ok", "count"],
                "additionalProperties": False,
            },
        ),
        types.Tool(
            name="unicode",
            description="Return the given text verbatim (defaults to a non-ASCII sample).",
            inputSchema=_object_schema({"text": {"type": "string", "default": _DEFAULT_UNICODE}}),
        ),
        types.Tool(
            name="write_note",
            description="Write a note file relative to --root and return its path.",
            inputSchema=_object_schema(
                {"path": {"type": "string"}, "content": {"type": "string"}}, ["path", "content"]
            ),
            annotations=DESTRUCTIVE,
        ),
        types.Tool(
            name="delete_all",
            description="Pretend to delete everything; never touches the filesystem.",
            inputSchema=_object_schema(),
            annotations=DESTRUCTIVE_OPEN_WORLD,
        ),
    ]


def text_result(text: str) -> types.CallToolResult:
    return types.CallToolResult(content=[types.TextContent(type="text", text=text)])


def error_result(text: str) -> types.CallToolResult:
    return types.CallToolResult(content=[types.TextContent(type="text", text=text)], isError=True)


def resolve_under_root(root: Path, raw_path: str) -> Path:
    """Resolve a caller-supplied path under root; absolute-looking paths are kept inside."""
    target = (root / raw_path.lstrip("/")).resolve()
    if target != root and root not in target.parents:
        raise ValueError(f"path escapes --root: {raw_path}")
    return target


def make_tool_handler(root: Path):
    """Build the shared call_tool handler bound to a state directory."""

    async def call_tool(name: str, arguments: dict[str, Any]) -> types.CallToolResult:
        match name:
            case "echo":
                return text_result(arguments["text"])
            case "add":
                return text_result(str(arguments["a"] + arguments["b"]))
            case "slow":
                ms = arguments["ms"]
                await anyio.sleep(ms / 1000.0)
                return text_result(f"slept {ms}ms")
            case "fail":
                return error_result(_FAIL_MESSAGE)
            case "big":
                kb = int(arguments.get("kb", 256))
                return text_result(("A" * 63 + "\n") * (max(kb, 0) * 16))
            case "image":
                return types.CallToolResult(
                    content=[
                        types.ImageContent(type="image", data=TINY_PNG_B64, mimeType="image/png"),
                        types.TextContent(type="text", text="tiny 1x1 png"),
                    ]
                )
            case "structured":
                return types.CallToolResult(
                    content=[types.TextContent(type="text", text="structured ok")],
                    structuredContent={"ok": True, "count": 3},
                )
            case "unicode":
                return text_result(arguments.get("text", _DEFAULT_UNICODE))
            case "write_note":
                try:
                    target = resolve_under_root(root, arguments["path"])
                except ValueError as exc:
                    return error_result(str(exc))
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_text(arguments["content"], encoding="utf-8")
                return text_result(str(target))
            case "delete_all":
                return text_result("would delete everything")
            case _:
                return error_result(f"unknown tool: {name}")

    return call_tool


def build_server(root: Path) -> Server:
    server: Server = Server(
        SERVER_NAME,
        version=SERVER_VERSION,
        instructions="aish MCP fixture server: shared tool set.",
    )

    @server.list_tools()
    async def list_tools() -> list[types.Tool]:
        return tool_table()

    @server.call_tool()
    async def call_tool(name: str, arguments: dict[str, Any]) -> types.CallToolResult:
        # Returning a CallToolResult makes the SDK pass it through untouched,
        # so isError/structuredContent reach the wire exactly as written here.
        return await make_tool_handler(root)(name, arguments)

    return server


# --------------------------------------------------------------------------- #
# startup plumbing (arguments, --root, --ready-file, transports)
# --------------------------------------------------------------------------- #


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--transport", choices=["stdio", "http"], default="stdio")
    parser.add_argument("--port", type=int, default=None, help="required for --transport http")
    parser.add_argument("--path", default="/mcp", help="Streamable HTTP path (default /mcp)")
    parser.add_argument("--ready-file", default=None, help="file to append 'READY <transport> <url|pid>' to")
    parser.add_argument("--root", default=None, help="state directory (default $MCP_FIXTURE_ROOT)")
    args = parser.parse_args(argv)
    if args.transport == "http" and args.port is None:
        parser.error("--transport http requires --port")
    return args


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


async def send_json(send, status: int, payload: dict[str, Any], extra_headers: list[tuple[bytes, bytes]] | None = None):
    body = json.dumps(payload).encode("utf-8")
    headers = [(b"content-type", b"application/json"), (b"content-length", str(len(body)).encode())]
    headers.extend(extra_headers or [])
    await send({"type": "http.response.start", "status": status, "headers": headers})
    await send({"type": "http.response.body", "body": body})


class StreamableHttpApp:
    """ASGI wrapper: path guard, lifespan-managed session manager, ready-file hook."""

    def __init__(self, manager, path: str, ready_file: str | None, ready_url: str):
        self.manager = manager
        self.path = "/" + path.strip("/") if path.strip("/") else "/"
        self.ready_file = ready_file
        self.ready_url = ready_url
        self._stack = contextlib.AsyncExitStack()

    async def __call__(self, scope, receive, send):
        if scope["type"] == "lifespan":
            await self._handle_lifespan(receive, send)
            return
        if scope["type"] != "http":
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


def serve_stdio(server: Server, init_options, ready_file: str | None) -> None:
    async def run() -> None:
        async with stdio_server() as (read_stream, write_stream):
            write_ready(ready_file, f"READY stdio {os.getpid()}")
            await server.run(read_stream, write_stream, init_options)

    anyio.run(run)


def serve_http(server: Server, args: argparse.Namespace) -> None:
    from mcp.server.streamable_http_manager import StreamableHTTPSessionManager

    manager = StreamableHTTPSessionManager(app=server, json_response=False, stateless=False)
    ready_url = f"http://127.0.0.1:{args.port}{args.path}"
    app = StreamableHttpApp(manager, path=args.path, ready_file=args.ready_file, ready_url=ready_url)

    import uvicorn

    # log_config=None keeps uvicorn's own logging out of stdout; warnings go to stderr.
    # uvicorn handles SIGTERM itself and, after its graceful shutdown, re-raises the
    # captured signal; this no-op handler keeps that re-raise from killing the process,
    # so the server ends with status 0 instead of 143.
    signal.signal(signal.SIGTERM, lambda *_: None)
    uvicorn.run(app, host="127.0.0.1", port=args.port, log_config=None, log_level="warning", lifespan="on")


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    root = resolve_root(args.root)
    server = build_server(root)
    if args.transport == "stdio":
        serve_stdio(server, server.create_initialization_options(), args.ready_file)
    else:
        serve_http(server, args)
    return 0


if __name__ == "__main__":
    sys.exit(main())
