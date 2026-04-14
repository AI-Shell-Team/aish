from __future__ import annotations

from typing import TYPE_CHECKING, Any

from aish.tools.base import ToolBase
from aish.tools.result import ToolResult

if TYPE_CHECKING:
    from aish.memory.manager import MemoryManager


class MemoryTool(ToolBase):
    """LLM tool for explicit memory management."""

    def __init__(self, memory_manager: "MemoryManager") -> None:
        super().__init__(
            name="memory",
            description=(
                "Search, store, or manage long-term memories. "
                "Use 'search' to find relevant past knowledge, "
                "'store' to explicitly save important information, "
                "'list' to see recent memories, "
                "'forget' to remove outdated info."
            ),
            parameters={
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["search", "store", "forget", "list"],
                        "description": "Memory operation to perform",
                    },
                    "query": {
                        "type": "string",
                        "description": "Search query (for 'search' action)",
                    },
                    "content": {
                        "type": "string",
                        "description": "Content to store (for 'store' action)",
                    },
                    "category": {
                        "type": "string",
                        "enum": [
                            "preference",
                            "environment",
                            "solution",
                            "pattern",
                            "other",
                        ],
                        "description": "Category for stored memory (default: other)",
                    },
                    "memory_id": {
                        "type": "integer",
                        "description": "Memory ID to forget (for 'forget' action)",
                    },
                },
                "required": ["action"],
            },
        )
        self.memory_manager = memory_manager
        self._allowed_actions: set[str] | None = None

    def set_allowed_actions(self, actions: set[str] | None) -> None:
        self._allowed_actions = None if actions is None else set(actions)

    def __call__(
        self,
        action: str,
        query: str | None = None,
        content: str | None = None,
        category: str | None = None,
        memory_id: int | None = None,
        **kwargs: Any,
    ) -> ToolResult:
        if self._allowed_actions is not None and action not in self._allowed_actions:
            return ToolResult(
                ok=False,
                output=(
                    f"Error: memory action '{action}' is not available in the current session phase"
                ),
                meta={"kind": "plan_mode_blocked", "action": action},
                stop_tool_chain=True,
            )

        if action == "search":
            if not query:
                return ToolResult(
                    ok=False, output="Error: query is required for search"
                )
            results = self.memory_manager.recall(query)
            if not results:
                return ToolResult(ok=True, output="No matching memories found.")
            lines = []
            for r in results:
                lines.append(f"  [{r.category.value}] {r.content} (id={r.id})")
            return ToolResult(ok=True, output="Found memories:\n" + "\n".join(lines))

        elif action == "store":
            if not content:
                return ToolResult(
                    ok=False, output="Error: content is required for store"
                )
            from aish.memory.models import MemoryCategory

            cat = MemoryCategory(category) if category else MemoryCategory.OTHER
            entry_id = self.memory_manager.store(
                content=content,
                category=cat,
                source="explicit",
                importance=0.8,
            )
            return ToolResult(ok=True, output=f"Stored as memory #{entry_id}.")

        elif action == "forget":
            if memory_id is None:
                return ToolResult(
                    ok=False, output="Error: memory_id is required for forget"
                )
            self.memory_manager.delete(memory_id)
            return ToolResult(ok=True, output=f"Forgot memory #{memory_id}.")

        elif action == "list":
            entries = self.memory_manager.list_recent(limit=10)
            if not entries:
                return ToolResult(ok=True, output="No memories yet.")
            lines = []
            for e in entries:
                lines.append(f"  #{e.id} [{e.category.value}] {e.content}")
            return ToolResult(
                ok=True, output="Recent memories:\n" + "\n".join(lines)
            )

        else:
            return ToolResult(
                ok=False,
                output=f"Unknown action: {action}. Use search/store/forget/list.",
            )
