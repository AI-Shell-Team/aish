from __future__ import annotations

import hashlib
import shutil
import uuid
from dataclasses import dataclass, replace
from datetime import datetime, timezone
from enum import Enum
from pathlib import Path
from typing import Any

from aish.config import get_default_aish_data_dir

PLAN_STATE_KEY = "plan_mode"


class PlanPhase(str, Enum):
    NORMAL = "normal"
    PLANNING = "planning"


class PlanApprovalStatus(str, Enum):
    DRAFT = "draft"
    AWAITING_USER = "awaiting_user"
    CHANGES_REQUESTED = "changes_requested"
    APPROVED = "approved"


PLANNING_VISIBLE_TOOL_NAMES = frozenset(
    {
        "read_file",
        "glob",
        "grep",
        "ask_user",
        "write_file",
        "edit_file",
        "exit_plan_mode",
        "memory",
    }
)

PLAN_ONLY_TOOL_NAMES = frozenset({"exit_plan_mode"})

SIDE_EFFECT_TOOL_NAMES = frozenset(
    {
        "bash_exec",
        "python_exec",
        "write_file",
        "edit_file",
        "system_diagnose_agent",
    }
)

READ_ONLY_TOOL_NAMES = frozenset(
    {
        "read_file",
        "glob",
        "grep",
        "ask_user",
        "memory",
    }
)


def utc_now_iso() -> str:
    return datetime.now(timezone.utc).replace(microsecond=0).isoformat()


def _normalize_phase(value: object) -> str:
    raw = str(value or PlanPhase.NORMAL.value).strip().lower()
    if raw == "execution":
        return PlanPhase.NORMAL.value
    if raw in {phase.value for phase in PlanPhase}:
        return raw
    return PlanPhase.NORMAL.value


def _normalize_approval_status(value: object) -> str:
    raw = str(value or PlanApprovalStatus.DRAFT.value).strip().lower()
    if raw in {status.value for status in PlanApprovalStatus}:
        return raw
    return PlanApprovalStatus.DRAFT.value


def _sanitize_session_id(value: str) -> str:
    allowed: list[str] = []
    for ch in str(value or "session"):
        if ch.isalnum() or ch in {"-", "_"}:
            allowed.append(ch)
        else:
            allowed.append("-")
    cleaned = "".join(allowed).strip("-")
    return cleaned or "session"


def _clean_optional_text(value: object) -> str | None:
    if value is None:
        return None
    text = str(value).strip()
    return text or None


def _normalize_revision(value: object) -> int:
    if isinstance(value, bool):
        revision = int(value)
    elif isinstance(value, int):
        revision = value
    elif isinstance(value, float):
        revision = int(value)
    elif isinstance(value, str):
        try:
            revision = int(value)
        except ValueError:
            return 0
    else:
        return 0
    return max(revision, 0)


def _generate_plan_id() -> str:
    return uuid.uuid4().hex[:12]


