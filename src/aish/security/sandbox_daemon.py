from __future__ import annotations

import json
import logging
import os
import pwd
import socket
import struct
import subprocess
import sys
import threading
import time
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Optional

from ..state.logging import (add_context_filter,
                             build_sandboxd_rotating_file_handler,
                             init_sandboxd_logging, set_session_uuid)
from .sandbox import (DEFAULT_SANDBOX_SOCKET_PATH, SandboxUnavailableError,
                      strip_sudo_prefix)
from .sandbox_types import FsChange, SandboxResult

_SANDBOX_LOG_FILE = "sandbox.log"
_SANDBOX_LOG_DIR = Path(".config") / "aish" / "logs"
_PATH_MAX_LEN = 200
_CHANGES_SAMPLE_MAX = 5
_IPC_STDIO_MAX_BYTES = 2 * 1024 * 1024
_IPC_CHANGES_MAX = 10_000
_LOG_DETAIL_MAX_CHARS = 4096


def _build_worker_command() -> list[str]:
    worker_cmd = [
        "unshare",
        "--mount",
        "--propagation",
        "private",
        "--",
        sys.executable,
    ]
    if getattr(sys, "frozen", False):
        worker_cmd.append("--sandbox-worker")
        return worker_cmd

    worker_cmd.extend(["-m", "aish.security.sandbox_worker"])
    return worker_cmd


def _elapsed_ms(start_ts: float) -> int:
    return int((time.monotonic() - start_ts) * 1000)


def _sanitize_detail(detail: Optional[str]) -> Optional[str]:
    if not detail:
        return None
    s = str(detail)
    if len(s) <= _LOG_DETAIL_MAX_CHARS:
        return s
    return s[:_LOG_DETAIL_MAX_CHARS] + "\n…(truncated)…"


def _sanitize_path_value(value: Optional[str]) -> Optional[str]:
    if not value:
        return None
    s = str(value)
    if len(s) <= _PATH_MAX_LEN:
        return s
    # Keep tail for debugging (paths are often distinctive at the end)
    keep_tail = 120
    return "…" + s[-keep_tail:]


def _summarize_changes(changes: Any) -> tuple[int, int, int, int, list[str]]:
    created = modified = deleted = 0
    samples: list[str] = []
    if not isinstance(changes, list):
        return 0, 0, 0, 0, samples

    for c in changes:
        try:
            kind = getattr(c, "kind", None)
            path = getattr(c, "path", None)
        except Exception:
            continue

        if kind == "created":
            created += 1
        elif kind == "modified":
            modified += 1
        elif kind == "deleted":
            deleted += 1

        if (
            len(samples) < _CHANGES_SAMPLE_MAX
            and isinstance(kind, str)
            and isinstance(path, str)
        ):
            # Keep it short and structured.
            samples.append(f"{kind}:{path}")

    total = len(changes)
    return total, created, modified, deleted, samples


def _preferred_sandbox_log_path(uid: int) -> Path:
    pw = pwd.getpwuid(uid)
    home = Path(pw.pw_dir)
    return home / _SANDBOX_LOG_DIR / _SANDBOX_LOG_FILE


def _try_build_file_handler(log_path: Path) -> Optional[logging.Handler]:
    try:
        log_path.parent.mkdir(parents=True, exist_ok=True)
        return build_sandboxd_rotating_file_handler(log_path, logging.INFO)
    except Exception:
        return None


