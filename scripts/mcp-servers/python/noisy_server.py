#!/usr/bin/env python3
"""MCP fixture server that floods stderr while serving one `echo` tool.

It writes a junk (non-protocol) line and several megabytes of noise to stderr
from a background thread, starting immediately and continuing while the protocol
runs.  If the client does not drain the child's stderr, the pipe fills up and the
server blocks, so a healthy run proves stderr is being drained.  The final
`noise complete` line lets a driver assert the whole flood was consumed.

    python noisy_server.py --transport stdio [--noise-kib 4096] [--ready-file PATH]
"""

from __future__ import annotations

import argparse
import os
import sys
import tempfile
import threading
from pathlib import Path
from typing import Any

import anyio
import mcp.types as types
from mcp.server.lowlevel import Server
from mcp.server.stdio import stdio_server

SERVER_NAME = "aish-fixture-noisy"
SERVER_VERSION = "1.0.0"

BLOCK_BYTES = 64 * 1024
JUNK_LINE = b'not-json: {"this is": not a protocol frame}\n'


def _noise_block() -> bytes:
    """64 KiB of 'noise ...' lines (every line is exactly 64 bytes)."""
    line = b"noise " + b"x" * 57 + b"\n"
    return line * (BLOCK_BYTES // len(line))


def _stderr_writer(total_bytes: int) -> None:
    write = os.write
    write(2, b"noise start\n")
    write(2, JUNK_LINE)
    block = _noise_block()
    written = 0
    while written < total_bytes:
        chunk = block[: min(len(block), total_bytes - written)]
        # Partial writes are possible once the pipe fills up.
        offset = 0
        while offset < len(chunk):
            offset += write(2, chunk[offset:])
        written += len(chunk)
    write(2, b"noise complete\n")


def build_server() -> Server:
    server: Server = Server(
        SERVER_NAME,
        version=SERVER_VERSION,
        instructions="aish MCP fixture server: noisy stderr.",
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
    parser.add_argument("--transport", choices=["stdio"], default="stdio")
    parser.add_argument("--noise-kib", type=int, default=4096, help="kibibytes of stderr noise (default 4096)")
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


def serve_stdio(server: Server, init_options, ready_file: str | None, noise_bytes: int) -> None:
    async def run() -> None:
        # A dedicated thread so a full stderr pipe blocks the writer, never the loop.
        threading.Thread(target=_stderr_writer, args=(noise_bytes,), daemon=True).start()
        async with stdio_server() as (read_stream, write_stream):
            write_ready(ready_file, f"READY stdio {os.getpid()}")
            await server.run(read_stream, write_stream, init_options)

    anyio.run(run)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    # Accepted for CLI uniformity; this server keeps no state files.
    resolve_root(args.root)
    server = build_server()
    serve_stdio(server, server.create_initialization_options(), args.ready_file, max(args.noise_kib, 0) * 1024)
    return 0


if __name__ == "__main__":
    sys.exit(main())
