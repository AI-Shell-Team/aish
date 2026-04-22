from __future__ import annotations

import os
import select
import time
from pathlib import Path

from aish.terminal.pty.control_protocol import BackendControlEvent
from aish.terminal.pty.control_protocol import decode_control_chunk
from aish.terminal.pty.manager import PTYManager


def _wait_for_prompt_ready(manager: PTYManager, command_seq: int, timeout: float = 5.0) -> bytes:
    output = bytearray()
    deadline = time.monotonic() + timeout
    saw_prompt = False

    while time.monotonic() < deadline:
        ready, _, _ = select.select([manager.control_fd, manager._master_fd], [], [], 0.1)
        for fd in ready:
            data = os.read(fd, 4096)
            if not data:
                continue
            if fd == manager.control_fd:
                events, errors = manager.decode_control_events(data)
                assert errors == []
                for event in events:
                    manager.handle_backend_event(event)
                    if event.type == "prompt_ready" and event.payload.get("command_seq") == command_seq:
                        saw_prompt = True
            else:
                output.extend(data)
        if saw_prompt:
            break

    if not saw_prompt:
        rendered = output.decode("utf-8", errors="replace")
        raise TimeoutError(
            f"timed out waiting for prompt_ready with command_seq={command_seq}: {rendered!r}"
        )

    return bytes(output)


def _drain_master_fd(manager: PTYManager) -> bytes:
    output = bytearray()
    while True:
        ready, _, _ = select.select([manager._master_fd], [], [], 0)
        if not ready:
            break
        output.extend(os.read(manager._master_fd, 4096))
    return bytes(output)


def test_decode_control_chunk_handles_partial_ndjson_frames():
    first = b'{"version":1,"type":"session_ready","ts":1}\n{"version":1'
    second = b',"type":"prompt_ready","ts":2}\n'

    events, remainder, errors = decode_control_chunk(b"", first)

    assert [event.type for event in events] == ["session_ready"]
    assert remainder == b'{"version":1'
    assert errors == []

    events, remainder, errors = decode_control_chunk(remainder, second)

    assert [event.type for event in events] == ["prompt_ready"]
    assert remainder == b""
    assert errors == []


def test_decode_control_chunk_reports_invalid_lines_without_dropping_valid_events():
    chunk = b'{"version":1,"type":"session_ready","ts":1}\nnot-json\n'

    events, remainder, errors = decode_control_chunk(b"", chunk)

    assert [event.type for event in events] == ["session_ready"]
    assert remainder == b""
    assert len(errors) == 1


def test_pty_manager_collects_protocol_decode_errors():
    manager = PTYManager(use_output_thread=False)

    manager._dispatch_control_chunk(b'{"version":1,"type":"session_ready","ts":1}\nnot-json\n')

    issues = manager.consume_protocol_issues()
    assert len(issues) == 1
    assert "invalid control event JSON" in issues[0]
    assert manager.consume_protocol_issues() == ()


def test_pty_manager_surfaces_command_state_protocol_issues():
    manager = PTYManager(use_output_thread=False)

    result = manager.handle_backend_event(
        BackendControlEvent(
            version=1,
            type="prompt_ready",
            ts=1,
            payload={"command_seq": 404, "exit_code": 0},
        )
    )

    assert result is None
    issues = manager.consume_protocol_issues()
    assert len(issues) == 1
    assert "prompt_ready missing matching submission" in issues[0]
    assert manager.consume_protocol_issues() == ()


def test_pty_manager_aggregates_decode_and_state_issues_together():
    manager = PTYManager(use_output_thread=False)

    manager._dispatch_control_chunk(b'{"version":1,"type":"session_ready","ts":1}\nnot-json\n')
    manager.handle_backend_event(
        BackendControlEvent(
            version=1,
            type="prompt_ready",
            ts=2,
            payload={"command_seq": 505, "exit_code": 0},
        )
    )

    issues = manager.consume_protocol_issues()
    assert len(issues) == 2
    assert any("invalid control event JSON" in issue for issue in issues)
    assert any("prompt_ready missing matching submission" in issue for issue in issues)
    assert manager.consume_protocol_issues() == ()


def test_pty_manager_emits_control_events_for_user_command():
    manager = PTYManager(use_output_thread=False)

    try:
        manager.start()
        manager.register_user_command("echo hello")
        manager.send(b"echo hello\n")

        saw_started = False
        saw_ready = False
        started_submission_id = None
        deadline = time.monotonic() + 5.0

        while time.monotonic() < deadline and not (saw_started and saw_ready):
            ready, _, _ = select.select([manager.control_fd, manager._master_fd], [], [], 0.1)
            for fd in ready:
                data = os.read(fd, 4096)
                if not data:
                    continue
                if fd == manager.control_fd:
                    events, errors = manager.decode_control_events(data)
                    assert errors == []
                    for event in events:
                        result = manager.handle_backend_event(event)
                        if event.type == "command_started":
                            saw_started = event.payload.get("command") == "echo hello"
                            started_submission_id = event.payload.get("submission_id")
                            if started_submission_id is not None:
                                assert isinstance(started_submission_id, str)
                                assert started_submission_id.startswith("subm_")
                        if event.type == "prompt_ready" and saw_started:
                            saw_ready = event.payload.get("exit_code") == 0
                            assert result is not None
                            assert result.command == "echo hello"
                            if started_submission_id is not None:
                                assert event.payload.get("submission_id") == started_submission_id
                                assert result.submission_id == started_submission_id
                else:
                    continue

        assert saw_started is True
        assert saw_ready is True
        assert manager.last_command == "echo hello"
        assert manager.last_exit_code == 0
    finally:
        manager.stop()


