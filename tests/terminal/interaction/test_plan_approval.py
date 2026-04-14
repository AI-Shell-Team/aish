from __future__ import annotations

import pytest

from aish.plan.approval import (
    PlanApprovalInteractionAdapter,
    PlanApprovalRequestBuilder,
)
from aish.terminal.interaction import (
    InteractionAnswer,
    InteractionAnswerType,
    InteractionKind,
    InteractionResponse,
    InteractionStatus,
)


pytestmark = pytest.mark.timeout(5)


def test_plan_approval_request_builder_builds_typed_request():
    request = PlanApprovalRequestBuilder.from_payload(
        prompt="Review this plan.",
        summary="Add comments to cli.py",
        artifact_path="/tmp/plan.md",
        artifact_preview="# Plan\n\nStep 1",
    )

    assert request.kind == InteractionKind.PLAN_APPROVAL
    assert request.source.name == "exit_plan_mode"
    assert request.default == "approve"
    assert request.custom is not None
    assert request.metadata["prompt"] == "Review this plan."
    assert request.metadata["artifact_path"] == "/tmp/plan.md"
    assert request.metadata["artifact_preview"] == "# Plan\n\nStep 1"
    assert request.options[0].value == "approve"
    assert "Summary:" in request.prompt
    assert "Plan:" in request.prompt


def test_plan_approval_adapter_maps_approve_to_typed_result():
    request = PlanApprovalRequestBuilder.from_payload(prompt="Review this plan.")
    response = InteractionResponse(
        interaction_id=request.id,
        status=InteractionStatus.SUBMITTED,
        answer=InteractionAnswer(
            type=InteractionAnswerType.OPTION,
            value="approve",
            label="Approve",
        ),
    )

    result = PlanApprovalInteractionAdapter.to_tool_result(request, response)

    assert result.ok is True
    assert result.data["decision"] == "approve"
    assert result.meta["interaction_status"] == "submitted"


def test_plan_approval_adapter_maps_custom_text_to_changes_requested():
    request = PlanApprovalRequestBuilder.from_payload(prompt="Review this plan.")
    response = InteractionResponse(
        interaction_id=request.id,
        status=InteractionStatus.SUBMITTED,
        answer=InteractionAnswer(
            type=InteractionAnswerType.TEXT,
            value="Split step 2 into two steps",
        ),
    )

    result = PlanApprovalInteractionAdapter.to_tool_result(request, response)

    assert result.ok is True
    assert result.data["decision"] == "changes_requested"
    assert result.data["feedback"] == "Split step 2 into two steps"
    assert result.stop_tool_chain is True


def test_plan_approval_adapter_pauses_when_cancelled():
    request = PlanApprovalRequestBuilder.from_payload(
        prompt="Review this plan.",
        artifact_path="/tmp/plan.md",
    )
    response = InteractionResponse(
        interaction_id=request.id,
        status=InteractionStatus.CANCELLED,
        reason="cancelled",
    )

    result = PlanApprovalInteractionAdapter.to_tool_result(request, response)

    assert result.ok is False
    assert result.meta["kind"] == "plan_approval_required"
    assert "/tmp/plan.md" in result.output