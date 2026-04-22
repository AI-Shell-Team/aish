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
def test_register_command_returns_pending_submission_metadata():
    """Registering a command should expose explicit pending-submission metadata."""
    tracker = CommandState()

    submission = tracker.register_backend_command("echo hello", command_seq=-1)

    assert submission is not None
    assert submission.submission_id.startswith("subm_")
    assert submission.command == "echo hello"
    assert submission.source == "backend"
    assert submission.command_seq == -1
    assert submission.status == "submitted"
    assert tracker.pending_submission is submission
    assert tracker.pending_submission_count == 1
    assert tracker.pending_submissions == (submission,)
    assert tracker.get_pending_submission(-1) is submission
    assert tracker.has_pending_submission(-1) is True


@pytest.mark.timeout(5)
def test_pending_submissions_include_id_only_registrations():
    tracker = CommandState()

    first = tracker.register_user_command("echo one")
    second = tracker.register_user_command("echo two")

    assert first is not None
    assert second is not None
    assert tracker.pending_submission_count == 2
    assert {submission.submission_id for submission in tracker.pending_submissions} == {
        first.submission_id,
        second.submission_id,
    }


@pytest.mark.timeout(5)
def test_pending_submission_is_cleared_after_prompt_ready():
    """Pending submissions should be removed once prompt_ready resolves them."""
    tracker = CommandState()

    tracker.register_backend_command("echo hello", command_seq=-1)

    assert tracker.has_pending_submission(-1) is True

    tracker.handle_backend_event(_command_started("echo hello", command_seq=-1))
    pending = tracker.pending_submission
    assert pending is not None
    assert pending.status == "started"
    tracker.handle_backend_event(_prompt_ready(0, command_seq=-1))

    assert tracker.pending_submission is None
    assert tracker.pending_submission_count == 0
    assert tracker.pending_submissions == ()
    assert tracker.get_pending_submission(-1) is None
    assert tracker.has_pending_submission(-1) is False


@pytest.mark.timeout(5)
def test_prompt_ready_without_command_seq_uses_pending_submission_seq():
    """Missing prompt_ready command_seq should still resolve and clear seq-bound submissions."""
    tracker = CommandState()

    tracker.register_backend_command("echo hello", command_seq=-1)
    tracker.handle_backend_event(_command_started("echo hello"))

    result = tracker.handle_backend_event(_prompt_ready(0))

    assert result is not None
    assert result.command == "echo hello"
    assert result.command_seq == -1
    assert result.submission_id is not None
    assert tracker.pending_submission is None
    assert tracker.pending_submission_count == 0


@pytest.mark.timeout(5)
def test_prompt_ready_without_matching_submission_records_protocol_issue():
    tracker = CommandState()

    result = tracker.handle_backend_event(_prompt_ready(0, command_seq=99))

    assert result is None
    issues = tracker.consume_protocol_issues()
    assert len(issues) == 1
    assert "prompt_ready missing matching submission" in issues[0]
    assert tracker.consume_protocol_issues() == ()


@pytest.mark.timeout(5)
def test_command_started_without_matching_submission_records_protocol_issue():
    tracker = CommandState()

    tracker.handle_backend_event(_command_started("echo unexpected", command_seq=42))

    issues = tracker.consume_protocol_issues()
    assert len(issues) == 1
    assert "command_started missing matching submission" in issues[0]


@pytest.mark.timeout(5)
def test_command_started_keeps_original_text_and_tracks_observed_command():
    """Original submitted text stays authoritative while shell-observed text is retained separately."""
    tracker = CommandState()

    submission = tracker.register_command("ls", source="user", command_seq=7)

    tracker.handle_backend_event(_command_started("ls --color=auto"))
    result = tracker.handle_backend_event(_prompt_ready(0))

    assert submission is not None
    assert submission.command == "ls"
    assert submission.observed_command == "ls --color=auto"
    assert result is not None
    assert result.command == "ls"
    assert result.command_seq == 7


@pytest.mark.timeout(5)
def test_submission_id_matches_correct_pending_submission_before_active_fallback():
    tracker = CommandState()

    first = tracker.register_backend_command("echo one", command_seq=-1)
    second = tracker.register_backend_command("echo two", command_seq=-2)

    assert first is not None
    assert second is not None

    tracker.handle_backend_event(
        _command_started(
            "echo one",
            submission_id=first.submission_id,
        )
    )
    result = tracker.handle_backend_event(
        _prompt_ready(
            0,
            submission_id=first.submission_id,
        )
    )

    assert result is not None
    assert result.command == "echo one"
    assert result.command_seq == -1
    assert result.submission_id == first.submission_id
    assert tracker.get_pending_submission(-1) is None
    assert tracker.get_pending_submission(-2) is second
    assert second.status == "submitted"


@pytest.mark.timeout(5)
def test_unmatched_submission_id_does_not_guess_command_seq():
    tracker = CommandState()

    tracker.register_backend_command("echo one", command_seq=-1)
    tracker.register_backend_command("echo two", command_seq=-2)

    assert tracker.resolve_command_seq(None, "subm_missing") is None


@pytest.mark.timeout(5)
def test_conflicting_identifiers_do_not_resolve_or_rebind_submission():
    tracker = CommandState()

    first = tracker.register_backend_command("echo one", command_seq=-1)
    second = tracker.register_backend_command("echo two", command_seq=-2)

    assert first is not None
    assert second is not None

    assert tracker.resolve_command_seq(-2, first.submission_id) is None

    tracker.handle_backend_event(
        _command_started(
            "echo unexpected",
            command_seq=-2,
            submission_id=first.submission_id,
        )
    )

    assert tracker.get_pending_submission(-1) is first
    assert tracker.get_pending_submission(-2) is second
    assert first.status == "submitted"
    assert second.status == "submitted"

    issues = tracker.consume_protocol_issues()
    assert any("conflicting backend identifiers" in issue for issue in issues)


@pytest.mark.timeout(5)
def test_submission_id_can_bind_first_command_seq_when_unclaimed():
    tracker = CommandState()

    submission = tracker.register_user_command("echo one")

    assert submission is not None
    assert submission.command_seq is None

    tracker.handle_backend_event(
        _command_started(
            "echo one",
            command_seq=7,
            submission_id=submission.submission_id,
        )
    )

    assert submission.command_seq == 7
    assert tracker.get_pending_submission(7) is submission


@pytest.mark.timeout(5)
def test_missing_identifiers_do_not_guess_when_multiple_pending_submissions_exist():
    tracker = CommandState()

    first = tracker.register_backend_command("echo one", command_seq=-1)
    second = tracker.register_backend_command("echo two", command_seq=-2)

    result = tracker.handle_backend_event(_prompt_ready(0))

    assert first is not None
    assert second is not None
    assert result is None
    assert tracker.get_pending_submission(-1) is first
    assert tracker.get_pending_submission(-2) is second


@pytest.mark.timeout(5)
def test_submission_id_only_placeholder_stays_backend_sourced():
    tracker = CommandState()

    tracker.handle_backend_event(
        _command_started(
            "echo unexpected",
            submission_id="subm_missing",
        )
    )

    pending = tracker.pending_submission
    assert pending is not None
    assert pending.source == "backend"
    assert pending.allow_error_correction is False

    issues = tracker.consume_protocol_issues()
    assert any("backend event missing matching submission" in issue for issue in issues)


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