def test_start_drains_startup_prompt_ready_in_poll_mode():
    manager = PTYManager(use_output_thread=False)

    try:
        manager.start()

        ready, _, _ = select.select([manager.control_fd], [], [], 0.1)
        assert ready == []

        manager.send_command("printf 'ready\\n'", command_seq=11, source="backend")
        output = _wait_for_prompt_ready(manager, 11)
        output += _drain_master_fd(manager)

        assert b"ready" in output
        assert manager.last_command == "printf 'ready\\n'"
        assert manager.last_exit_code == 0
    finally:
        manager.stop()


def test_start_preserves_startup_master_output_in_poll_mode(tmp_path: Path):
    bashrc = tmp_path / ".bashrc"
    bashrc.write_text("printf 'startup-marker\\n'\n", encoding="utf-8")
    manager = PTYManager(use_output_thread=False, env={"HOME": str(tmp_path)})

    try:
        manager.start()

        output = bytearray()
        deadline = time.monotonic() + 1.0
        while time.monotonic() < deadline and b"startup-marker" not in output:
            ready, _, _ = select.select([manager._master_fd], [], [], 0.1)
            if not ready:
                continue
            output.extend(os.read(manager._master_fd, 4096))

        assert b"startup-marker" in output
    finally:
        manager.stop()


def test_pty_manager_execute_command_returns_output_without_marker():
    manager = PTYManager(use_output_thread=False)

    try:
        manager.start()
        output, exit_code = manager.execute_command("printf 'hello\\n'")

        assert exit_code == 0
        assert output == "hello"
        assert "[AISH_EXIT:" not in output
    finally:
        manager.stop()


def test_clean_pty_output_strips_echo_after_leading_blank_line():
    manager = PTYManager(use_output_thread=False)

    cleaned = manager._clean_pty_output(
        b"\r\n printf 'hello\\n'\r\nhello\r\n",
        "printf 'hello\\n'",
    )

    assert cleaned == "hello"


def test_execute_multiline_command_uses_reported_continuation_prompt():
    manager = PTYManager(use_output_thread=False, env={"PS2": "cont> "})

    try:
        manager.start()
        output, exit_code = manager.execute_command("printf 'hello\\n' && \\\nprintf 'world\\n'")

        assert exit_code == 0
        assert output == "hello\nworld"
    finally:
        manager.stop()


def test_pty_manager_execute_command_waits_without_implicit_timeout():
    manager = PTYManager(use_output_thread=False)

    try:
        manager.start()
        output, exit_code = manager.execute_command("printf x; sleep 0.2; printf y")

        assert exit_code == 0
        assert output == "xy"
    finally:
        manager.stop()


def test_pty_manager_execute_command_honors_explicit_timeout():
    manager = PTYManager(use_output_thread=False)

    try:
        manager.start()
        output, exit_code = manager.execute_command(
            "printf 'hello\\n'; sleep 1",
            timeout=0.1,
        )

        assert exit_code == -1
        assert output == "hello"
    finally:
        manager.stop()


def test_bash_history_keeps_user_command_without_internal_prefix(tmp_path: Path):
    histfile = tmp_path / "bash_history"
    manager = PTYManager(use_output_thread=False, env={"HISTFILE": str(histfile)})

    try:
        manager.start()
        manager.send_command("echo hello", command_seq=7, source="user")
        _wait_for_prompt_ready(manager, 7)
        _drain_master_fd(manager)

        output, exit_code = manager.execute_command("history | tail -n 5")
        history_lines = [
            line for line in output.splitlines() if line.lstrip()[:1].isdigit()
        ]

        assert exit_code == 0
        assert len(history_lines) == 1
        assert history_lines[0].endswith("echo hello")
        assert all("__AISH_ACTIVE_COMMAND_SEQ" not in line for line in history_lines)
        assert all("history | tail -n 5" not in line for line in history_lines)
    finally:
        manager.stop()


