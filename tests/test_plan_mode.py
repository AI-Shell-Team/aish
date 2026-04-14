from __future__ import annotations

import json
from pathlib import Path
from unittest.mock import Mock

import pytest

from aish.config import ConfigModel
from aish.llm import LLMCallbackResult, LLMSession, ToolDispatchStatus
from aish.plan import (
    PlanApprovalStatus,
    PlanPhase,
    compute_artifact_hash,
    create_approved_snapshot,
    decode_plan_state,
    get_default_plan_directory,
)
from aish.skills import SkillManager
from aish.state import ContextManager, MemoryType, SessionStore
from aish.terminal.interaction import (
    InteractionAnswer,
    InteractionAnswerType,
    InteractionResponse,
    InteractionStatus,
)


pytestmark = pytest.mark.timeout(5)


def test_session_store_update_session_state_persists(tmp_path):
    store = SessionStore(tmp_path / "sessions.db")
    try:
        session = store.create_session(model="test-model", state={"status": "active"})
        updated = store.update_session_state(
            session.session_uuid,
            {"plan_mode": {"phase": "planning", "artifact_path": "/tmp/plan.md"}},
        )
        assert updated is not None
        fetched = store.get_session(session.session_uuid)
        assert fetched is not None
        assert fetched.state["plan_mode"]["phase"] == "planning"
    finally:
        store.close()


def test_plan_mode_hides_side_effect_tools_in_planning(tmp_path):
    session = LLMSession(
        config=ConfigModel(model="test-model", api_key="test-key"),
        skill_manager=SkillManager(),
        session_uuid="session-1",
    )
    _ = tmp_path
    session.begin_new_plan()

    tools = {item["function"]["name"] for item in session._get_tools_spec()}
    assert "read_file" in tools
    assert "glob" in tools
    assert "grep" in tools
    assert "write_file" in tools
    assert "edit_file" in tools
    assert "exit_plan_mode" in tools
    assert "enter_plan_mode" not in tools
    assert "bash_exec" not in tools
    assert "system_diagnose_agent" not in tools


def test_begin_new_plan_creates_distinct_artifacts():
    session = LLMSession(
        config=ConfigModel(model="test-model", api_key="test-key"),
        skill_manager=SkillManager(),
        session_uuid="session-new-plan",
    )

    first = session.begin_new_plan()
    second = session.begin_new_plan()

    assert first.plan_id is not None
    assert second.plan_id is not None
    assert first.plan_id != second.plan_id
    assert first.artifact_path != second.artifact_path
    assert Path(first.artifact_path).exists()
    assert Path(second.artifact_path).exists()
    assert Path(first.artifact_path).name == "plan.md"
    assert Path(second.artifact_path).name == "plan.md"
    assert Path(first.artifact_path).parent == get_default_plan_directory(
        session_uuid=session.session_uuid,
        plan_id=first.plan_id,
    )
    assert Path(second.artifact_path).parent == get_default_plan_directory(
        session_uuid=session.session_uuid,
        plan_id=second.plan_id,
    )


def test_enter_plan_mode_tool_begins_new_plan_from_normal_mode():
    session = LLMSession(
        config=ConfigModel(model="test-model", api_key="test-key"),
        skill_manager=SkillManager(),
        session_uuid="session-enter-plan",
    )

    result = session.enter_plan_mode_tool()

    assert result.ok is True
    assert result.stop_tool_chain is False
    assert result.data["phase"] == PlanPhase.PLANNING.value
    assert result.data["artifact_path"] == session.plan_state.artifact_path
    assert session.plan_state.phase == PlanPhase.PLANNING.value
    assert session.plan_state.plan_id is not None
    assert Path(session.plan_state.artifact_path).exists()
    assert "Bound plan file:" in result.output


def test_enter_plan_mode_tool_rejects_when_already_planning():
    session = LLMSession(
        config=ConfigModel(model="test-model", api_key="test-key"),
        skill_manager=SkillManager(),
        session_uuid="session-enter-plan-existing",
    )
    session.begin_new_plan()

    result = session.enter_plan_mode_tool()

    assert result.ok is False
    assert "already planning" in result.output.lower()


