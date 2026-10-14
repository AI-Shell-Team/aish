#!/usr/bin/env python3
"""MCP fixture server that dies right after its first `initialize`.

On the first run (no `crash-once.marker` in --root) it answers `initialize` and
then exits with status 1, so a client reconnect ladder has to notice the failure
and retry.  The marker is created before serving starts, so every later run
serves normally, which lets a retry succeed.

    python crash_once_server.py --transport stdio [--ready-file PATH] [--root DIR]
"""

from __future__ import annotations

import argparse
import os
import sys
import tempfile
from pathlib import Path
from typing import Any

import anyio
import mcp.types as types
from mcp.server.lowlevel import Server
from mcp.server.stdio import stdio_server

SERVER_NAME = "aish-fixture-crash-once"
SERVER_VERSION = "1.0.0"
MARKER_NAME = "crash-once.marker"


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
        instructions="aish MCP fixture server: crashes after the first initialize.",
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
    parser.add_argument("--transport", choices=["stdio"], default="stdio")
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


async def crash_after_first_response(watch, write_stream, first_run: bool) -> None:
    """Forward session output, but on the first run kill the process right after
    the initialize response has been written to stdout."""
    try:
        async for session_message in watch:
            if first_run and isinstance(session_message.message.root, types.JSONRPCResponse):
                # Write the frame ourselves and flush, so the client really sees
                # the initialize response before the connection dies.
                payload = session_message.message.model_dump_json(by_alias=True, exclude_none=True)
                sys.stdout.buffer.write(payload.encode("utf-8") + b"\n")
                sys.stdout.buffer.flush()
                os._exit(1)
            await write_stream.send(session_message)
    finally:
        await write_stream.aclose()


def serve_stdio(server: Server, init_options, ready_file: str | None, first_run: bool) -> None:
    async def run() -> None:
        async with stdio_server() as (read_stream, write_stream):
            write_ready(ready_file, f"READY stdio {os.getpid()}")
            if not first_run:
                await server.run(read_stream, write_stream, init_options)
                return
            send_to_session, watch = anyio.create_memory_object_stream(0)
            async with anyio.create_task_group() as tg:
                tg.start_soon(crash_after_first_response, watch, write_stream, first_run)
                try:
                    await server.run(read_stream, send_to_session, init_options)
                finally:
                    tg.cancel_scope.cancel()

    anyio.run(run)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    root = resolve_root(args.root)
    marker = root / MARKER_NAME
    first_run = not marker.exists()
    if first_run:
        # Claim the first run before serving, so a reconnect always finds a
        # marker and gets a working server.
        marker.write_text("crashed\n", encoding="utf-8")
    server = build_server()
    serve_stdio(server, server.create_initialization_options(), args.ready_file, first_run)
    return 0


if __name__ == "__main__":
    sys.exit(main())
