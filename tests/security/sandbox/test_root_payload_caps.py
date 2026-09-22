from __future__ import annotations

import subprocess
from pathlib import Path

from aish.security.sandbox import SandboxConfig, SandboxExecutor


def _capture_bwrap(monkeypatch, **kwargs):
    captured: dict[str, list[str]] = {}

    def fake_run(cmd, **_kwargs):
        captured["cmd"] = list(cmd)
        return subprocess.CompletedProcess(cmd, 0, stdout="", stderr="")

    monkeypatch.setattr("aish.security.sandbox.run_cmd", fake_run)
    executor = SandboxExecutor(SandboxConfig(repo_root=Path("/")))
    executor._run_in_bubblewrap(
        lower_root=Path("/"),
        upperdir=Path("/tmp/upper"),
        workdir=Path("/tmp/work"),
        work_subdir=Path("/"),
        command="id",
        **kwargs,
    )
    return captured["cmd"]


def test_root_payload_uses_cap_whitelist(monkeypatch):
    cmd = _capture_bwrap(monkeypatch)
    drop_at = cmd.index("--cap-drop")
    add_at = cmd.index("--cap-add")
    assert drop_at < add_at
    assert cmd[drop_at + 1] == "ALL"
    assert cmd[add_at + 1] == "CAP_DAC_OVERRIDE"
    assert "CAP_FOWNER" in cmd
    assert "CAP_CHOWN" in cmd
    assert "CAP_FSETID" in cmd
    assert "CAP_DAC_READ_SEARCH" not in cmd
    assert "--inh-caps=-all" not in cmd
    assert cmd[cmd.index("bash") :] == ["bash", "-lc", "id"]


def test_user_payload_still_drops_inheritable_caps(monkeypatch):
    cmd = _capture_bwrap(monkeypatch, run_as_uid=1000, run_as_gid=1000)
    assert "--cap-drop" not in cmd
    assert "--inh-caps=-all" in cmd
    assert cmd[cmd.index("setpriv") : cmd.index("bash")] == [
        "setpriv",
        "--reuid",
        "1000",
        "--regid",
        "1000",
        "--clear-groups",
        "--inh-caps=-all",
    ]