def _format_log_message(
    *,
    daemon_pid: int,
    daemon_session: str,
    uid: int,
    gid: int,
    peer_pid: Optional[int],
    client_pid: Optional[int],
    stage: str,
    status: str,
    cmd: Optional[str],
    run_as: Optional[str],
    reason: Optional[str],
    detail: Optional[str],
    exit_code: Optional[int],
    changes_count: Optional[int],
    changes_created: Optional[int],
    changes_modified: Optional[int],
    changes_deleted: Optional[int],
    changes_sample: Optional[list[str]],
    repo_root: Optional[str],
    cwd: Optional[str],
    simulate_ms: Optional[int],
    duration_ms: Optional[int],
) -> str:
    state = f"{stage}_{status}"
    cwd_s = _sanitize_path_value(cwd)
    repo_root_s = _sanitize_path_value(repo_root)

    run_as_display = run_as or "-"

    file_changes_display = "unknown"
    if isinstance(changes_count, int):
        if changes_count <= 0:
            file_changes_display = "none"
        else:
            file_changes_display = str(changes_count)
            if (
                changes_created is not None
                or changes_modified is not None
                or changes_deleted is not None
            ):
                file_changes_display = (
                    f"{changes_count} (created:{int(changes_created or 0)}, "
                    f"modified:{int(changes_modified or 0)}, deleted:{int(changes_deleted or 0)})"
                )

    header = f"sandboxd(uid={uid}, pid={daemon_pid}, session={daemon_session})"
    meta_bits: list[str] = []
    if peer_pid is not None and peer_pid >= 0:
        meta_bits.append(f"peer_pid={peer_pid}")
    if client_pid is not None:
        meta_bits.append(f"client_pid={client_pid}")
    if meta_bits:
        header += " " + ", ".join(meta_bits)

    lines: list[str] = [header]
    if cmd:
        lines.append(f"  Command: {cmd}")
    if cwd_s:
        lines.append(f"  CWD: {cwd_s} (run as {run_as_display})")
    if repo_root_s:
        lines.append(f"  RepoRoot: {repo_root_s}")

    status_bits: list[str] = [f"Status: {state}"]
    if exit_code is not None:
        status_bits.append(f"exit_code={exit_code}")
    status_bits.append(f"file_changes={file_changes_display}")
    lines.append("  " + " | ".join(status_bits))

    if reason:
        lines.append(f"  Reason: {reason}")

    detail_s = _sanitize_detail(detail)
    if detail_s:
        lines.append("  Error:")
        for raw_line in detail_s.splitlines():
            line = raw_line.strip("\n")
            if not line.strip():
                continue
            lines.append(f"    > {line}")

    if changes_sample:
        lines.append(
            f"  ChangeSample: {json.dumps(changes_sample, ensure_ascii=False)}"
        )
    if simulate_ms is not None or duration_ms is not None:
        timing_bits: list[str] = []
        if simulate_ms is not None:
            timing_bits.append(f"simulate_ms={simulate_ms}")
        if duration_ms is not None:
            timing_bits.append(f"duration_ms={duration_ms}")
        lines.append("  Timing: " + " | ".join(timing_bits))

    return "\n".join(lines)


def _truncate_text(value: str, limit: int) -> tuple[str, bool]:
    if value is None:
        return "", False
    if len(value) <= limit:
        return value, False
    return value[:limit], True


def _get_systemd_listen_socket() -> Optional[socket.socket]:
    """Return the pre-opened listening socket from systemd socket activation.

    systemd passes file descriptors starting from 3 and sets LISTEN_FDS/LISTEN_PID.
    We only need one socket.
    """

    try:
        listen_fds = int(os.environ.get("LISTEN_FDS", "0"))
        listen_pid = int(os.environ.get("LISTEN_PID", "0"))
    except ValueError:
        return None

    if listen_fds < 1:
        return None
    if listen_pid != os.getpid():
        return None

    # systemd guarantees fds are sequential from 3
    fd = 3
    try:
        sock = socket.fromfd(fd, socket.AF_UNIX, socket.SOCK_STREAM)
        # Duplicate so closing doesn't affect inherited fd table unexpectedly.
        dup = sock.dup()
        sock.close()
        return dup
    except Exception:
        return None


def _peercred(conn: socket.socket) -> tuple[int, int, int]:
    """Return (pid, uid, gid) for a Unix domain socket peer (Linux)."""

    SO_PEERCRED = getattr(socket, "SO_PEERCRED", 17)
    creds = conn.getsockopt(socket.SOL_SOCKET, SO_PEERCRED, struct.calcsize("3i"))
    pid, uid, gid = struct.unpack("3i", creds)
    return pid, uid, gid


@dataclass
class SandboxDaemonConfig:
    socket_path: Path = DEFAULT_SANDBOX_SOCKET_PATH
    backlog: int = 32
    max_request_bytes: int = 1024 * 1024


