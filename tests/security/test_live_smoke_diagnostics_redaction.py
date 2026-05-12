from __future__ import annotations

import importlib.util
import sys
from pathlib import Path


def _load_live_smoke_conftest():
    conftest_path = Path(__file__).parents[1] / "flows" / "live_smoke" / "conftest.py"
    spec = importlib.util.spec_from_file_location("live_smoke_conftest", conftest_path)
    assert spec is not None
    assert spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def test_live_smoke_artifact_redacts_api_key_from_argv_and_text(monkeypatch):
    live_smoke_conftest = _load_live_smoke_conftest()
    monkeypatch.setenv("AISH_LIVE_SMOKE_API_KEY", "LIVE_SECRET_VALUE")

    artifact = {
        "argv": [
            "aish",
            "check-tool-support",
            "--api-key",
            "LIVE_SECRET_VALUE",
            "--api-key=LIVE_SECRET_VALUE",
        ],
        "result": live_smoke_conftest.LiveSmokeCommandResult(
            argv=["aish", "--api-key", "LIVE_SECRET_VALUE"],
            cwd="/workspace",
            returncode=1,
            stdout="stdout LIVE_SECRET_VALUE",
            stderr="stderr LIVE_SECRET_VALUE",
            duration_seconds=0.1,
        ),
    }

    redacted = live_smoke_conftest._redact_live_smoke_artifact(artifact)

    assert "LIVE_SECRET_VALUE" not in repr(redacted)
    assert redacted["argv"] == [
        "aish",
        "check-tool-support",
        "--api-key",
        "<redacted>",
        "--api-key=<redacted>",
    ]
    assert redacted["result"]["argv"] == ["aish", "--api-key", "<redacted>"]
    assert redacted["result"]["stdout"] == "stdout <redacted>"
    assert redacted["result"]["stderr"] == "stderr <redacted>"