@pytest.mark.anyio
async def test_plan_mode_blocks_write_file_in_planning():
    session = LLMSession(
        config=ConfigModel(model="test-model", api_key="test-key"),
        skill_manager=SkillManager(),
        session_uuid="session-2",
    )
    session.begin_new_plan()

    outcome = await session.pre_execute_tool(
        "write_file",
        {"file_path": "/tmp/demo.txt", "content": "hello"},
    )

    assert outcome.status == ToolDispatchStatus.SHORT_CIRCUIT
    assert outcome.result.meta.get("kind") == "plan_mode_blocked"


@pytest.mark.anyio
async def test_plan_mode_allows_bound_plan_writes_in_planning():
    session = LLMSession(
        config=ConfigModel(model="test-model", api_key="test-key"),
        skill_manager=SkillManager(),
        session_uuid="session-2b",
        event_callback=lambda _event: LLMCallbackResult.APPROVE,
    )
    session.begin_new_plan()

    outcome = await session.pre_execute_tool(
        "write_file",
        {
            "file_path": session.plan_state.artifact_path,
            "content": "# Plan\n\nUpdated",
        },
    )

    assert outcome.status == ToolDispatchStatus.EXECUTED
    assert outcome.result.ok is True
    assert session.plan_state.artifact is not None
    assert (
        session.plan_state.artifact.read_text(encoding="utf-8")
        == "# Plan\n\nUpdated"
    )


@pytest.mark.anyio
async def test_plan_mode_blocks_memory_store_in_planning():
    memory_manager = Mock()
    session = LLMSession(
        config=ConfigModel(model="test-model", api_key="test-key"),
        skill_manager=SkillManager(),
        session_uuid="session-3",
        memory_manager=memory_manager,
    )
    session.begin_new_plan()

    outcome = await session.pre_execute_tool(
        "memory",
        {"action": "store", "content": "secret"},
    )

    assert outcome.status == ToolDispatchStatus.SHORT_CIRCUIT
    assert outcome.result.meta.get("kind") == "plan_mode_blocked"


def test_exit_plan_mode_tool_approves_and_updates_state(tmp_path):
    session = LLMSession(
        config=ConfigModel(model="test-model", api_key="test-key"),
        skill_manager=SkillManager(),
        session_uuid="session-4",
    )
    _ = tmp_path
    session.begin_new_plan()
    artifact = session.plan_state.artifact
    assert artifact is not None
    artifact.write_text("# Plan\n\nApproved", encoding="utf-8")
    session.exit_plan_mode_tool._request_interaction = lambda request: InteractionResponse(
        interaction_id=request.id,
        status=InteractionStatus.SUBMITTED,
        answer=InteractionAnswer(
            type=InteractionAnswerType.OPTION,
            value="approve",
            label="Approve",
        ),
    )

    result = session.exit_plan_mode_tool(summary="ready")

    assert result.ok is True
    assert result.stop_tool_chain is False
    assert result.data["decision"] == "approve"
    assert result.data["approved_artifact_path"] == session.plan_state.approved_artifact_path
    assert result.data["summary"] == "ready"
    assert session.plan_state.phase == PlanPhase.NORMAL.value
    assert session.plan_state.approval_status == PlanApprovalStatus.APPROVED.value
    assert session.plan_state.summary == "ready"
    assert "Approved plan artifact:" in result.output
    assert "# Plan\n\nApproved" not in result.output
    assert result.context_messages


