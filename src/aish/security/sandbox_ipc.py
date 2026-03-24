from __future__ import annotations

import json
import os
import socket
import uuid
from pathlib import Path
from typing import Any, Optional

from .sandbox import DEFAULT_SANDBOX_SOCKET_PATH, SandboxUnavailableError
from .sandbox_types import FsChange, SandboxResult, SandboxSecurityResult


class SandboxIpcClient:
    def __init__(
        self,
        socket_path: Path = DEFAULT_SANDBOX_SOCKET_PATH,
        *,
        timeout_s: float = 60.0,
    ) -> None:
        self._socket_path = Path(socket_path)
        self._timeout_s = float(timeout_s)

    def simulate(self, *, command: str, cwd: Path, repo_root: Path) -> SandboxResult:
        request_id = str(uuid.uuid4())
        timeout_s = float(self._timeout_s)
        payload: dict[str, Any] = {
            "id": request_id,
            "command": str(command),
            "cwd": str(Path(cwd).resolve()),
            "repo_root": str(Path(repo_root).resolve()),
            "client_pid": os.getpid(),
            "timeout_s": timeout_s,
        }

        raw = (json.dumps(payload, ensure_ascii=False) + "\n").encode("utf-8")

        try:
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
                # Give daemon enough time to finish and send response.
                # Daemon enforces its own execution timeout using the same timeout_s.
                sock.settimeout(timeout_s + 5.0)
                sock.connect(str(self._socket_path))
                sock.sendall(raw)

                # Read one JSON line response
                buf = b""
                while b"\n" not in buf:
                    chunk = sock.recv(64 * 1024)
                    if not chunk:
                        break
                    buf += chunk
                    if len(buf) > 8 * 1024 * 1024:
                        raise SandboxUnavailableError(
                            "sandbox_ipc_protocol_error", details="response_too_large"
                        )

        except socket.timeout as exc:
            raise SandboxUnavailableError(
                "sandbox_ipc_timeout", details=str(exc)
            ) from exc
        except (FileNotFoundError, ConnectionRefusedError, OSError) as exc:
            raise SandboxUnavailableError(
                "sandbox_ipc_unavailable", details=str(exc)
            ) from exc

        if not buf:
            raise SandboxUnavailableError(
                "sandbox_ipc_protocol_error", details="empty_response"
            )

        line = buf.split(b"\n", 1)[0]
        try:
            resp = json.loads(line.decode("utf-8", errors="replace"))
        except Exception as exc:
            raise SandboxUnavailableError(
                "sandbox_ipc_protocol_error", details="invalid_json"
            ) from exc

        if not isinstance(resp, dict) or resp.get("id") != request_id:
            raise SandboxUnavailableError(
                "sandbox_ipc_protocol_error", details="id_mismatch"
            )

        if resp.get("ok") is not True:
            reason = resp.get("reason")
            error = resp.get("error")
            detail = str(error or reason or "unknown")
            if isinstance(reason, str) and reason.startswith("sandbox_"):
                raise SandboxUnavailableError(reason, details=detail)
            raise SandboxUnavailableError("sandbox_ipc_failed", details=detail)

        result_obj = resp.get("result")
        if not isinstance(result_obj, dict):
            raise SandboxUnavailableError(
                "sandbox_ipc_protocol_error", details="missing_result"
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

        exit_code = result_obj.get("exit_code")
        stdout = result_obj.get("stdout")
        stderr = result_obj.get("stderr")
        stdout_truncated = bool(result_obj.get("stdout_truncated", False))
        stderr_truncated = bool(result_obj.get("stderr_truncated", False))
        changes_truncated = bool(result_obj.get("changes_truncated", False))

        return SandboxResult(
            exit_code=int(exit_code) if exit_code is not None else 1,
            stdout=str(stdout) if stdout is not None else "",
            stderr=str(stderr) if stderr is not None else "",
            changes=changes,
            stdout_truncated=stdout_truncated,
            stderr_truncated=stderr_truncated,
            changes_truncated=changes_truncated,
        )


class SandboxSecurityIpc:
    """Main-process sandbox runner that delegates execution to the privileged daemon via IPC."""

    def __init__(
        self,
        *,
        repo_root: Path,
        enabled: bool = True,
        socket_path: Path = DEFAULT_SANDBOX_SOCKET_PATH,
        timeout_s: float = 60.0,
    ) -> None:
        self._repo_root = Path(repo_root).resolve()
        self._enabled = enabled
        self._client = SandboxIpcClient(socket_path=socket_path, timeout_s=timeout_s)

    @property
    def enabled(self) -> bool:
        return self._enabled

    def set_enabled(self, enabled: bool) -> None:
        self._enabled = enabled

    def run(self, command: str, cwd: Optional[Path] = None):
        # Keep the shared SandboxSecurityResult contract for callers.
        if not self._enabled:
            return None

        effective_cwd = (cwd or self._repo_root).resolve()
        sandbox_result = self._client.simulate(
            command=command, cwd=effective_cwd, repo_root=self._repo_root
        )
        return SandboxSecurityResult(
            command=command, cwd=effective_cwd, sandbox=sandbox_result
        )