@dataclass(slots=True)
class PlanModeState:
    phase: str = PlanPhase.NORMAL.value
    plan_id: str | None = None
    artifact_path: str | None = None
    draft_revision: int = 0
    approval_status: str = PlanApprovalStatus.DRAFT.value
    summary: str | None = None
    approved_artifact_path: str | None = None
    approved_revision: int | None = None
    approved_artifact_hash: str | None = None
    approval_feedback_summary: str | None = None
    source_session_uuid: str = ""
    updated_at: str = ""

    def __post_init__(self) -> None:
        self.phase = _normalize_phase(self.phase)
        self.plan_id = _clean_optional_text(self.plan_id)
        self.approval_status = _normalize_approval_status(self.approval_status)
        self.artifact_path = _clean_optional_text(self.artifact_path)
        self.draft_revision = _normalize_revision(self.draft_revision)
        self.summary = _clean_optional_text(self.summary)
        self.approved_artifact_path = _clean_optional_text(self.approved_artifact_path)
        self.approved_revision = (
            _normalize_revision(self.approved_revision)
            if self.approved_revision is not None
            else None
        )
        self.approved_artifact_hash = _clean_optional_text(self.approved_artifact_hash)
        self.approval_feedback_summary = _clean_optional_text(
            self.approval_feedback_summary
        )
        self.source_session_uuid = str(self.source_session_uuid or "")
        self.updated_at = self.updated_at or utc_now_iso()

    @property
    def artifact(self) -> Path | None:
        if not self.artifact_path:
            return None
        return Path(self.artifact_path).expanduser()

    @property
    def approved_artifact(self) -> Path | None:
        if not self.approved_artifact_path:
            return None
        return Path(self.approved_artifact_path).expanduser()

    @property
    def active_plan_artifact(self) -> Path | None:
        return self.approved_artifact or self.artifact

    @property
    def has_approved_plan_context(self) -> bool:
        return (
            self.approval_status == PlanApprovalStatus.APPROVED.value
            and self.approved_artifact is not None
            and self.approved_artifact_hash is not None
        )

    def to_dict(self) -> dict[str, Any]:
        return {
            "phase": self.phase,
            "plan_id": self.plan_id,
            "artifact_path": self.artifact_path,
            "draft_revision": self.draft_revision,
            "approval_status": self.approval_status,
            "summary": self.summary,
            "approved_artifact_path": self.approved_artifact_path,
            "approved_revision": self.approved_revision,
            "approved_artifact_hash": self.approved_artifact_hash,
            "approval_feedback_summary": self.approval_feedback_summary,
            "source_session_uuid": self.source_session_uuid,
            "updated_at": self.updated_at,
        }

    def with_updates(self, **changes: Any) -> "PlanModeState":
        updates = dict(changes)
        updates["updated_at"] = utc_now_iso()
        return replace(self, **updates)


def decode_plan_state(
    raw_state: object,
    *,
    default_source_session_uuid: str = "",
) -> PlanModeState:
    payload = raw_state if isinstance(raw_state, dict) else {}
    return PlanModeState(
        phase=payload.get("phase", PlanPhase.NORMAL.value),
        plan_id=payload.get("plan_id"),
        artifact_path=payload.get("artifact_path"),
        draft_revision=payload.get("draft_revision", 0),
        approval_status=payload.get(
            "approval_status", PlanApprovalStatus.DRAFT.value
        ),
        summary=payload.get("summary"),
        approved_artifact_path=payload.get("approved_artifact_path"),
        approved_revision=payload.get("approved_revision"),
        approved_artifact_hash=payload.get("approved_artifact_hash"),
        approval_feedback_summary=payload.get("approval_feedback_summary"),
        source_session_uuid=str(
            payload.get("source_session_uuid")
            or default_source_session_uuid
            or ""
        ),
        updated_at=str(payload.get("updated_at") or ""),
    )


def encode_plan_state(
    session_state: dict[str, Any] | None,
    plan_state: PlanModeState,
) -> dict[str, Any]:
    updated = dict(session_state or {})
    updated[PLAN_STATE_KEY] = plan_state.to_dict()
    return updated


def get_plan_artifacts_dir(base_dir: Path | None = None) -> Path:
    root = base_dir.expanduser() if base_dir is not None else get_default_aish_data_dir()
    return root / "plans"


def get_default_plan_directory(
    *,
    session_uuid: str,
    plan_id: str,
    base_dir: Path | None = None,
) -> Path:
    plans_dir = get_plan_artifacts_dir(base_dir)
    directory_name = (
        f"{_sanitize_session_id(session_uuid)}-{_sanitize_session_id(plan_id)}"
    )
    return plans_dir / directory_name


def get_default_plan_artifact_path(
    *,
    session_uuid: str,
    plan_id: str,
    base_dir: Path | None = None,
) -> Path:
    return get_default_plan_directory(
        session_uuid=session_uuid,
        plan_id=plan_id,
        base_dir=base_dir,
    ) / "plan.md"


def get_default_approved_artifact_path(
    *,
    session_uuid: str,
    plan_id: str,
    revision: int,
    base_dir: Path | None = None,
) -> Path:
    plan_dir = get_default_plan_directory(
        session_uuid=session_uuid,
        plan_id=plan_id,
        base_dir=base_dir,
    )
    return plan_dir / "snapshots" / f"approved-r{max(revision, 0)}.md"