class SandboxDaemon:
    def __init__(self, config: SandboxDaemonConfig) -> None:
        self._config = config
        self._sock: Optional[socket.socket] = None
        self._stop_event = threading.Event()
        # sandboxd is a long-lived process and doesn't share the interactive shell
        # session id; generate a stable id for this daemon instance.
        self._session_uuid = uuid.uuid4().hex[:8]
        set_session_uuid(self._session_uuid)
        self._logger = init_sandboxd_logging(
            log_path=None, level=logging.INFO, also_stderr=True
        )
        self._log_lock = threading.Lock()
        self._file_handlers: dict[int, logging.Handler] = {}
        self._file_handler_order: list[int] = []
        self._max_file_handlers = 32

    def serve_forever(self) -> None:
        sock = _get_systemd_listen_socket()
        if sock is None:
            sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            path = str(self._config.socket_path)
            Path(path).parent.mkdir(parents=True, exist_ok=True)
            try:
                os.unlink(path)
            except FileNotFoundError:
                pass
            sock.bind(path)
            os.chmod(path, 0o666)
            sock.listen(self._config.backlog)
        else:
            sock.listen(self._config.backlog)

        self._sock = sock

        while not self._stop_event.is_set():
            try:
                conn, _addr = sock.accept()
            except OSError:
                continue
            t = threading.Thread(
                target=self._handle_connection, args=(conn,), daemon=True
            )
            t.start()

    def stop(self) -> None:
        self._stop_event.set()
        try:
            if self._sock is not None:
                self._sock.close()
        except Exception:
            pass

    def _handle_connection(self, conn: socket.socket) -> None:
        with conn:
            start_ts = time.monotonic()
            try:
                peer_pid, uid, gid = _peercred(conn)
            except Exception:
                peer_pid = -1
                uid = -1
                gid = -1

            client_pid: Optional[int] = None
            timeout_s: Optional[float] = None
            command: Optional[str] = None
            cwd: Optional[str] = None
            repo_root: Optional[str] = None

            try:
                raw = self._recv_line(conn)
                req = json.loads(raw.decode("utf-8", errors="replace"))
            except Exception as exc:
                self._log_event(
                    uid=uid,
                    gid=gid,
                    peer_pid=peer_pid,
                    client_pid=client_pid,
                    stage="build",
                    status="fail",
                    cmd=None,
                    run_as=None,
                    reason="bad_request",
                    detail=str(exc),
                    repo_root=repo_root,
                    cwd=cwd,
                    simulate_ms=None,
                    duration_ms=_elapsed_ms(start_ts),
                )
                self._send_json(
                    conn,
                    {
                        "id": None,
                        "ok": False,
                        "reason": "bad_request",
                        "error": str(exc),
                    },
                )
                return

            req_id = req.get("id") if isinstance(req, dict) else None
            if not isinstance(req, dict) or not isinstance(req_id, str):
                self._log_event(
                    uid=uid,
                    gid=gid,
                    peer_pid=peer_pid,
                    client_pid=client_pid,
                    stage="build",
                    status="fail",
                    cmd=None,
                    run_as=None,
                    reason="bad_request",
                    detail="missing_id",
                    repo_root=repo_root,
                    cwd=cwd,
                    simulate_ms=None,
                    duration_ms=_elapsed_ms(start_ts),
                )
                self._send_json(
                    conn,
                    {
                        "id": None,
                        "ok": False,
                        "reason": "bad_request",
                        "error": "missing_id",
                    },
                )
                return

            client_pid_raw = req.get("client_pid")
            if isinstance(client_pid_raw, int):
                client_pid = client_pid_raw

            timeout_raw = req.get("timeout_s")
            if isinstance(timeout_raw, (int, float)):
                # Clamp to sane bounds to avoid abuse.
                v = float(timeout_raw)
                if 1.0 <= v <= 300.0:
                    timeout_s = v

            command = req.get("command")
            cwd = req.get("cwd")
            repo_root = req.get("repo_root")
            if (
                not isinstance(command, str)
                or not isinstance(cwd, str)
                or not isinstance(repo_root, str)
            ):
                self._log_event(
                    uid=uid,
                    gid=gid,
                    peer_pid=peer_pid,
                    client_pid=client_pid,
                    stage="build",
                    status="fail",
                    cmd=command if isinstance(command, str) else None,
                    run_as=None,
                    reason="bad_request",
                    detail="missing_fields",
                    repo_root=repo_root if isinstance(repo_root, str) else None,
                    cwd=cwd if isinstance(cwd, str) else None,
                    simulate_ms=None,
                    duration_ms=_elapsed_ms(start_ts),
                )
                self._send_json(
                    conn,
                    {
                        "id": req_id,
                        "ok": False,
                        "reason": "bad_request",
                        "error": "missing_fields",
                    },
                )
                return

            run_as: Optional[str] = None
            simulate_start_ts = time.monotonic()
            _, sudo_detected, ok = strip_sudo_prefix(command)
            if sudo_detected:
                if not ok:
                    self._log_event(
                        uid=uid,
                        gid=gid,
                        peer_pid=peer_pid,
                        client_pid=client_pid,
                        stage="build",
                        status="fail",
                        cmd=command,
                        run_as="root",
                        reason="sandbox_execute_failed",
                        detail="missing_command",
                        repo_root=repo_root,
                        cwd=cwd,
                        simulate_ms=_elapsed_ms(simulate_start_ts),
                        duration_ms=_elapsed_ms(start_ts),
                    )
                    self._send_json(
                        conn,
                        {
                            "id": req_id,
                            "ok": False,
                            "reason": "sandbox_execute_failed",
                            "error": "missing_command",
                        },
                    )
                    return
                run_as = "root"
            else:
                run_as = f"{uid}:{gid}"

            try:
                result = self._simulate_for_user(
                    command=command,
                    cwd=Path(cwd),
                    repo_root=Path(repo_root),
                    uid=uid,
                    gid=gid,
                    timeout_s=timeout_s,
                )
            except SandboxUnavailableError as exc:
                self._log_event(
                    uid=uid,
                    gid=gid,
                    peer_pid=peer_pid,
                    client_pid=client_pid,
                    stage="build",
                    status="fail",
                    cmd=command,
                    run_as=run_as,
                    reason=exc.reason,
                    detail=exc.details or str(exc),
                    repo_root=repo_root,
                    cwd=cwd,
                    simulate_ms=_elapsed_ms(simulate_start_ts),
                    duration_ms=_elapsed_ms(start_ts),
                )
                self._send_json(
                    conn,
                    {
                        "id": req_id,
                        "ok": False,
                        "reason": exc.reason,
                        "error": exc.details or str(exc),
                    },
                )
                return
            except Exception as exc:
                self._log_event(
                    uid=uid,
                    gid=gid,
                    peer_pid=peer_pid,
                    client_pid=client_pid,
                    stage="build",
                    status="fail",
                    cmd=command,
                    run_as=run_as,
                    reason="server_error",
                    detail=f"{type(exc).__name__}: {exc}",
                    repo_root=repo_root,
                    cwd=cwd,
                    simulate_ms=_elapsed_ms(simulate_start_ts),
                    duration_ms=_elapsed_ms(start_ts),
                )
                self._send_json(
                    conn,
                    {
                        "id": req_id,
                        "ok": False,
                        "reason": "server_error",
                        "error": f"{type(exc).__name__}: {exc}",
                    },
                )
                return

            changes_total, c_created, c_modified, c_deleted, samples = (
                _summarize_changes(result.changes or [])
            )
            exit_code_int = int(result.exit_code)
            stderr_full = result.stderr or ""
            reason = None if exit_code_int == 0 else "sandbox_execute_failed"
            detail = None if exit_code_int == 0 else (stderr_full or None)

            self._log_event(
                uid=uid,
                gid=gid,
                stage="exec",
                status=("success" if exit_code_int == 0 else "fail"),
                cmd=command,
                run_as=run_as,
                reason=reason,
                detail=detail,
                exit_code=exit_code_int,
                changes_count=changes_total,
                changes_created=c_created,
                changes_modified=c_modified,
                changes_deleted=c_deleted,
                changes_sample=samples,
                repo_root=repo_root,
                cwd=cwd,
                simulate_ms=_elapsed_ms(simulate_start_ts),
                duration_ms=_elapsed_ms(start_ts),
            )

            stdout_limited, stdout_truncated = _truncate_text(
                result.stdout or "", _IPC_STDIO_MAX_BYTES
            )
            stderr_limited, stderr_truncated = _truncate_text(
                result.stderr or "", _IPC_STDIO_MAX_BYTES
            )
            changes_raw = result.changes or []
            changes_truncated = len(changes_raw) > _IPC_CHANGES_MAX
            changes_limited = changes_raw[:_IPC_CHANGES_MAX]

            self._send_json(
                conn,
                {
                    "id": req_id,
                    "ok": True,
                    "result": {
                        "exit_code": result.exit_code,
                        "stdout": stdout_limited,
                        "stderr": stderr_limited,
                        "stdout_truncated": stdout_truncated,
                        "stderr_truncated": stderr_truncated,
                        "changes_truncated": changes_truncated,
                        "changes": [
                            {"path": c.path, "kind": c.kind} for c in changes_limited
                        ],
                    },
                },
            )

    def _recv_line(self, conn: socket.socket) -> bytes:
        buf = b""
        while b"\n" not in buf:
            chunk = conn.recv(64 * 1024)
            if not chunk:
                break
            buf += chunk
            if len(buf) > self._config.max_request_bytes:
                raise ValueError("request_too_large")
        return buf.split(b"\n", 1)[0]

    def _send_json(self, conn: socket.socket, obj: dict[str, Any]) -> None:
        data = (json.dumps(obj, ensure_ascii=False) + "\n").encode("utf-8")
        try:
            conn.sendall(data)
        except (BrokenPipeError, ConnectionResetError):
            # Client closed early (timeout/cancel/crash). Not a sandbox failure.
            return

    def _log_event(
        self,
        *,
        uid: int,
        gid: int,
        peer_pid: Optional[int] = None,
        client_pid: Optional[int] = None,
        stage: str,
        status: str,
        cmd: Optional[str] = None,
        run_as: Optional[str] = None,
        reason: Optional[str] = None,
        detail: Optional[str] = None,
        exit_code: Optional[int] = None,
        changes_count: Optional[int] = None,
        changes_created: Optional[int] = None,
        changes_modified: Optional[int] = None,
        changes_deleted: Optional[int] = None,
        changes_sample: Optional[list[str]] = None,
        repo_root: Optional[str] = None,
        cwd: Optional[str] = None,
        simulate_ms: Optional[int] = None,
        duration_ms: Optional[int] = None,
    ) -> None:
        message = _format_log_message(
            daemon_pid=os.getpid(),
            daemon_session=self._session_uuid,
            uid=uid,
            gid=gid,
            peer_pid=peer_pid,
            client_pid=client_pid,
            stage=stage,
            status=status,
            cmd=cmd,
            run_as=run_as,
            reason=reason,
            detail=detail,
            exit_code=exit_code,
            changes_count=changes_count,
            changes_created=changes_created,
            changes_modified=changes_modified,
            changes_deleted=changes_deleted,
            changes_sample=changes_sample,
            repo_root=repo_root,
            cwd=cwd,
            simulate_ms=simulate_ms,
            duration_ms=duration_ms,
        )

        level = logging.INFO if status == "success" else logging.WARNING
        self._logger.log(level, message)

        file_logger = self._get_user_file_logger(uid)
        if file_logger is not None:
            file_logger.log(level, message)

    def _get_user_file_logger(self, uid: int) -> Optional[logging.Logger]:
        if uid < 0:
            return None

        with self._log_lock:
            handler = self._file_handlers.get(uid)
            if handler is not None:
                return logging.getLogger(f"aish.sandboxd.uid.{uid}")

            handler = self._build_user_file_handler(uid)
            if handler is None:
                return None

            logger = logging.getLogger(f"aish.sandboxd.uid.{uid}")
            logger.setLevel(logging.DEBUG)
            logger.propagate = False
            if not logger.handlers:
                add_context_filter(handler)
                logger.addHandler(handler)

            self._file_handlers[uid] = handler
            self._file_handler_order.append(uid)
            self._evict_old_handlers_locked()
            return logger

    def _build_user_file_handler(self, uid: int) -> Optional[logging.Handler]:
        try:
            preferred_path = _preferred_sandbox_log_path(uid)
        except Exception:
            return None
        return _try_build_file_handler(preferred_path)

    def _evict_old_handlers_locked(self) -> None:
        while len(self._file_handler_order) > self._max_file_handlers:
            old_uid = self._file_handler_order.pop(0)
            handler = self._file_handlers.pop(old_uid, None)
            if handler is not None:
                try:
                    handler.close()
                except Exception:
                    pass

    def _simulate_for_user(
        self,
        *,
        command: str,
        cwd: Path,
        repo_root: Path,
        uid: int,
        gid: int,
        timeout_s: Optional[float] = None,
    ):
        rr = Path(repo_root).resolve()
        wd = Path(cwd).resolve()
        if not rr.is_absolute() or not wd.is_absolute():
            raise SandboxUnavailableError(
                "invalid_paths", details="repo_root/cwd must be absolute"
            )

        sim_uid: Optional[int] = uid if uid >= 0 else None
        sim_gid: Optional[int] = gid if gid >= 0 else None

        # Sudo handling for simulation:
        # - Strip leading sudo and flags while keeping command structure.
        # - If sudo is detected, simulate as root to observe privileged side effects.
        stripped_cmd, sudo_detected, ok = strip_sudo_prefix(command)
        if sudo_detected:
            if not ok:
                raise SandboxUnavailableError(
                    "sandbox_execute_failed", details="missing_command"
                )
            command = stripped_cmd
            sim_uid = None
            sim_gid = None

        payload = {
            "command": command,
            "cwd": str(wd),
            "repo_root": str(rr),
            "sim_uid": sim_uid,
            "sim_gid": sim_gid,
            "timeout_s": timeout_s,
        }

        worker_cmd = _build_worker_command()
        exec_timeout: Optional[float] = None
        if timeout_s is not None:
            exec_timeout = float(timeout_s) + 10.0

        try:
            proc = subprocess.run(
                worker_cmd,
                input=json.dumps(payload, ensure_ascii=False),
                text=True,
                capture_output=True,
                timeout=exec_timeout,
            )
        except subprocess.TimeoutExpired as exc:
            details = f"timeout_s={timeout_s}" if timeout_s is not None else "timeout"
            raise SandboxUnavailableError("sandbox_timeout", details=details) from exc
        except FileNotFoundError as exc:
            raise SandboxUnavailableError(
                "command_not_found", details="unshare"
            ) from exc
        except OSError as exc:
            raise SandboxUnavailableError(
                "sandbox_unavailable", details=str(exc)
            ) from exc

        stdout_text = (proc.stdout or "").strip()
        if not stdout_text:
            stderr_text = (proc.stderr or "").strip()
            detail = stderr_text or f"worker_exit={proc.returncode}"
            raise SandboxUnavailableError("sandbox_execute_failed", details=detail)

        line = stdout_text.splitlines()[-1]
        try:
            resp = json.loads(line)
        except Exception as exc:
            detail = f"invalid_worker_json: {line[:200]}"
            raise SandboxUnavailableError(
                "sandbox_execute_failed", details=detail
            ) from exc

        if not isinstance(resp, dict):
            raise SandboxUnavailableError(
                "sandbox_execute_failed", details="worker_response_not_object"
            )

        if resp.get("ok") is not True:
            reason = str(resp.get("reason") or "sandbox_execute_failed")
            error = str(resp.get("error") or reason)
            raise SandboxUnavailableError(reason, details=error)

        result_obj = resp.get("result")
        if not isinstance(result_obj, dict):
            raise SandboxUnavailableError(
                "sandbox_execute_failed", details="worker_missing_result"
            )

        changes_raw = result_obj.get("changes")
        changes: list[FsChange] = []
        if isinstance(changes_raw, list):
            for item in changes_raw:
                if not isinstance(item, dict):
                    continue
                path = item.get("path")
                kind = item.get("kind")
                if isinstance(path, str) and isinstance(kind, str):
                    changes.append(FsChange(path=path, kind=kind))

        return SandboxResult(
            exit_code=int(result_obj.get("exit_code", 1)),
            stdout=str(result_obj.get("stdout") or ""),
            stderr=str(result_obj.get("stderr") or ""),
            changes=changes,
        )
