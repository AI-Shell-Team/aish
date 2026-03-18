from __future__ import annotations

import json
import socket
import threading
from pathlib import Path

import pytest

from aish.security.security_policy import SecurityPolicy
from aish.security.sandbox import SandboxUnavailableError
from aish.security.sandbox import SandboxSecurity
from aish.security.sandbox_ipc import SandboxIpcClient, SandboxSecurityIpc
from aish.security.security_manager import SimpleSecurityManager


def _serve_once(sock_path: Path, handler):
    if sock_path.exists():
        sock_path.unlink()

    srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    srv.bind(str(sock_path))
    srv.listen(1)

    def run():
        try:
            conn, _ = srv.accept()
        except Exception:
            return
        with conn:
            buf = b""
            while b"\n" not in buf:
                chunk = conn.recv(4096)
                if not chunk:
                    break
                buf += chunk
            line = buf.split(b"\n", 1)[0]
            req = json.loads(line.decode("utf-8"))
            resp = handler(req)
            conn.sendall((json.dumps(resp, ensure_ascii=False) + "\n").encode("utf-8"))
        srv.close()

    t = threading.Thread(target=run, daemon=True)
    t.start()
    return t


def test_sandbox_ipc_roundtrip(tmp_path: Path):
    sock_path = tmp_path / "sandbox.sock"

    def handler(req):
        return {
            "id": req["id"],
            "ok": True,
            "result": {
                "exit_code": 0,
                "stdout": "ok",
                "stderr": "",
                "changes": [{"path": "a.txt", "kind": "modified"}],
            },
        }

    _serve_once(sock_path, handler)

    client = SandboxIpcClient(socket_path=sock_path, timeout_s=2.0)
    result = client.simulate(command="echo ok", cwd=tmp_path, repo_root=tmp_path)

    assert result.exit_code == 0
    assert "ok" in (result.stdout or "")
    assert result.changes and result.changes[0].path == "a.txt"


def test_sandbox_ipc_error_response(tmp_path: Path):
    sock_path = tmp_path / "sandbox.sock"

    def handler(req):
        return {
            "id": req["id"],
            "ok": False,
            "reason": "sandbox_unavailable",
            "error": "boom",
        }

    _serve_once(sock_path, handler)

    client = SandboxIpcClient(socket_path=sock_path, timeout_s=2.0)
    with pytest.raises(SandboxUnavailableError):
        client.simulate(command="echo ok", cwd=tmp_path, repo_root=tmp_path)


def test_security_manager_prefers_ipc_for_root_when_enabled(monkeypatch, tmp_path: Path):
    monkeypatch.setattr("aish.security.security_manager.os.geteuid", lambda: 0)
    policy = SecurityPolicy(enable_sandbox=True, rules=[])

    manager = SimpleSecurityManager(
        repo_root=tmp_path,
        policy=policy,
        use_privileged_sandbox=True,
        privileged_sandbox_socket=tmp_path / "sandbox.sock",
    )

    assert isinstance(manager._sandbox_security, SandboxSecurityIpc)


def test_security_manager_uses_local_sandbox_when_privileged_disabled(monkeypatch, tmp_path: Path):
    monkeypatch.setattr("aish.security.security_manager.os.geteuid", lambda: 0)
    policy = SecurityPolicy(enable_sandbox=True, rules=[])

    manager = SimpleSecurityManager(
        repo_root=tmp_path,
        policy=policy,
        use_privileged_sandbox=False,
    )

    assert isinstance(manager._sandbox_security, SandboxSecurity)
