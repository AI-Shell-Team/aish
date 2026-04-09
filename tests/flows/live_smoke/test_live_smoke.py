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
        "Reply with exactly one ASCII token formed by joining these parts without spaces: "
        "AISH, _, SMOKE, _, TEST, _, OK."
    )

    result = live_smoke_chat_runner(
        prompt=prompt,
        expected_token=expected_token,
        timeout=120.0,
    )

    assert result.expected_token_seen
    assert not _contains_traceback(result.transcript)


@pytest.mark.live_smoke
def test_ai_can_use_tools_to_create_file_in_workspace(
    live_smoke_paths,
    live_smoke_chat_runner,
):
    output_file = live_smoke_paths.workspace / "live-smoke-ai-task.txt"
    prompt = (
        f"Use available tools to create a file at this exact absolute path: {output_file}. "
        "Write exactly this single line into the file: AISH_AI_TOOL_OK. "
        "Do not write to any other path. After the file is created, briefly tell me it is done."
    )

    result = live_smoke_chat_runner(
        prompt=prompt,
        expected_file=output_file,
        timeout=60.0,
        auto_approve=True,
    )

    assert output_file.exists()
    assert output_file.read_text(encoding="utf-8").strip() == "AISH_AI_TOOL_OK"
    assert not _contains_traceback(result.transcript)