def test_exit_plan_mode_tool_changes_requested_stops_and_preserves_planning():
    session = LLMSession(
        config=ConfigModel(model="test-model", api_key="test-key"),
        skill_manager=SkillManager(),
        session_uuid="session-4b",
    )
    session.begin_new_plan()
    artifact = session.plan_state.artifact
    assert artifact is not None
    artifact.write_text("# Plan\n\nNeeds work", encoding="utf-8")
    session.exit_plan_mode_tool._request_interaction = lambda request: InteractionResponse(
        interaction_id=request.id,
        status=InteractionStatus.SUBMITTED,
        answer=InteractionAnswer(
            type=InteractionAnswerType.TEXT,
            value="Split deployment and validation into separate steps",
        ),
    )

    result = session.exit_plan_mode_tool(summary="ready")

    assert result.ok is True
    assert result.stop_tool_chain is True
    assert result.data["decision"] == "changes_requested"
    assert session.plan_state.phase == PlanPhase.PLANNING.value
    assert session.plan_state.approval_status == PlanApprovalStatus.CHANGES_REQUESTED.value
    assert session.plan_state.approval_feedback_summary == (
        "Split deployment and validation into separate steps"
    )
    assert result.context_messages


def test_exit_plan_mode_tool_cancelled_stops_and_returns_to_draft_planning():
    session = LLMSession(
        config=ConfigModel(model="test-model", api_key="test-key"),
        skill_manager=SkillManager(),
        session_uuid="session-4c",
    )
    session.begin_new_plan()
    artifact = session.plan_state.artifact
    assert artifact is not None
    artifact.write_text("# Plan\n\nCancelled", encoding="utf-8")
    session.exit_plan_mode_tool._request_interaction = lambda request: InteractionResponse(
        interaction_id=request.id,
        status=InteractionStatus.CANCELLED,
        reason="cancelled",
    )

    result = session.exit_plan_mode_tool(summary="ready")

    assert result.ok is True
    assert result.stop_tool_chain is True
    assert result.data["decision"] == "cancelled"
    assert session.plan_state.phase == PlanPhase.PLANNING.value
    assert session.plan_state.approval_status == PlanApprovalStatus.DRAFT.value


@pytest.mark.anyio
async def test_exit_plan_mode_approval_keeps_tool_chain_active():
    session = LLMSession(
        config=ConfigModel(model="test-model", api_key="test-key"),
        skill_manager=SkillManager(),
        session_uuid="session-4d",
    )
    session.begin_new_plan()
    artifact = session.plan_state.artifact
    assert artifact is not None
    artifact.write_text("# Plan\n\nApproved", encoding="utf-8")
    session.exit_plan_mode_tool._request_interaction = lambda request: InteractionResponse(
        interaction_id=request.id,
        status=InteractionStatus.SUBMITTED,
        answer=InteractionAnswer(
            type=InteractionAnswerType.OPTION,
            value="approve",
            label="Approve",
        ),
    )
    context_manager = ContextManager()
    context_manager.add_memory(MemoryType.LLM, {"role": "user", "content": "hello"})

    tool_call_cancelled, output, _messages = await session._handle_tool_calls(
        [
            {
                "id": "call-1",
                "function": {
                    "name": "exit_plan_mode",
                    "arguments": json.dumps({"summary": "ready"}),
                },
            }
        ],
        context_manager,
        None,
        "",
    )

    assert tool_call_cancelled is False
    assert output == ""
    assert session.plan_state.phase == PlanPhase.NORMAL.value
    assert session.plan_state.approval_status == PlanApprovalStatus.APPROVED.value
    llm_memories = [
        memory["content"]
        for memory in context_manager.memories
        if memory["memory_type"] == MemoryType.LLM
    ]
    assert any(
        isinstance(memory, dict)
        and memory.get("role") == "tool"
        and "Approved plan artifact:" in str(memory.get("content"))
        for memory in llm_memories
    )
    assert any(
        isinstance(memory, dict)
        and memory.get("role") == "user"
        and "approved" in str(memory.get("content", "")).lower()
        for memory in llm_memories
    )