def test_send_command_emits_matching_submission_id_for_started_and_ready():
    manager = PTYManager(use_output_thread=False)

    try:
        manager.start()
        manager.send_command("echo hello", command_seq=7, source="user")

        started_submission_id = None
        saw_ready = False
        deadline = time.monotonic() + 5.0

        while time.monotonic() < deadline and not saw_ready:
            ready, _, _ = select.select([manager.control_fd, manager._master_fd], [], [], 0.1)
            for fd in ready:
                data = os.read(fd, 4096)
                if not data:
                    continue
                if fd != manager.control_fd:
                    continue

                events, errors = manager.decode_control_events(data)
                assert errors == []
                for event in events:
                    manager.handle_backend_event(event)
                    if event.type == "command_started" and event.payload.get("command_seq") == 7:
                        started_submission_id = event.payload.get("submission_id")
                    if event.type == "prompt_ready" and event.payload.get("command_seq") == 7:
                        assert event.payload.get("submission_id") == started_submission_id
                        saw_ready = True

        assert isinstance(started_submission_id, str)
        assert started_submission_id.startswith("subm_")
        assert saw_ready is True
    finally:
        manager.stop()


def test_bash_history_excludes_backend_commands(tmp_path: Path):
    histfile = tmp_path / "bash_history"
    manager = PTYManager(use_output_thread=False, env={"HISTFILE": str(histfile)})

    try:
        manager.start()
        manager.send_command("echo hello", command_seq=7, source="user")
        _wait_for_prompt_ready(manager, 7)
        _drain_master_fd(manager)

        backend_output, backend_exit_code = manager.execute_command("printf 'backend\\n'")
        assert backend_exit_code == 0
        assert backend_output == "backend"

        output, exit_code = manager.execute_command("history | tail -n 5")
        history_lines = [
            line for line in output.splitlines() if line.lstrip()[:1].isdigit()
        ]

        assert exit_code == 0
        assert len(history_lines) == 1
        assert history_lines[0].endswith("echo hello")
        assert all("printf 'backend" not in line for line in history_lines)
    finally:
        manager.stop()


def test_history_expansion_does_not_leak_internal_metadata(tmp_path: Path):
    histfile = tmp_path / "bash_history"
    manager = PTYManager(use_output_thread=False, env={"HISTFILE": str(histfile)})

    try:
        manager.start()
        manager.send_command("echo hello", command_seq=7, source="user")
        _wait_for_prompt_ready(manager, 7)
        _drain_master_fd(manager)

        manager.send_command("!!", command_seq=8, source="user")
        output = _wait_for_prompt_ready(manager, 8)
        output += _drain_master_fd(manager)

        rendered = output.decode("utf-8", errors="ignore")
        assert "__AISH_ACTIVE_COMMAND_SEQ" not in rendered
        assert "__AISH_ACTIVE_COMMAND_TEXT" not in rendered

        history_output, exit_code = manager.execute_command("history | tail -n 5")
        history_lines = [
            line for line in history_output.splitlines() if line.lstrip()[:1].isdigit()
        ]

        assert exit_code == 0
        assert any(line.endswith("echo hello") for line in history_lines)
        assert any(line.endswith("!!") for line in history_lines)
    finally:
        manager.stop()


def test_pty_manager_execute_command_with_shell_metacharacters_has_no_metadata_leak():
    manager = PTYManager(use_output_thread=False)

    try:
        manager.start()
        output, exit_code = manager.execute_command("printf '%s\\n' 'a|b & c'")

        assert exit_code == 0
        assert output == "a|b & c"
        assert "__AISH_ACTIVE_COMMAND_SEQ" not in output
        assert "__AISH_ACTIVE_COMMAND_TEXT" not in output
    finally:
        manager.stop()


def test_pty_manager_execute_multiline_command_has_no_metadata_leak():
    manager = PTYManager(use_output_thread=False)

    try:
        manager.start()
        output, exit_code = manager.execute_command("printf 'hello\\n' && \\\nprintf 'world\\n'")

        assert exit_code == 0
        assert output == "hello\nworld"
        assert "__AISH_ACTIVE_COMMAND_SEQ" not in output
        assert "__AISH_ACTIVE_COMMAND_TEXT" not in output
    finally:
        manager.stop()


def test_pty_manager_emits_shell_exiting_event_for_exit_command():
    manager = PTYManager(use_output_thread=False)

    try:
        manager.start()
        manager.send_command("exit", command_seq=9, source="user")

        saw_started = False
        saw_exiting = False
        exit_code = None
        deadline = time.monotonic() + 5.0

        while time.monotonic() < deadline and not saw_exiting:
            ready, _, _ = select.select([manager.control_fd, manager._master_fd], [], [], 0.1)
            for fd in ready:
                data = os.read(fd, 4096)
                if not data:
                    continue
                if fd == manager.control_fd:
                    events, errors = manager.decode_control_events(data)
                    assert errors == []
                    for event in events:
                        if event.type == "command_started":
                            saw_started = event.payload.get("command_seq") == 9
                        if event.type == "shell_exiting":
                            saw_exiting = True
                            exit_code = event.payload.get("exit_code")

        assert saw_started is True
        assert saw_exiting is True
        assert exit_code == 0
    finally:
        manager.stop()