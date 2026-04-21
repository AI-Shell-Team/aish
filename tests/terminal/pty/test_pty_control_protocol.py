from __future__ import annotations

import os
import select
import time
from pathlib import Path

from aish.terminal.pty.control_protocol import decode_control_chunk
from aish.terminal.pty.manager import PTYManager


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


def test_pty_manager_emits_control_events_for_user_command():
    manager = PTYManager(use_output_thread=False)

    try:
        manager.start()
        manager.register_user_command("echo hello")
        manager.send(b"echo hello\n")

        saw_started = False
        saw_ready = False
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
                        if event.type == "prompt_ready":
                            saw_ready = event.payload.get("exit_code") == 0
                            assert result is not None
                            assert result.command == "echo hello"
                else:
                    continue

        assert saw_started is True
        assert saw_ready is True
        assert manager.last_command == "echo hello"
        assert manager.last_exit_code == 0
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


def test_bash_history_hides_internal_command_seq_prefix_for_user_commands(tmp_path: Path):
    histfile = tmp_path / "bash_history"
    manager = PTYManager(use_output_thread=False, env={"HISTFILE": str(histfile)})

    try:
        manager.start()
        manager.send_command("echo hello", command_seq=7, source="user")

        deadline = time.monotonic() + 5.0
        while time.monotonic() < deadline:
            ready, _, _ = select.select([manager.control_fd, manager._master_fd], [], [], 0.1)
            saw_prompt = False
            for fd in ready:
                data = os.read(fd, 4096)
                if not data:
                    continue
                if fd == manager.control_fd:
                    events, errors = manager.decode_control_events(data)
                    assert errors == []
                    for event in events:
                        manager.handle_backend_event(event)
                        if event.type == "prompt_ready":
                            saw_prompt = True
            if saw_prompt:
                break

        while True:
            ready, _, _ = select.select([manager._master_fd], [], [], 0)
            if not ready:
                break
            os.read(manager._master_fd, 4096)

        output, exit_code = manager.execute_command("history | tail -n 5")
        history_lines = [
            line for line in output.splitlines() if line.lstrip()[:1].isdigit()
        ]

        assert exit_code == 0
        assert history_lines == ["    1  echo hello"]
        assert all("__AISH_ACTIVE_COMMAND_SEQ" not in line for line in history_lines)
        assert all("history | tail -n 5" not in line for line in history_lines)
    finally:
        manager.stop()