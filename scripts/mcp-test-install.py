#!/usr/bin/env python3
"""Write the test MCP servers into the user's aish config (used by the script).

Kept as a separate file so the shell wrapper stays readable: it reads the
`servers:` snippet this script's caller generated, parses it, and replaces only
`mcp.servers` (plus `mcp.enabled`/`mcp.request_timeout_ms` defaults) in the
target config.
"""

import os
import sys

import yaml


def main() -> int:
    body = os.environ["AISH_MCP_BODY"]
    target = os.environ["AISH_MCP_TARGET"]
    # Indent the snippet so it parses as the value of a top-level `mcp:` key.
    snippet = "\n".join("  " + line if line.strip() else line for line in body.splitlines())
    doc = yaml.safe_load("mcp:\n" + snippet + "\n")
    servers = doc["mcp"]["servers"]

    with open(target, encoding="utf-8") as handle:
        config = yaml.safe_load(handle) or {}
    config.setdefault("mcp", {})
    config["mcp"]["servers"] = servers
    config["mcp"].setdefault("enabled", True)
    config["mcp"].setdefault("request_timeout_ms", 60000)
    with open(target, "w", encoding="utf-8") as handle:
        yaml.safe_dump(config, handle, sort_keys=False, allow_unicode=True)
    print(f"installed {len(servers)} servers into {target}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
