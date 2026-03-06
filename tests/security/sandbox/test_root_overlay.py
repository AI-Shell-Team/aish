from __future__ import annotations

import os
import stat
from pathlib import Path

import pytest

from aish.security.sandbox import (SandboxConfig, SandboxExecutor,
                                   SandboxUnavailableError)


def test_root_overlay_tmp_mount_failure_does_not_fallback(monkeypatch):
    executor = SandboxExecutor(SandboxConfig(repo_root=Path("/")))

    monkeypatch.setattr(executor, "_bind_mount", lambda source, target: None)
    monkeypatch.setattr(executor, "_remount_bind_readonly", lambda target: None)
    monkeypatch.setattr(
        executor, "_list_system_root_overlay_targets", lambda: [Path("/tmp")]
    )

    def fail_mount(*, lowerdir, upperdir, workdir, merged):
        raise SandboxUnavailableError(
            "overlay_mount_failed", details="operation not permitted"
        )

    monkeypatch.setattr(executor, "_mount_overlay", fail_mount)

    def should_not_run(*args, **kwargs):
        raise AssertionError(
            "fallback should not continue to bubblewrap after overlay failure"
        )

    monkeypatch.setattr(executor, "_run_in_bubblewrap", should_not_run)
    monkeypatch.setattr(executor, "_umount", lambda path: None)

    with pytest.raises(SandboxUnavailableError) as exc_info:
        executor._run_in_overlay_sandbox(
            command="echo ok",
            repo_root=Path("/"),
            cwd=Path("/"),
        )

    msg = str(exc_info.value)
    assert "lowerdir=/tmp" in msg


def test_root_overlay_syncs_upper_metadata_from_lower(monkeypatch):
    executor = SandboxExecutor(SandboxConfig(repo_root=Path("/")))

    monkeypatch.setattr(executor, "_bind_mount", lambda source, target: None)
    monkeypatch.setattr(executor, "_remount_bind_readonly", lambda target: None)
    monkeypatch.setattr(
        executor, "_list_system_root_overlay_targets", lambda: [Path("/tmp")]
    )

    observed: dict[str, int] = {}

    def capture_mount(*, lowerdir, upperdir, workdir, merged):
        observed["upper_mode"] = stat.S_IMODE(os.stat(upperdir).st_mode)
        observed["lower_mode"] = stat.S_IMODE(os.stat(lowerdir).st_mode)
        raise SandboxUnavailableError(
            "overlay_mount_failed", details="stop-after-check"
        )

    monkeypatch.setattr(executor, "_mount_overlay", capture_mount)
    monkeypatch.setattr(executor, "_umount", lambda path: None)

    with pytest.raises(SandboxUnavailableError):
        executor._run_in_overlay_sandbox(
            command="echo ok",
            repo_root=Path("/"),
            cwd=Path("/"),
        )

    assert observed["upper_mode"] == observed["lower_mode"]
