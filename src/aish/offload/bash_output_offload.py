from __future__ import annotations

import datetime as dt
import hashlib
import json
import os
import threading
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from aish.config import BashOutputOffloadSettings, get_default_aish_data_dir

_EXEC_SEQ_LOCK = threading.Lock()
_EXEC_SEQ = 0


def _utf8_len(text: str) -> int:
    return len(text.encode("utf-8"))


def _truncate_utf8_bytes(text: str, limit_bytes: int) -> tuple[str, bool]:
    if limit_bytes <= 0:
        return "", bool(text)
    raw = text.encode("utf-8")
    if len(raw) <= limit_bytes:
        return text, False
    return raw[:limit_bytes].decode("utf-8", errors="ignore"), True


def _default_offload_base_dir() -> Path:
    return get_default_aish_data_dir() / "offload"


def _next_exec_seq() -> int:
    global _EXEC_SEQ
    with _EXEC_SEQ_LOCK:
        _EXEC_SEQ += 1
        return _EXEC_SEQ


def _safe_session_dir_name(session_uuid: str) -> str:
    value = "".join(
        ch if (ch.isalnum() or ch in {"-", "_", "."}) else "_"
        for ch in str(session_uuid or "unknown-session")
    ).strip("_")
    return value or "unknown-session"


def _build_exec_id(now: dt.datetime, pid: int, seq: int) -> str:
    return f"{now.strftime('%Y%m%dT%H%M%S')}.{now.microsecond // 1000:03d}_{pid}_{seq}"


def _build_uid(command: str, now: dt.datetime, pid: int, seq: int) -> str:
    payload = f"{now.timestamp()}:{pid}:{seq}:{command}"
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()[:16]


def _mkdir_700(path: Path) -> None:
    path.mkdir(parents=True, exist_ok=True)
    os.chmod(path, 0o700)


def _write_text_file(path: Path, content: str) -> None:
    flags = os.O_WRONLY | os.O_CREAT | os.O_TRUNC
    fd = os.open(path, flags, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as handle:
        handle.write(content)


def _write_json_file(path: Path, payload: dict[str, Any]) -> None:
    _write_text_file(
        path, json.dumps(payload, ensure_ascii=False, indent=2, sort_keys=True)
    )


@dataclass(slots=True)
class BashOffloadRenderResult:
    stdout_text: str
    stderr_text: str
    offload_payload: dict[str, Any]


def render_bash_output(
    *,
    stdout: str,
    stderr: str,
    command: str,
    return_code: int,
    session_uuid: str,
    cwd: str,
    settings: BashOutputOffloadSettings,
) -> BashOffloadRenderResult:
    if not settings.enabled:
        return BashOffloadRenderResult(
            stdout_text=stdout,
            stderr_text=stderr,
            offload_payload={"status": "inline", "reason": "disabled"},
        )

    stdout_bytes = _utf8_len(stdout)
    stderr_bytes = _utf8_len(stderr)
    should_offload = (
        stdout_bytes > settings.threshold_bytes
        or stderr_bytes > settings.threshold_bytes
    )

    if not should_offload:
        return BashOffloadRenderResult(
            stdout_text=stdout,
            stderr_text=stderr,
            offload_payload={"status": "inline", "reason": "below_threshold"},
        )

    preview_stdout, _ = _truncate_utf8_bytes(stdout, settings.preview_bytes)
    preview_stderr, _ = _truncate_utf8_bytes(stderr, settings.preview_bytes)

    now = dt.datetime.now()
    pid = os.getpid()
    seq = _next_exec_seq()
    exec_id = _build_exec_id(now, pid, seq)
    uid = _build_uid(command, now, pid, seq)

    base_dir = (
        Path(settings.base_dir).expanduser()
        if settings.base_dir
        else _default_offload_base_dir()
    )
    session_dir = base_dir / _safe_session_dir_name(session_uuid)
    exec_dir = session_dir / exec_id

    try:
        _mkdir_700(base_dir)
        _mkdir_700(session_dir)
        _mkdir_700(exec_dir)

        stdout_path = exec_dir / "stdout.txt"
        stderr_path = exec_dir / "stderr.txt"
        meta_path = exec_dir / f"result_{uid}.json"

        _write_text_file(stdout_path, stdout)
        _write_text_file(stderr_path, stderr)

        meta_payload: dict[str, Any] = {
            "version": 1,
            "tool": "bash_exec",
            "uid": uid,
            "session_uuid": session_uuid,
            "exec_id": exec_id,
            "timestamp_utc": now.astimezone(dt.timezone.utc).isoformat(),
            "cwd": cwd,
            "return_code": return_code,
            "command_sha256": hashlib.sha256(command.encode("utf-8")).hexdigest(),
            "threshold_bytes": settings.threshold_bytes,
            "preview_bytes": settings.preview_bytes,
            "stdout": {
                "path": str(stdout_path.resolve()),
                "bytes": stdout_bytes,
                "sha256": hashlib.sha256(stdout.encode("utf-8")).hexdigest(),
            },
            "stderr": {
                "path": str(stderr_path.resolve()),
                "bytes": stderr_bytes,
                "sha256": hashlib.sha256(stderr.encode("utf-8")).hexdigest(),
            },
        }
        if settings.write_meta:
            _write_json_file(meta_path, meta_payload)

        return BashOffloadRenderResult(
            stdout_text=preview_stdout,
            stderr_text=preview_stderr,
            offload_payload={
                "status": "offloaded",
                "stdout_path": str(stdout_path.resolve()),
                "stderr_path": str(stderr_path.resolve()),
                "meta_path": str(meta_path.resolve()) if settings.write_meta else "",
                "hint": "Read offload paths for full output",
            },
        )
    except Exception as exc:
        return BashOffloadRenderResult(
            stdout_text=preview_stdout,
            stderr_text=preview_stderr,
            offload_payload={
                "status": "failed",
                "error": str(exc),
                "hint": "Output shown as preview only",
            },
        )
