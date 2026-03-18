from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import pytest

from aish.security.sandbox import SandboxUnavailableError
from aish.security import sandbox_daemon
from aish.security.sandbox_daemon import SandboxDaemon, SandboxDaemonConfig


def _make_daemon() -> SandboxDaemon:
    return SandboxDaemon(SandboxDaemonConfig(socket_path=Path("/tmp/aish-test.sock")))


def test_build_worker_command_uses_module_entrypoint(monkeypatch):
    monkeypatch.delattr(sys, "frozen", raising=False)
    monkeypatch.setattr(sys, "executable", "/usr/bin/python3")

    cmd = sandbox_daemon._build_worker_command()

    assert cmd == [
        "unshare",
        "--mount",
        "--propagation",
        "private",
        "--",
        "/usr/bin/python3",
        "-m",
        "aish.security.sandbox_worker",
    ]


def test_build_worker_command_uses_internal_entrypoint_when_frozen(monkeypatch):
    monkeypatch.setattr(sys, "frozen", True, raising=False)
    monkeypatch.setattr(sys, "executable", "/usr/bin/aish-sandbox")

    cmd = sandbox_daemon._build_worker_command()

    assert cmd == [
        "unshare",
        "--mount",
        "--propagation",
        "private",
        "--",
        "/usr/bin/aish-sandbox",
        "--sandbox-worker",
    ]


def test_simulate_for_user_uses_unshare_worker(monkeypatch):
    daemon = _make_daemon()

    def fake_run(cmd, input, text, capture_output, timeout):
        assert cmd[0] == "unshare"
        assert "aish.security.sandbox_worker" in cmd
        payload = json.loads(input)
        assert payload["command"] == "echo ok"

        return subprocess.CompletedProcess(
            cmd,
            0,
            stdout=json.dumps(
                {
                    "ok": True,
                    "result": {
                        "exit_code": 0,
                        "stdout": "ok",
                        "stderr": "",
                        "changes": [{"path": "tmp/x", "kind": "modified"}],
                    },
                },
                ensure_ascii=False,
            ),
            stderr="",
        )

    monkeypatch.setattr("subprocess.run", fake_run)

    result = daemon._simulate_for_user(
        command="echo ok",
        cwd=Path("/"),
        repo_root=Path("/"),
        uid=1000,
        gid=1000,
        timeout_s=30.0,
    )

    assert result.exit_code == 0
    assert result.stdout == "ok"
    assert result.changes and result.changes[0].path == "tmp/x"


def test_simulate_for_user_maps_worker_error(monkeypatch):
    daemon = _make_daemon()

    def fake_run(cmd, input, text, capture_output, timeout):
        return subprocess.CompletedProcess(
            cmd,
            0,
            stdout=json.dumps(
                {
                    "ok": False,
                    "reason": "overlay_mount_failed",
                    "error": "lowerdir=/tmp: operation not permitted",
                },
                ensure_ascii=False,
            ),
            stderr="",
        )

    monkeypatch.setattr("subprocess.run", fake_run)

    with pytest.raises(SandboxUnavailableError) as exc_info:
        daemon._simulate_for_user(
            command="sudo apt update",
            cwd=Path("/"),
            repo_root=Path("/"),
            uid=1000,
            gid=1000,
            timeout_s=30.0,
        )

    assert exc_info.value.reason == "overlay_mount_failed"
    assert "lowerdir=/tmp" in str(exc_info.value)
