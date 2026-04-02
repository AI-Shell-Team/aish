from __future__ import annotations

import pytest


def _contains_traceback(text: str) -> bool:
    return "Traceback (most recent call last)" in text


@pytest.mark.live_smoke
def test_info_command_starts_cleanly(live_smoke_runner):
    result = live_smoke_runner("info")

    assert result.returncode == 0
    assert "AI Shell" in result.stdout
    assert not _contains_traceback(result.combined_output)


@pytest.mark.live_smoke
def test_check_tool_support_succeeds_with_real_provider(
    live_smoke_provider_config,
    live_smoke_runner,
):
    args = ["check-tool-support", "--model", live_smoke_provider_config.model]
    if live_smoke_provider_config.api_base:
        args.extend(["--api-base", live_smoke_provider_config.api_base])
    args.extend(["--api-key", live_smoke_provider_config.api_key])

    result = live_smoke_runner(*args, timeout=90.0)

    assert result.returncode == 0
    assert not _contains_traceback(result.combined_output)
    assert "error" not in result.stderr.lower()


@pytest.mark.live_smoke
def test_interactive_shell_can_complete_one_live_round_trip(live_smoke_chat_runner):
    expected_token = "AISH_SMOKE_TEST_OK"
    prompt = (
        "Reply with exactly this ASCII token and nothing else: "
        f"{expected_token}"
    )

    result = live_smoke_chat_runner(
        prompt=prompt,
        expected_token=expected_token,
        timeout=120.0,
    )

    assert result.signalstatus is None
    assert result.exitstatus == 0
    assert expected_token in result.transcript
    assert not _contains_traceback(result.transcript)