def build_default_plan_template() -> str:
    return "\n".join(
        [
            "# Plan",
            "",
            "## Context",
            "",
            "## Goal",
            "",
            "## Constraints",
            "",
            "## Findings",
            "",
            "## Proposed Steps",
            "",
            "## Affected Files And Systems",
            "",
            "## Verification",
            "",
            "## Open Questions",
            "",
        ]
    )


def ensure_plan_artifact(
    plan_state: PlanModeState,
    *,
    session_uuid: str,
    base_dir: Path | None = None,
) -> PlanModeState:
    plan_id = plan_state.plan_id or _generate_plan_id()
    artifact_path = plan_state.artifact_path
    artifact = (
        Path(artifact_path).expanduser()
        if artifact_path
        else get_default_plan_artifact_path(
            session_uuid=session_uuid,
            plan_id=plan_id,
            base_dir=base_dir,
        )
    )
    artifact.parent.mkdir(parents=True, exist_ok=True)
    if not artifact.exists():
        artifact.write_text(build_default_plan_template(), encoding="utf-8")
    return plan_state.with_updates(
        plan_id=plan_id,
        artifact_path=str(artifact),
        source_session_uuid=plan_state.source_session_uuid or session_uuid,
    )


def create_new_plan_state(
    *,
    session_uuid: str,
    base_dir: Path | None = None,
) -> PlanModeState:
    plan_state = PlanModeState(
        phase=PlanPhase.PLANNING.value,
        plan_id=_generate_plan_id(),
        artifact_path=None,
        draft_revision=0,
        approval_status=PlanApprovalStatus.DRAFT.value,
        summary=None,
        approved_artifact_path=None,
        approved_revision=None,
        approved_artifact_hash=None,
        approval_feedback_summary=None,
        source_session_uuid=session_uuid,
    )
    return ensure_plan_artifact(
        plan_state,
        session_uuid=session_uuid,
        base_dir=base_dir,
    )


def bump_draft_revision(plan_state: PlanModeState) -> PlanModeState:
    return plan_state.with_updates(
        draft_revision=plan_state.draft_revision + 1,
        approval_status=PlanApprovalStatus.DRAFT.value,
        approved_artifact_path=None,
        approved_revision=None,
        approved_artifact_hash=None,
        approval_feedback_summary=None,
    )


def create_approved_snapshot(
    plan_state: PlanModeState,
    *,
    base_dir: Path | None = None,
) -> tuple[PlanModeState, Path]:
    artifact = plan_state.artifact
    if artifact is None:
        raise ValueError("plan artifact is not initialized")

    plan_id = plan_state.plan_id or _generate_plan_id()
    session_uuid = plan_state.source_session_uuid or "session"
    snapshot = get_default_approved_artifact_path(
        session_uuid=session_uuid,
        plan_id=plan_id,
        revision=plan_state.draft_revision,
        base_dir=base_dir,
    )
    snapshot.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(artifact, snapshot)
    approved_hash = compute_artifact_hash(snapshot)
    next_state = plan_state.with_updates(
        plan_id=plan_id,
        approved_artifact_path=str(snapshot),
        approved_revision=plan_state.draft_revision,
        approved_artifact_hash=approved_hash,
    )
    return next_state, snapshot


def read_artifact_text(path: str | Path | None) -> str:
    if not path:
        return ""
    artifact = Path(path).expanduser()
    if not artifact.exists() or not artifact.is_file():
        return ""
    return artifact.read_text(encoding="utf-8")


def compute_artifact_hash(path: str | Path | None) -> str | None:
    if not path:
        return None
    artifact = Path(path).expanduser()
    if not artifact.exists() or not artifact.is_file():
        return None
    return hashlib.sha256(artifact.read_bytes()).hexdigest()


def is_approved_plan_current(plan_state: PlanModeState) -> bool:
    if not plan_state.has_approved_plan_context:
        return False
    approved_artifact = plan_state.approved_artifact
    if approved_artifact is None:
        return False
    return (
        compute_artifact_hash(approved_artifact)
        == plan_state.approved_artifact_hash
    )


def is_freshly_approved(plan_state: PlanModeState) -> bool:
    return is_approved_plan_current(plan_state)