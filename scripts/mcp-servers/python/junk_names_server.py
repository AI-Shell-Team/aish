#!/usr/bin/env python3
"""MCP fixture server serving tool names a client must reject.

Besides a valid `echo` it lists three names that are invalid for a client:
`*`, `bad\x01name` (a control character), and a 200-character name.  The Python
SDK accepts them on the wire (it only logs a warning), so `tools/list` really
carries them unmodified - which is what makes this fixture useful for checking
that the aish client filters or rejects them instead of choking.

    python junk_names_server.py --transport stdio [--ready-file PATH] [--root DIR]
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

SERVER_NAME = "aish-fixture-junk-names"
SERVER_VERSION = "1.0.0"

STAR_NAME = "*"
CONTROL_CHAR_NAME = "bad\x01name"
LONG_NAME = "j" * 200


def _object_schema() -> dict[str, Any]:
    return {"type": "object", "properties": {}, "additionalProperties": False}


def build_server() -> Server:
    server: Server = Server(
        SERVER_NAME,
        version=SERVER_VERSION,
        instructions="aish MCP fixture server: invalid tool names on the wire.",
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
            ),
            types.Tool(name=STAR_NAME, description="Invalid name: bare asterisk.", inputSchema=_object_schema()),
            types.Tool(
                name=CONTROL_CHAR_NAME,
                description="Invalid name: contains a control character.",
                inputSchema=_object_schema(),
            ),
            types.Tool(
                name=LONG_NAME,
                description="Invalid name: 200 characters long.",
                inputSchema=_object_schema(),
            ),
        ]

    @server.call_tool()
    async def call_tool(name: str, arguments: dict[str, Any]) -> types.CallToolResult:
        if name == "echo":
            return types.CallToolResult(content=[types.TextContent(type="text", text=arguments["text"])])
        if name in (STAR_NAME, CONTROL_CHAR_NAME, LONG_NAME):
            return types.CallToolResult(
                content=[types.TextContent(type="text", text=f"called invalid-name tool: {name!r}")]
            )
        return types.CallToolResult(
            content=[types.TextContent(type="text", text=f"unknown tool: {name}")], isError=True
        )

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


def serve_stdio(server: Server, init_options, ready_file: str | None) -> None:
    async def run() -> None:
        async with stdio_server() as (read_stream, write_stream):
            write_ready(ready_file, f"READY stdio {os.getpid()}")
            await server.run(read_stream, write_stream, init_options)

    anyio.run(run)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    # Accepted for CLI uniformity; this server keeps no state files.
    resolve_root(args.root)
    server = build_server()
    serve_stdio(server, server.create_initialization_options(), args.ready_file)
    return 0


if __name__ == "__main__":
    sys.exit(main())
