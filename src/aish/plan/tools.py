from __future__ import annotations

from collections.abc import Callable

from aish.i18n import t
from aish.plan.approval import (
    PlanApprovalInteractionAdapter,
    PlanApprovalRequestBuilder,
)
from aish.plan.state import (
    PlanApprovalStatus,
    PlanModeState,
    PlanPhase,
    compute_artifact_hash,
    create_approved_snapshot,
    read_artifact_text,
)
from aish.terminal.interaction.models import InteractionRequest, InteractionResponse
from aish.tools.base import ToolBase
from aish.tools.result import ToolResult


class EnterPlanModeTool(ToolBase):
    def __init__(
        self,
        *,
        get_plan_state: Callable[[], PlanModeState],
        begin_new_plan: Callable[[], PlanModeState],
    ) -> None:
        super().__init__(
            name="enter_plan_mode",
            description=(
                "Enter plan mode for a non-trivial task that needs research and design "
                "before implementation. Use this to create a new bound plan file and "
                "switch into planning."
            ),
            parameters={
                "type": "object",
                "properties": {},
                "required": [],
            },
        )
        self._get_plan_state = get_plan_state
        self._begin_new_plan = begin_new_plan

    def __call__(self) -> ToolResult:
        plan_state = self._get_plan_state()
        if plan_state.phase == PlanPhase.PLANNING.value:
            artifact_path = plan_state.artifact_path or "(unbound)"
            return ToolResult(
                ok=False,
                output=(
                    "Error: enter_plan_mode cannot be used while already planning. "
                    f"Current bound plan file: {artifact_path}"
                ),
            )

        next_state = self._begin_new_plan()
        artifact_path = next_state.artifact_path or "(unbound)"
        return ToolResult(
            ok=True,
            output=(
                "Entered plan mode. Focus on research and writing the bound plan file before implementation.\n"
                f"Bound plan file: {artifact_path}"
            ),
            data={
                "phase": next_state.phase,
                "artifact_path": artifact_path,
                "plan_id": next_state.plan_id,
            },
        )


class ExitPlanModeTool(ToolBase):
    def __init__(
        self,
        *,
        get_plan_state: Callable[[], PlanModeState],
        update_plan_state: Callable[[PlanModeState], PlanModeState],
        request_interaction: Callable[[InteractionRequest], InteractionResponse],
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
        self._request_interaction = request_interaction

    @staticmethod
    def _build_system_reminder(message: str) -> dict[str, str]:
        return {
            "role": "user",
            "content": f"<system-reminder>\n{message}\n</system-reminder>",
        }

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
        summary_text = str(summary or plan_state.summary or "").strip()
        self._update_plan_state(
            plan_state.with_updates(
                approval_status=PlanApprovalStatus.AWAITING_USER.value,
                summary=summary_text or plan_state.summary,
            )
        )
        request = PlanApprovalRequestBuilder.from_payload(
            prompt=t("plan.approval.prompt"),
            summary=summary_text,
            artifact_path=str(artifact_path),
            artifact_preview=artifact_text,
            source_name=self.name,
        )
        response = self._request_interaction(request)
        approval_result = PlanApprovalInteractionAdapter.to_tool_result(request, response)
        result_payload = (
            approval_result.data if isinstance(approval_result.data, dict) else {}
        )
        decision = str(result_payload.get("decision") or "").strip().lower()

        if decision == "approve":
            approved_state, snapshot_path = create_approved_snapshot(self._get_plan_state())
            approved_hash = compute_artifact_hash(snapshot_path)
            next_state = self._update_plan_state(
                approved_state.with_updates(
                    phase=PlanPhase.NORMAL.value,
                    approval_status=PlanApprovalStatus.APPROVED.value,
                    approved_artifact_hash=approved_hash,
                    approval_feedback_summary=None,
                    summary=summary_text or approved_state.summary,
                )
            )
            output_lines = [
                "Plan approved by user. Begin implementation now.",
                f"Approved plan artifact: {snapshot_path}",
            ]
            if next_state.summary:
                output_lines.append(f"Approved summary: {next_state.summary}")
            output_lines.append(
                "Follow the approved plan artifact. Read it directly if you need the full implementation details."
            )
            return ToolResult(
                ok=True,
                output="\n".join(output_lines),
                data={
                    "decision": "approve",
                    "approved_artifact_path": str(snapshot_path),
                    "approved_artifact_hash": approved_hash,
                    "summary": next_state.summary or "",
                    "interaction_id": approval_result.meta.get("interaction_id"),
                },
                meta=dict(approval_result.meta),
                context_messages=[
                    self._build_system_reminder(
                        t("plan.approval.reminder_approved", path=snapshot_path)
                    )
                ],
            )

        if decision == "changes_requested":
            feedback = str(result_payload.get("feedback") or "").strip()
            next_state = self._update_plan_state(
                self._get_plan_state().with_updates(
                    phase=PlanPhase.PLANNING.value,
                    approval_status=PlanApprovalStatus.CHANGES_REQUESTED.value,
                    approval_feedback_summary=feedback or summary_text or None,
                    approved_artifact_path=None,
                    approved_revision=None,
                    approved_artifact_hash=None,
                )
            )
            reminder_lines = [t("plan.approval.reminder_changes")]
            output_lines = [
                "Plan changes requested by user. Remain in planning mode and revise the bound plan file before calling exit_plan_mode again.",
            ]
            if feedback:
                reminder_lines.append(
                    t("plan.approval.reminder_feedback", feedback=feedback)
                )
                output_lines.append(f"Feedback: {feedback}")
            return ToolResult(
                ok=True,
                output="\n".join(output_lines),
                data={
                    "decision": "changes_requested",
                    "feedback": feedback,
                    "summary": next_state.summary or "",
                    "interaction_id": approval_result.meta.get("interaction_id"),
                },
                meta=dict(approval_result.meta),
                context_messages=[
                    self._build_system_reminder("\n".join(reminder_lines))
                ],
                stop_tool_chain=True,
            )

        if response.status.value == "cancelled":
            next_state = self._update_plan_state(
                self._get_plan_state().with_updates(
                    phase=PlanPhase.PLANNING.value,
                    approval_status=PlanApprovalStatus.DRAFT.value,
                    approved_artifact_path=None,
                    approved_revision=None,
                    approved_artifact_hash=None,
                    approval_feedback_summary=None,
                )
            )
            return ToolResult(
                ok=True,
                output="Plan review cancelled. Remain in planning mode.",
                data={
                    "decision": "cancelled",
                    "summary": next_state.summary or "",
                    "interaction_id": approval_result.meta.get("interaction_id"),
                },
                meta=dict(approval_result.meta),
                stop_tool_chain=True,
            )

        return approval_result
