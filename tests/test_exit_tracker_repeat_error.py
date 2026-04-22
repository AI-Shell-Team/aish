"""Test command-state error hint behavior for repeated commands and backend commands."""

import pytest

from aish.terminal.pty.command_state import CommandState
from aish.terminal.pty.control_protocol import BackendControlEvent


def _command_started(
    command: str,
    command_seq: int | None = None,
    *,
    submission_id: str | None = None,
) -> BackendControlEvent:
    payload = {"command": command}
    if submission_id is not None:
        payload["submission_id"] = submission_id
    if command_seq is not None:
        payload["command_seq"] = command_seq
    return BackendControlEvent(version=1, type="command_started", ts=1, payload=payload)


def _prompt_ready(
    exit_code: int,
    command_seq: int | None = None,
    *,
    interrupted: bool = False,
    submission_id: str | None = None,
) -> BackendControlEvent:
    payload = {"exit_code": exit_code, "interrupted": interrupted}
    if submission_id is not None:
        payload["submission_id"] = submission_id
    if command_seq is not None:
        payload["command_seq"] = command_seq
    return BackendControlEvent(version=1, type="prompt_ready", ts=2, payload=payload)


@pytest.mark.timeout(5)
def test_same_command_failure_retriggers_after_consume():
    """Same command failing twice (with new execution) should show hint both times."""
    tracker = CommandState()

    tracker.register_user_command("ls-a")
    tracker.handle_backend_event(_command_started("ls-a"))
    tracker.handle_backend_event(_prompt_ready(127))
    error1 = tracker.consume_error()
    assert error1 == ("ls-a", 127)

    tracker.register_user_command("ls-a")
    tracker.handle_backend_event(_command_started("ls-a"))
    tracker.handle_backend_event(_prompt_ready(127))
    error2 = tracker.consume_error()
    assert error2 == ("ls-a", 127)


@pytest.mark.timeout(5)
def test_prompt_redraw_no_duplicate_hint():
    """Prompt redraw (same marker, no set_last_command) must not re-trigger hint."""
    tracker = CommandState()

    tracker.register_user_command("bad")
    tracker.handle_backend_event(_command_started("bad"))
    tracker.handle_backend_event(_prompt_ready(1))
    assert tracker.consume_error() == ("bad", 1)

    tracker.handle_backend_event(_prompt_ready(1))
    assert tracker.consume_error() is None


@pytest.mark.timeout(5)
def test_backend_command_no_error_hint():
    """Commands from bash_exec/AI tools should NOT trigger error hints."""
    tracker = CommandState()

    tracker.register_backend_command("rm -rf /protected", command_seq=-1)
    tracker.handle_backend_event(_command_started("rm -rf /protected", command_seq=-1))
    tracker.handle_backend_event(_prompt_ready(1, command_seq=-1))
    assert tracker.consume_error() is None


@pytest.mark.timeout(5)
def test_backend_error_prompt_redraw_no_hint():
    """After backend command fails, prompt redraws must NOT show error hint.

    Regression: _suppress_error was cleared after first marker processing,
    so prompt redraws re-triggered the hint via _error_hint_shown still being False.
    """
    tracker = CommandState()

    # Backend command fails
    tracker.register_backend_command("ipaw", command_seq=-1)
    tracker.handle_backend_event(_command_started("ipaw", command_seq=-1))
    tracker.handle_backend_event(_prompt_ready(127, command_seq=-1))

    # Prompt redraw sends same marker — must NOT show hint
    tracker.handle_backend_event(_prompt_ready(127, command_seq=-1))
    assert tracker.consume_error() is None

    # Another redraw — still no hint
    tracker.handle_backend_event(_prompt_ready(127, command_seq=-1))
    assert tracker.consume_error() is None


@pytest.mark.timeout(5)
def test_backend_error_does_not_affect_next_user_command():
    """After a backend command fails, next user command failure should show hint."""
    tracker = CommandState()

    tracker.register_backend_command("bad-ai-cmd", command_seq=-1)
    tracker.handle_backend_event(_command_started("bad-ai-cmd", command_seq=-1))
    tracker.handle_backend_event(_prompt_ready(1, command_seq=-1))

    # User command fails — should show hint
    tracker.register_user_command("user-cmd")
    tracker.handle_backend_event(_command_started("user-cmd"))
    tracker.handle_backend_event(_prompt_ready(1))
    assert tracker.consume_error() == ("user-cmd", 1)


@pytest.mark.timeout(5)
def test_up_arrow_reexecution_shows_hint():
    """Re-running the same user command should trigger a fresh hint."""
    tracker = CommandState()

    tracker.register_user_command("ls-a")
    tracker.handle_backend_event(_command_started("ls-a"))
    tracker.handle_backend_event(_prompt_ready(127))
    tracker.consume_error()

    tracker.register_user_command("ls-a")
    tracker.handle_backend_event(_command_started("ls-a"))
    tracker.handle_backend_event(_prompt_ready(127))
    assert tracker.consume_error() == ("ls-a", 127)


@pytest.mark.timeout(5)
def test_successful_command_clears_error():
    """A successful command after an error should clear the error state."""
    tracker = CommandState()

    tracker.register_user_command("bad")
    tracker.handle_backend_event(_command_started("bad"))
    tracker.handle_backend_event(_prompt_ready(1))
    tracker.consume_error()

    tracker.register_user_command("good")
    tracker.handle_backend_event(_command_started("good"))
    tracker.handle_backend_event(_prompt_ready(0))
    assert tracker.consume_error() is None


@pytest.mark.timeout(5)
def test_interrupted_command_does_not_offer_error_correction():
    """Interrupted commands should not produce an error-correction hint."""
    tracker = CommandState()

    tracker.register_user_command("sleep 5")
    tracker.handle_backend_event(_command_started("sleep 5"))
    tracker.handle_backend_event(_prompt_ready(130, interrupted=True))
    assert tracker.consume_error() is None


@pytest.mark.timeout(5)
def test_interactive_ssh_session_exit_does_not_offer_error_correction():
    """Interactive ssh exits should not be treated as shell-command failures."""
    tracker = CommandState()

    tracker.register_user_command("ssh root@example.com")
    tracker.handle_backend_event(_command_started("ssh root@example.com"))
    tracker.handle_backend_event(_prompt_ready(255))

    assert tracker.consume_error() is None
    assert tracker.can_correct_last_error is False


@pytest.mark.timeout(5)
def test_ssh_remote_command_failure_still_offers_error_correction():
    """Non-interactive ssh remote commands should still behave like normal failures."""
    tracker = CommandState()

    tracker.register_user_command("ssh root@example.com ls /missing")
    tracker.handle_backend_event(_command_started("ssh root@example.com ls /missing"))
    tracker.handle_backend_event(_prompt_ready(2))

    assert tracker.consume_error() == ("ssh root@example.com ls /missing", 2)
    assert tracker.can_correct_last_error is True


@pytest.mark.timeout(5)
def test_user_command_keeps_original_text_when_bash_expands_alias():
    """User history should preserve submitted text instead of expanded BASH_COMMAND."""
    tracker = CommandState()

    tracker.register_command("ls", source="user", command_seq=7)
    tracker.handle_backend_event(_command_started("ls --color=auto", command_seq=7))
    tracker.handle_backend_event(_prompt_ready(0, command_seq=7))

    assert tracker.last_command == "ls"
    assert tracker.last_result is not None
    assert tracker.last_result.command == "ls"
