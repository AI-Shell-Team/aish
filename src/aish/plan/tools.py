from __future__ import annotations

from collections.abc import Callable

from aish.plan.state import (
    PlanModeState,
    PlanPhase,
    read_artifact_text,
)
from aish.tools.base import ToolBase
from aish.tools.result import ToolResult


class ExitPlanModeTool(ToolBase):
    def __init__(
        self,
        *,
        get_plan_state: Callable[[], PlanModeState],
        update_plan_state: Callable[[PlanModeState], PlanModeState],
    ) -> None:
        super().__init__(
            name="exit_plan_mode",
            description=(
                "Signal that the current plan file is ready for user review. "
                "Use this after updating the bound plan.md in planning mode."
            ),
            parameters={
                "type": "object",
                "properties": {
                    "summary": {
                        "type": "string",
                        "description": "Optional short summary of the completed plan.",
                    },
                },
                "required": [],
            },
        )
        self._get_plan_state = get_plan_state
        self._update_plan_state = update_plan_state

    def __call__(self, summary: str | None = None) -> ToolResult:
        plan_state = self._get_plan_state()
        if plan_state.phase != PlanPhase.PLANNING.value:
            return ToolResult(
                ok=False,
                output="Error: exit_plan_mode can only be used while planning",
            )

        artifact_path = plan_state.artifact
        if artifact_path is None:
            return ToolResult(
                ok=False,
                output="Error: plan artifact is not initialized",
            )

        artifact_text = read_artifact_text(artifact_path)
        next_state = self._update_plan_state(
            plan_state.with_updates(summary=summary or plan_state.summary)
        )
        return ToolResult(
            ok=True,
            output="",
            data={
                "signal": "exit_plan_mode",
                "phase": next_state.phase,
                "artifact_path": str(artifact_path),
                "artifact_preview": artifact_text,
                "summary": next_state.summary or "",
            },
            stop_tool_chain=True,
        )
