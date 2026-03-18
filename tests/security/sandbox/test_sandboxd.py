from __future__ import annotations

from aish import sandboxd
from aish.security import sandbox_worker


def test_sandboxd_main_dispatches_internal_worker(monkeypatch):
    monkeypatch.setattr(sandbox_worker, "main", lambda: 17)

    assert sandboxd.main(["--sandbox-worker"]) == 17
