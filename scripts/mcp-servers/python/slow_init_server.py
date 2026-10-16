#!/usr/bin/env python3
"""MCP fixture server that answers `initialize` only after a configurable delay.

The delay is applied to the very first bytes the server reads from stdin: the
process starts, appends its READY line, sleeps, and only then begins serving, so
the client's `initialize` request sits unanswered in the pipe for
`--init-delay-ms` milliseconds.  Used to check that the aish client either
honours its initialization timeout or waits as configured.

    python slow_init_server.py --transport stdio [--init-delay-ms 5000] [--ready-file PATH]
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

SERVER_NAME = "aish-fixture-slow-init"
SERVER_VERSION = "1.0.0"


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
        instructions="aish MCP fixture server: slow initialize.",
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
    parser.add_argument("--init-delay-ms", type=int, default=5000)
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


def serve_stdio(server: Server, init_options, ready_file: str | None, delay_ms: int) -> None:
    async def run() -> None:
        async with stdio_server() as (read_stream, write_stream):
            write_ready(ready_file, f"READY stdio {os.getpid()}")
            if delay_ms > 0:
                await anyio.sleep(delay_ms / 1000.0)
            await server.run(read_stream, write_stream, init_options)

    anyio.run(run)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    # Accepted for CLI uniformity; this server keeps no state files.
    resolve_root(args.root)
    server = build_server()
    serve_stdio(server, server.create_initialization_options(), args.ready_file, args.init_delay_ms)
    return 0


if __name__ == "__main__":
    sys.exit(main())
