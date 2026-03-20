from __future__ import annotations

from abc import abstractmethod
from typing import Any, Awaitable

from pydantic import BaseModel, ConfigDict

from aish.tools.result import ToolResult


class ToolBase(BaseModel):
    model_config = ConfigDict(extra="allow")

    name: str
    description: str
    parameters: dict

    def to_func_spec(self) -> dict:
        return {
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description,
                "parameters": self.parameters,
            },
        }

    def need_confirm_before_exec(self, *args, **kwargs) -> bool:
        return False

    def get_confirmation_info(self, *args, **kwargs) -> dict:
        """Get additional information for confirmation dialog"""
        return {}

    def get_session_output(self, result: ToolResult) -> str | None:
        """Optionally expose a tool result as the session's fallback output."""
        return None

    @abstractmethod
    def __call__(
        self, *args: Any, **kwargs: Any
    ) -> ToolResult | str | Awaitable[ToolResult] | Awaitable[str]:
        """Can return ToolResult or str (sync/async)."""
        raise NotImplementedError
