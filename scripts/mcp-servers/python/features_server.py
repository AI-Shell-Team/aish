#!/usr/bin/env python3
"""MCP fixture server with the shared tool set plus client-facing features.

On top of the shared tools it adds:
  * `list_added`      - registers a new tool and emits a real
                        `notifications/tools/list_changed` after responding.
  * `ask_client`      - sends a real `roots/list` request and a real
                        `sampling/createMessage` request to the client and writes
                        an outcome envelope per request (`{"result": ...}` or
                        `{"error": {code, message}}`) to `ask-client.json`.

It advertises tools + resources + prompts capabilities, backed by one resource
(`fixture://note`) and one prompt (`fixture_prompt`).

Transports:
    python features_server.py --transport stdio [--ready-file PATH]
    python features_server.py --transport http --port N [--path /mcp]
"""

from __future__ import annotations

import argparse
import contextlib
import functools
import json
import os
import signal
import sys
import tempfile
from pathlib import Path
from typing import Any

import anyio
import mcp.types as types
from mcp.server.lowlevel import NotificationOptions, Server
from mcp.server.stdio import stdio_server
from mcp.shared.message import SessionMessage

SERVER_NAME = "aish-fixture-features"
SERVER_VERSION = "1.0.0"

TOOLS_LIST_CHANGED = "notifications/tools/list_changed"

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
    schema: dict[str, Any] = {
        "type": "object",
        "properties": properties or {},
        "additionalProperties": False,
    }
    if required:
        schema["required"] = required
    return schema


def shared_tools() -> list[types.Tool]:
    """The shared tool contract, identical to tools_server.py and the Node fixtures."""
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
            inputSchema=_object_schema({"a": {"type": "number"}, "b": {"type": "number"}}, ["a", "b"]),
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


def feature_tools() -> list[types.Tool]:
    return [
        types.Tool(
            name="list_added",
            description="Register a new tool, then emit notifications/tools/list_changed after responding.",
            inputSchema=_object_schema(),
        ),
        types.Tool(
            name="ask_client",
            description="Send roots/list and sampling/createMessage to the client and report what came back.",
            inputSchema=_object_schema(),
        ),
    ]


ADDED_TOOL = types.Tool(
    name="added_after_change",
    description="Tool registered at runtime by list_added.",
    inputSchema=_object_schema(),
    annotations=READ_ONLY_IDEMPOTENT,
)


def text_result(text: str) -> types.CallToolResult:
    return types.CallToolResult(content=[types.TextContent(type="text", text=text)])


def error_result(text: str) -> types.CallToolResult:
    return types.CallToolResult(content=[types.TextContent(type="text", text=text)], isError=True)


def resolve_under_root(root: Path, raw_path: str) -> Path:
    target = (root / raw_path.lstrip("/")).resolve()
    if target != root and root not in target.parents:
        raise ValueError(f"path escapes --root: {raw_path}")
    return target


class Features:
    """State shared by the tool handlers and the transport helpers."""

    def __init__(self, root: Path, transport: str):
        self.root = root
        self.transport = transport
        self.tools: list[types.Tool] = shared_tools() + feature_tools()
        # Armed by list_added.  The stdio write hook clears it and emits the
        # notification right after the tool response; in http mode a helper task
        # does it, because server-to-client notifications travel on the standalone
        # SSE stream instead of on the request's response stream.
        self.list_changed_pending = False
        self.session = None
        self.task_group = None


def _error_payload(exc: BaseException) -> dict[str, Any]:
    """Failure envelope for ask-client.json: {"error": {code, message}}."""
    err = getattr(exc, "error", None)
    if isinstance(err, types.ErrorData):
        return {"error": {"code": err.code, "message": err.message}}
    return {"error": {"code": None, "message": str(exc)}}


def _result_payload(model: Any) -> dict[str, Any]:
    """Success envelope for ask-client.json: {"result": <raw result>}."""
    return {"result": model.model_dump(mode="json", by_alias=True, exclude_none=True)}


async def _ask_client(state: Features) -> types.CallToolResult:
    """Do one real roots/list and one real sampling/createMessage round-trip."""
    session = state.session
    if session is None:  # pragma: no cover - a call always runs inside a request context
        return error_result("no active session")

    try:
        roots: Any = _result_payload(await session.list_roots())
    except Exception as exc:
        roots = _error_payload(exc)

    try:
        sampling: Any = _result_payload(
            await session.create_message(
                messages=[
                    types.SamplingMessage(
                        role="user", content=types.TextContent(type="text", text="fixture sampling request")
                    )
                ],
                max_tokens=16,
            )
        )
    except Exception as exc:
        sampling = _error_payload(exc)

    # Envelope format shared with the Node fixture servers:
    #   {"roots": {"result": <raw result>} | {"error": {code, message}}, "sampling": <same>}
    payload = {"roots": roots, "sampling": sampling}
    (state.root / "ask-client.json").write_text(json.dumps(payload, indent=2, ensure_ascii=False), encoding="utf-8")

    def describe(value: Any) -> str:
        if "error" in value:
            return f"error {value['error']['code']}"
        return "ok"

    return text_result(f"client answers: roots={describe(roots)}, sampling={describe(sampling)}")


async def handle_tool(state: Features, name: str, arguments: dict[str, Any]) -> types.CallToolResult:
    root = state.root
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
        case "list_added":
            if not any(tool.name == ADDED_TOOL.name for tool in state.tools):
                state.tools.append(ADDED_TOOL)
            state.list_changed_pending = True
            if state.transport == "http" and state.task_group is not None and state.session is not None:
                state.task_group.start_soon(_notify_list_changed_soon, state)
            return text_result("added")
        case "added_after_change":
            return text_result("added after change")
        case "ask_client":
            return await _ask_client(state)
        case _:
            return error_result(f"unknown tool: {name}")


