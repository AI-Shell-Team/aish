from __future__ import annotations

import uuid

from aish.i18n import t
from aish.terminal.interaction.models import (
    InteractionAnswerType,
    InteractionCustomConfig,
    InteractionKind,
    InteractionOption,
    InteractionRequest,
    InteractionResponse,
    InteractionSource,
    InteractionStatus,
)
from aish.tools.result import ToolResult


class PlanApprovalRequestBuilder:
    @staticmethod
    def from_payload(
        *,
        prompt: str,
        summary: str = "",
        artifact_path: str = "",
        artifact_preview: str = "",
        source_name: str = "exit_plan_mode",
        interaction_id: str | None = None,
    ) -> InteractionRequest:
        prompt_lines = [prompt.strip() or "Review this plan."]
        if summary.strip():
            prompt_lines.extend(["", "Summary:", summary.strip()])
        if artifact_preview.strip():
            prompt_lines.extend(["", "Plan:", artifact_preview.strip()])

        metadata: dict[str, object] = {}
        if prompt.strip():
            metadata["prompt"] = prompt.strip()
        if artifact_path.strip():
            metadata["artifact_path"] = artifact_path.strip()
        if summary.strip():
            metadata["summary"] = summary.strip()
        if artifact_preview.strip():
            metadata["artifact_preview"] = artifact_preview.strip()

        return InteractionRequest(
            id=interaction_id or f"interaction_{uuid.uuid4().hex[:12]}",
            kind=InteractionKind.PLAN_APPROVAL,
            title=t("plan.approval.title"),
            prompt="\n".join(prompt_lines),
            required=True,
            allow_cancel=True,
            source=InteractionSource(type="tool", name=source_name),
            metadata=metadata,
            options=[
                InteractionOption(
                    value="approve",
                    label=t("plan.approval.option_approve"),
                    description=t("plan.approval.option_approve_desc"),
                ),
                InteractionOption(
                    value="changes_requested",
                    label=t("plan.approval.option_changes"),
                    description=t("plan.approval.option_changes_desc"),
                ),
            ],
            default="approve",
            custom=InteractionCustomConfig(
                label=t("plan.approval.custom_label"),
                placeholder=t("plan.approval.custom_placeholder"),
                submit_mode="inline",
            ),
        )


class PlanApprovalInteractionAdapter:
    @staticmethod
    def to_tool_result(
        request: InteractionRequest,
        response: InteractionResponse,
    ) -> ToolResult:
        if response.status == InteractionStatus.SUBMITTED and response.answer is not None:
            if response.answer.type == InteractionAnswerType.OPTION:
                decision = response.answer.value.strip().lower()
                label = response.answer.label or response.answer.value
                if decision == "approve":
                    return ToolResult(
                        ok=True,
                        output="Plan approved by user.",
                        data={
                            "decision": "approve",
                            "value": response.answer.value,
                            "label": label,
                            "interaction_id": response.interaction_id,
                            "answer_type": response.answer.type.value,
                        },
                        meta={
                            "interaction_id": response.interaction_id,
                            "interaction_status": response.status.value,
                        },
                    )
                return ToolResult(
                    ok=True,
                    output="Plan changes requested by user.",
                    data={
                        "decision": "changes_requested",
                        "value": response.answer.value,
                        "label": label,
                        "interaction_id": response.interaction_id,
                        "answer_type": response.answer.type.value,
                    },
                    meta={
                        "interaction_id": response.interaction_id,
                        "interaction_status": response.status.value,
                    },
                    stop_tool_chain=True,
                )

            if response.answer.type == InteractionAnswerType.TEXT:
                feedback = response.answer.value.strip()
                return ToolResult(
                    ok=True,
                    output="Plan changes requested with feedback.",
                    data={
                        "decision": "changes_requested",
                        "feedback": feedback,
                        "value": feedback,
                        "label": response.answer.label or feedback,
                        "interaction_id": response.interaction_id,
                        "answer_type": response.answer.type.value,
                    },
                    meta={
                        "interaction_id": response.interaction_id,
                        "interaction_status": response.status.value,
                    },
                    stop_tool_chain=True,
                )

        artifact_path = str(request.metadata.get("artifact_path") or "").strip()
        pause_lines = [
            "Plan approval is still required.",
            f"Prompt: {request.prompt}",
        ]
        if artifact_path:
            pause_lines.append(f"Plan artifact: {artifact_path}")
        pause_lines.append("Review the plan and either approve it or request changes.")
        return ToolResult(
            ok=False,
            output="\n".join(pause_lines),
            meta={
                "kind": "plan_approval_required",
                "reason": response.reason or response.status.value,
                "interaction_id": response.interaction_id,
                "interaction_status": response.status.value,
                "artifact_path": artifact_path or None,
            },
            stop_tool_chain=True,
        )