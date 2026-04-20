from __future__ import annotations

from abc import abstractmethod
from dataclasses import dataclass, field
from enum import Enum
from pathlib import Path
from typing import Any, Awaitable, Callable

from pydantic import BaseModel, ConfigDict

from aish.tools.result import ToolResult


class ToolPreflightAction(str, Enum):
    EXECUTE = "execute"
    CONFIRM = "confirm"
    SHORT_CIRCUIT = "short_circuit"


@dataclass
class ToolExecutionContext:
    cwd: Path
    cancellation_token: Any | None = None
    interruption_manager: Any | None = None
    is_approved: Callable[[str], bool] | None = None


@dataclass
class ToolPanelSpec:
    mode: str = "confirm"
    target: str | None = None
    preview: str | None = None
    analysis: dict[str, Any] = field(default_factory=dict)
    allow_remember: bool = False
    remember_key: str | None = None
    title: str | None = None

    def to_event_payload(self) -> dict[str, Any]:
        payload: dict[str, Any] = {
            "mode": self.mode,
            "allow_remember": self.allow_remember,
        }
        if self.target is not None:
            payload["target"] = self.target
        if self.preview is not None:
            payload["preview"] = self.preview
        if self.analysis:
            payload["analysis"] = self.analysis
        if self.remember_key is not None:
            payload["remember_key"] = self.remember_key
        if self.title is not None:
            payload["title"] = self.title
        return payload


@dataclass
class ToolPreflightResult:
    action: ToolPreflightAction = ToolPreflightAction.EXECUTE
    panel: ToolPanelSpec | None = None
    result: ToolResult | None = None


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

    def get_pre_execute_subject(self, tool_args: dict[str, Any]) -> Any:
        """Legacy adapter hook for tools that only inspect part of tool_args."""
        return tool_args

    def prepare_invocation(
        self, tool_args: dict[str, Any], context: ToolExecutionContext
    ) -> ToolPreflightResult:
        """Prepare a tool invocation before execution.

        New tools should override this method directly. The default implementation
        adapts the legacy confirmation hooks for backward compatibility.
        """

        _ = context
        subject = self.get_pre_execute_subject(tool_args)
        need_confirm = self.need_confirm_before_exec(subject)
        if not need_confirm:
            return ToolPreflightResult(action=ToolPreflightAction.EXECUTE)

        info = self.get_confirmation_info(subject)
        return ToolPreflightResult(
            action=ToolPreflightAction.CONFIRM,
            panel=self._build_panel_from_legacy(tool_args, info),
        )

    def get_session_output(self, result: ToolResult) -> str | None:
        """Optionally expose a tool result as the session's fallback output."""
        return None

    def should_stop_after_session_output(self, result: ToolResult) -> bool:
        """Whether exposing session output should also end the current turn."""
        _ = result
        return False

    def _build_panel_from_legacy(
        self, tool_args: dict[str, Any], info: object
    ) -> ToolPanelSpec:
        info_dict = info if isinstance(info, dict) else {}
        target: str | None = None
        preview: str | None = None
        analysis: dict[str, Any] = {}
        remember_key: str | None = None
        title: str | None = None

        if isinstance(tool_args, dict):
            raw_target = tool_args.get("file_path") or tool_args.get("path")
            if raw_target is not None:
                target = str(raw_target)

        if isinstance(info_dict, dict):
            raw_target = info_dict.get("target")
            if raw_target is not None:
                target = str(raw_target)

            raw_preview = info_dict.get("preview")
            if raw_preview is None:
                raw_preview = info_dict.get("content_preview")
            if raw_preview is not None:
                preview = str(raw_preview)

            raw_analysis = info_dict.get("analysis")
            if isinstance(raw_analysis, dict):
                analysis = raw_analysis
            elif isinstance(info_dict.get("security_analysis"), dict):
                analysis = info_dict["security_analysis"]

            raw_remember_key = info_dict.get("remember_key")
            if raw_remember_key is None:
                raw_remember_key = info_dict.get("command")
            if raw_remember_key is not None:
                remember_key = str(raw_remember_key)

            raw_title = info_dict.get("title")
            if raw_title is not None:
                title = str(raw_title)

        mode = str(info_dict.get("panel_mode", "confirm"))
        allow_remember = bool(info_dict.get("allow_remember", False))

        return ToolPanelSpec(
            mode=mode,
            target=target,
            preview=preview,
            analysis=analysis,
            allow_remember=allow_remember,
            remember_key=remember_key,
            title=title,
        )

    @abstractmethod
    def __call__(
        self, *args: Any, **kwargs: Any
    ) -> ToolResult | str | Awaitable[ToolResult] | Awaitable[str]:
        """Can return ToolResult or str (sync/async)."""
        raise NotImplementedError