async def _notify_list_changed_soon(state: Features) -> None:
    """http mode: let the tool response leave first, then announce the change."""
    await anyio.sleep(0.05)
    state.list_changed_pending = False
    if state.session is not None:
        await state.session.send_tool_list_changed()


# --------------------------------------------------------------------------- #
# server and transports
# --------------------------------------------------------------------------- #


def build_server(state: Features) -> Server:
    server: Server = Server(
        SERVER_NAME,
        version=SERVER_VERSION,
        instructions="aish MCP fixture server: tools + resources + prompts + list_changed.",
    )

    @server.list_tools()
    async def list_tools() -> list[types.Tool]:
        return state.tools

    @server.call_tool()
    async def call_tool(name: str, arguments: dict[str, Any]) -> types.CallToolResult:
        state.session = server.request_context.session
        return await handle_tool(state, name, arguments)

    @server.list_resources()
    async def list_resources() -> list[types.Resource]:
        return [
            types.Resource(
                uri="fixture://note",
                name="fixture-note",
                description="Static text resource served by the aish fixture.",
                mimeType="text/plain",
            )
        ]

    @server.read_resource()
    async def read_resource(uri) -> str:
        return f"fixture resource note served from {state.root}"

    @server.list_prompts()
    async def list_prompts() -> list[types.Prompt]:
        return [
            types.Prompt(
                name="fixture_prompt",
                description="Static prompt served by the aish fixture.",
                arguments=[types.PromptArgument(name="topic", description="optional topic", required=False)],
            )
        ]

    @server.get_prompt()
    async def get_prompt(name: str, arguments: dict[str, str] | None) -> types.GetPromptResult:
        topic = (arguments or {}).get("topic", "nothing")
        return types.GetPromptResult(
            description="fixture prompt",
            messages=[
                types.PromptMessage(
                    role="user", content=types.TextContent(type="text", text=f"fixture prompt about {topic}")
                )
            ],
        )

    return server


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


async def send_json(send, status: int, payload: dict[str, Any]):
    body = json.dumps(payload).encode("utf-8")
    headers = [(b"content-type", b"application/json"), (b"content-length", str(len(body)).encode())]
    await send({"type": "http.response.start", "status": status, "headers": headers})
    await send({"type": "http.response.body", "body": body})


class StreamableHttpApp:
    """ASGI wrapper: path guard, lifespan-managed session manager, ready-file hook."""

    def __init__(self, manager, state: Features, path: str, ready_file: str | None, ready_url: str):
        self.manager = manager
        self.state = state
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
                    # A long-lived task group, so tool handlers can schedule the
                    # post-response list_changed notification.
                    self.state.task_group = await self._stack.enter_async_context(anyio.create_task_group())
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


async def forward_output(watch, write_stream, state: Features) -> None:
    """Forward session output to stdout, emitting list_changed right after a response."""
    try:
        async for session_message in watch:
            await write_stream.send(session_message)
            if state.list_changed_pending and isinstance(session_message.message.root, types.JSONRPCResponse):
                state.list_changed_pending = False
                notification = types.JSONRPCMessage(types.JSONRPCNotification(jsonrpc="2.0", method=TOOLS_LIST_CHANGED))
                await write_stream.send(SessionMessage(notification))
    finally:
        # The session closes the interposed stream (not this one) when stdin ends,
        # and the transport's stdout writer runs until the send side is closed.
        await write_stream.aclose()


def serve_stdio(server: Server, init_options, state: Features, ready_file: str | None) -> None:
    async def run() -> None:
        async with stdio_server() as (read_stream, write_stream):
            write_ready(ready_file, f"READY stdio {os.getpid()}")
            # Interposed stream: the hook above observes every outbound frame, so
            # the list_changed notification is guaranteed to follow the response.
            send_to_session, watch = anyio.create_memory_object_stream(0)
            async with anyio.create_task_group() as tg:
                tg.start_soon(forward_output, watch, write_stream, state)
                try:
                    await server.run(read_stream, send_to_session, init_options)
                finally:
                    tg.cancel_scope.cancel()

    anyio.run(run)


def serve_http(server: Server, args: argparse.Namespace, state: Features) -> None:
    from mcp.server.streamable_http_manager import StreamableHTTPSessionManager

    manager = StreamableHTTPSessionManager(app=server, json_response=False, stateless=False)
    ready_url = f"http://127.0.0.1:{args.port}{args.path}"
    app = StreamableHttpApp(manager, state, path=args.path, ready_file=args.ready_file, ready_url=ready_url)

    import uvicorn

    # uvicorn handles SIGTERM itself and, after its graceful shutdown, re-raises
    # the captured signal.  This no-op handler keeps that re-raise from killing
    # the process, so the server ends with status 0 instead of 143.
    signal.signal(signal.SIGTERM, lambda *_: None)
    uvicorn.run(app, host="127.0.0.1", port=args.port, log_config=None, log_level="warning", lifespan="on")


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    state = Features(resolve_root(args.root), args.transport)
    server = build_server(state)
    # The HTTP session manager builds the initialization options from the server
    # instance itself, so bind `tools.listChanged` to the instance: both transports
    # then advertise the same capabilities.
    server.create_initialization_options = functools.partial(  # type: ignore[method-assign]
        Server.create_initialization_options, server, NotificationOptions(tools_changed=True)
    )
    init_options = server.create_initialization_options()
    if args.transport == "stdio":
        serve_stdio(server, init_options, state, args.ready_file)
    else:
        serve_http(server, args, state)
    return 0


if __name__ == "__main__":
    sys.exit(main())
