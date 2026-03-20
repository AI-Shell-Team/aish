from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Optional


@dataclass
class ToolResult:
    ok: bool
    output: str = ""
    code: Optional[int] = None
    meta: dict[str, Any] = field(default_factory=dict)
    data: dict[str, Any] = field(default_factory=dict)

    context_messages: list[dict[str, str]] = field(default_factory=list)
    """Additional messages to inject into the conversation context after the
    tool response message.  Each entry should be a dict with at least
    ``role`` and ``content`` keys (e.g. ``{"role": "user", "content": "..."}``).
    LLMSession processes these generically for *all* tools."""

    stop_tool_chain: bool = False
    """When ``True`` the tool execution loop should stop processing any
    remaining tool calls in the current turn.  This replaces per-tool-name
    checks such as ``security_blocked`` or ``user_input_required``."""

    def render_for_llm(self) -> str:
        return self.output