def test_execution_continues_using_approved_snapshot_after_draft_changes():
    session = LLMSession(
        config=ConfigModel(model="test-model", api_key="test-key"),
        skill_manager=SkillManager(),
        session_uuid="session-5",
    )
    session.begin_new_plan()
    artifact = session.plan_state.artifact
    assert artifact is not None
    artifact.write_text("# Plan\n\nVersion 1", encoding="utf-8")

    approved_state, snapshot_path = create_approved_snapshot(session.plan_state)
    session.update_plan_state(
        approved_state.with_updates(
            phase=PlanPhase.NORMAL.value,
            approval_status=PlanApprovalStatus.APPROVED.value,
            approved_artifact_hash=compute_artifact_hash(snapshot_path),
        )
    )

    artifact.write_text("# Plan\n\nVersion 2", encoding="utf-8")
    assert session._check_execution_plan_drift() is None
    approved_snapshot = session.plan_state.approved_artifact
    assert approved_snapshot is not None
    assert approved_snapshot.read_text(encoding="utf-8") == "# Plan\n\nVersion 1"


@pytest.mark.anyio
async def test_execution_blocks_when_approved_snapshot_changes():
    session = LLMSession(
        config=ConfigModel(model="test-model", api_key="test-key"),
        skill_manager=SkillManager(),
        session_uuid="session-5b",
    )
    session.begin_new_plan()
    artifact = session.plan_state.artifact
    assert artifact is not None
    artifact.write_text("# Plan\n\nVersion 1", encoding="utf-8")

    approved_state, snapshot_path = create_approved_snapshot(session.plan_state)
    session.update_plan_state(
        approved_state.with_updates(
            phase=PlanPhase.NORMAL.value,
            approval_status=PlanApprovalStatus.APPROVED.value,
            approved_artifact_hash=compute_artifact_hash(snapshot_path),
        )
    )

    approved_snapshot = session.plan_state.approved_artifact
    assert approved_snapshot is not None
    approved_snapshot.write_text("# Plan\n\nTampered", encoding="utf-8")
    outcome = await session.pre_execute_tool("bash_exec", {"command": "echo hi"})

    assert outcome.status == ToolDispatchStatus.SHORT_CIRCUIT
    assert "approved plan artifact changed" in outcome.result.output.lower()


@pytest.mark.anyio
async def test_enter_plan_mode_is_allowed_when_approved_plan_drift_is_detected():
    session = LLMSession(
        config=ConfigModel(model="test-model", api_key="test-key"),
        skill_manager=SkillManager(),
        session_uuid="session-5c",
    )
    session.begin_new_plan()
    artifact = session.plan_state.artifact
    assert artifact is not None
    artifact.write_text("# Plan\n\nVersion 1", encoding="utf-8")

    approved_state, snapshot_path = create_approved_snapshot(session.plan_state)
    session.update_plan_state(
        approved_state.with_updates(
            phase=PlanPhase.NORMAL.value,
            approval_status=PlanApprovalStatus.APPROVED.value,
            approved_artifact_hash=compute_artifact_hash(snapshot_path),
        )
    )

    approved_snapshot = session.plan_state.approved_artifact
    assert approved_snapshot is not None
    approved_snapshot.write_text("# Plan\n\nTampered", encoding="utf-8")

    outcome = await session.pre_execute_tool("enter_plan_mode", {})

    assert outcome.status == ToolDispatchStatus.EXECUTED
    assert outcome.result.ok is True
    assert session.plan_state.phase == PlanPhase.PLANNING.value


def test_normal_mode_hides_plan_only_tools():
    session = LLMSession(
        config=ConfigModel(model="test-model", api_key="test-key"),
        skill_manager=SkillManager(),
        session_uuid="session-7",
    )

    tools = {item["function"]["name"] for item in session._get_tools_spec()}

    assert "exit_plan_mode" not in tools
    assert "enter_plan_mode" in tools
    assert "bash_exec" in tools


def test_decode_plan_state_defaults():
    state = decode_plan_state({}, default_source_session_uuid="session-6")
    assert state.phase == PlanPhase.NORMAL.value
    assert state.source_session_uuid == "session-6"