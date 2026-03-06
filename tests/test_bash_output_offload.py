import json
import re
from pathlib import Path
from unittest.mock import patch

import pytest

from aish.config import BashOutputOffloadSettings
from aish.security.security_manager import SecurityDecision
from aish.security.security_policy import RiskLevel
from aish.tools.code_exec import BashTool


def _allow_decision() -> SecurityDecision:
    return SecurityDecision(
        level=RiskLevel.LOW,
        allow=True,
        require_confirmation=False,
        analysis={"risk_level": "LOW", "reasons": []},
    )


def _extract_tag(xml_text: str, tag_name: str) -> str:
    match = re.search(rf"<{tag_name}>(.*?)</{tag_name}>", xml_text, flags=re.S)
    assert match is not None
    return match.group(1).strip("\n")


@pytest.mark.asyncio
async def test_bash_exec_returns_inline_xml_when_below_threshold():
    tool = BashTool(
        offload_settings=BashOutputOffloadSettings(
            enabled=True,
            threshold_bytes=1024,
            preview_bytes=1024,
        )
    )

    with (
        patch.object(tool.security_manager, "decide", return_value=_allow_decision()),
        patch.object(
            tool.executor,
            "execute",
            return_value=(True, "a < b & c", "stderr > out", 0, {}),
        ),
    ):
        result = await tool("echo test")

    assert result.ok is True
    assert result.output.find("<stdout>") < result.output.find("<stderr>")
    assert result.output.find("<stderr>") < result.output.find("<return_code>")
    assert result.output.find("<return_code>") < result.output.find("<offload>")

    assert _extract_tag(result.output, "stdout") == "a < b & c"
    assert _extract_tag(result.output, "stderr") == "stderr > out"
    assert _extract_tag(result.output, "return_code") == "0"

    offload_payload = json.loads(_extract_tag(result.output, "offload"))
    assert offload_payload == {"status": "inline", "reason": "below_threshold"}


@pytest.mark.asyncio
async def test_bash_exec_offloads_large_output(tmp_path: Path):
    tool = BashTool(
        offload_settings=BashOutputOffloadSettings(
            enabled=True,
            threshold_bytes=10,
            preview_bytes=5,
            base_dir=str(tmp_path),
            write_meta=True,
        )
    )

    with (
        patch.object(tool.security_manager, "decide", return_value=_allow_decision()),
        patch.object(
            tool.executor,
            "execute",
            return_value=(True, "1234567890abcdef", "err", 0, {}),
        ),
    ):
        result = await tool("echo test")

    assert result.ok is True
    assert _extract_tag(result.output, "stdout") == "12345"
    assert _extract_tag(result.output, "stderr") == "err"
    assert _extract_tag(result.output, "return_code") == "0"

    offload_payload = json.loads(_extract_tag(result.output, "offload"))
    assert offload_payload["status"] == "offloaded"
    assert offload_payload["hint"] == "Read offload paths for full output"

    stdout_path = Path(offload_payload["stdout_path"])
    stderr_path = Path(offload_payload["stderr_path"])
    meta_path = Path(offload_payload["meta_path"])

    assert stdout_path.exists()
    assert stderr_path.exists()
    assert meta_path.exists()
    assert stdout_path.read_text(encoding="utf-8") == "1234567890abcdef"
    assert stderr_path.read_text(encoding="utf-8") == "err"


@pytest.mark.asyncio
async def test_bash_exec_logs_when_output_offloaded(caplog, tmp_path: Path):
    tool = BashTool(
        offload_settings=BashOutputOffloadSettings(
            enabled=True,
            threshold_bytes=10,
            preview_bytes=5,
            base_dir=str(tmp_path),
        )
    )

    with (
        patch.object(tool.security_manager, "decide", return_value=_allow_decision()),
        patch.object(
            tool.executor,
            "execute",
            return_value=(True, "1234567890abcdef", "", 0, {}),
        ),
        caplog.at_level("INFO", logger="aish.tools.code_exec"),
    ):
        await tool("echo test")

    assert any(
        "bash_exec output offloaded:" in record.message for record in caplog.records
    )


@pytest.mark.asyncio
async def test_bash_exec_failure_still_uses_xml_and_offload(tmp_path: Path):
    tool = BashTool(
        offload_settings=BashOutputOffloadSettings(
            enabled=True,
            threshold_bytes=8,
            preview_bytes=4,
            base_dir=str(tmp_path),
        )
    )

    with (
        patch.object(tool.security_manager, "decide", return_value=_allow_decision()),
        patch.object(
            tool.executor,
            "execute",
            return_value=(False, "", "abcdefghijklmnop", 2, {}),
        ),
    ):
        result = await tool("bad-command")

    assert result.ok is False
    assert result.code == 2
    assert _extract_tag(result.output, "stdout") == ""
    assert _extract_tag(result.output, "stderr") == "abcd"
    assert _extract_tag(result.output, "return_code") == "2"

    offload_payload = json.loads(_extract_tag(result.output, "offload"))
    assert offload_payload["status"] == "offloaded"


@pytest.mark.asyncio
async def test_bash_exec_offload_failure_returns_failed_status(tmp_path: Path):
    bad_base = tmp_path / "not-a-directory"
    bad_base.write_text("x", encoding="utf-8")

    tool = BashTool(
        offload_settings=BashOutputOffloadSettings(
            enabled=True,
            threshold_bytes=1,
            preview_bytes=3,
            base_dir=str(bad_base),
        )
    )

    with (
        patch.object(tool.security_manager, "decide", return_value=_allow_decision()),
        patch.object(
            tool.executor,
            "execute",
            return_value=(True, "abcdef", "", 0, {}),
        ),
    ):
        result = await tool("echo test")

    assert result.ok is True
    assert _extract_tag(result.output, "stdout") == "abc"
    offload_payload = json.loads(_extract_tag(result.output, "offload"))
    assert offload_payload["status"] == "failed"
    assert offload_payload["hint"] == "Output shown as preview only"


@pytest.mark.asyncio
async def test_bash_exec_logs_when_output_offload_failed(caplog, tmp_path: Path):
    bad_base = tmp_path / "not-a-directory"
    bad_base.write_text("x", encoding="utf-8")

    tool = BashTool(
        offload_settings=BashOutputOffloadSettings(
            enabled=True,
            threshold_bytes=1,
            preview_bytes=3,
            base_dir=str(bad_base),
        )
    )

    with (
        patch.object(tool.security_manager, "decide", return_value=_allow_decision()),
        patch.object(
            tool.executor,
            "execute",
            return_value=(True, "abcdef", "", 0, {}),
        ),
        caplog.at_level("WARNING", logger="aish.tools.code_exec"),
    ):
        await tool("echo test")

    assert any(
        "bash_exec output offload failed:" in record.message
        for record in caplog.records
    )
