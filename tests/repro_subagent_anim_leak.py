#!/usr/bin/env python3
"""Reproduce/verify the sub_agent_animation leak bug.

Spawns aish in a PTY (stdout+stderr merged), triggers a sub-agent via an
AI prompt, sends Ctrl+C while the sub-agent is thinking, drains all
buffered output, then checks whether NEW animation frames keep arriving.

- Bug:   the animation thread keeps running; new "思考中" frames flow.
- Fixed: the animation thread is stopped; no new frames after drain.

Usage:
    AISH_BIN=target/debug/aish python3 tests/repro_subagent_anim_leak.py
"""

import fcntl
import os
import re
import select
import struct
import subprocess
import sys
import tempfile
import termios
import time

import pty

# UTF-8 bytes for "思考中" (think indicator).
THINKING_BYTES = "\u601d\u8003\u4e2d".encode("utf-8")


def read_available(fd: int, timeout: float = 0.5) -> bytes:
    data = b""
    end = time.time() + timeout
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.1)
        if r:
            try:
                chunk = os.read(fd, 65536)
                if chunk:
                    data += chunk
            except OSError:
                break
    return data


def main() -> int:
    aish_bin = os.environ.get("AISH_BIN", "target/debug/aish")
    if not os.path.isfile(aish_bin):
        print(f"aish binary not found: {aish_bin}")
        return 2

    real_config_home = os.path.expanduser("~/.config")
    tmp_config = tempfile.mkdtemp(prefix="aish-repro-")
    dst = os.path.join(tmp_config, "aish")
    if not os.path.exists(dst):
        os.symlink(os.path.join(real_config_home, "aish"), dst)

    env = os.environ.copy()
    env["XDG_CONFIG_HOME"] = tmp_config
    env["TERM"] = "xterm-256color"
    env["RUST_LOG"] = "warn"

    master, slave = pty.openpty()
    winsize = struct.pack("HHHH", 40, 120, 0, 0)
    fcntl.ioctl(master, termios.TIOCSWINSZ, winsize)

    proc = subprocess.Popen(
        [aish_bin],
        stdin=slave,
        stdout=slave,
        stderr=slave,
        env=env,
        close_fds=True,
    )
    os.close(slave)
    flags = fcntl.fcntl(master, fcntl.F_GETFL)
    fcntl.fcntl(master, fcntl.F_SETFL, flags | os.O_NONBLOCK)

    try:
        # Wait for prompt.
        end = time.time() + 20
        while time.time() < end:
            data = read_available(master, 1.0)
            text = data.decode("utf-8", errors="replace")
            if "\u25cf" in text and "->" in text:
                print("--- aish prompt reached ---")
                break
        else:
            print("--- timeout waiting for prompt ---")
            return 4

        # Send AI prompt that triggers explore sub-agent (long-running task
        # so the animation thread has time to build up frames).
        prompt = (
            "; Please use the explore sub-agent to do a deep dive into "
            "the entire workspace structure, read all Cargo.toml files, "
            "and provide a comprehensive summary of every crate and its "
            "dependencies. This requires thorough exploration.\r"
        )
        os.write(master, prompt.encode())
        print("--- AI prompt sent ---")

        # Wait for sub-agent thinking line.
        end = time.time() + 30
        saw_sub_agent = False
        while time.time() < end:
            data = read_available(master, 2.0)
            if b"explore" in data and THINKING_BYTES in data:
                saw_sub_agent = True
                print("--- sub-agent thinking line detected ---")
                break
        else:
            print("--- sub-agent not triggered; skipping ---")
            os.write(master, b"\x03\x03")
            return 77

        # Let animation build up (3 s ensures the sub-agent's tool loop is
        # actively running and emitting "思考中" frames).
        time.sleep(3.0)
        pre_ctrl_c = read_available(master, 0.5)
        # Confirm the sub-agent animation is actually emitting frames.
        if THINKING_BYTES not in pre_ctrl_c:
            print(
                "--- sub-agent animation not emitting frames before "
                "Ctrl+C; inconclusive ---"
            )
            return 77
        print("--- thinking frames confirmed before Ctrl+C ---")

        # Send Ctrl+C.
        os.write(master, b"\x03")
        print("--- Ctrl+C sent ---")
        # Phase 1: drain ALL buffered output (3 s quiet period).
        end = time.time() + 20
        last_data = time.time()
        drained = b""
        while time.time() < end:
            data = read_available(master, 0.5)
            drained += data
            if data:
                last_data = time.time()
            elif time.time() - last_data > 3.0:
                break
        print(f"--- drained {len(drained)} bytes ---")

        # Phase 2: check for NEW output (animation still running?).
        new_data = b""
        end = time.time() + 5.0
        while time.time() < end:
            data = read_available(master, 0.5)
            new_data += data

        # Verify the aish process is still alive (the leak is a
        # background thread, not a crash).
        if proc.poll() is not None:
            print("--- aish process exited unexpectedly ---")
            return 3

        # The key signal: new "思考中" frames after drain.
        new_text = new_data.decode("utf-8", errors="replace")
        new_thinking_frames = new_text.count(THINKING_BYTES.decode("utf-8"))
        new_timers = re.findall(r"(\d+\.\d+)s", new_text)
        print(
            f"--- new output after drain: {len(new_data)} bytes, "
            f"{new_thinking_frames} thinking frames, "
            f"{len(new_timers)} timer frames ---"
        )

        if new_thinking_frames > 0:
            print(
                "\n--- BUG REPRODUCED: animation still running after "
                "Ctrl+C ---"
            )
            return 1
        else:
            print(
                "\n--- FIX VERIFIED: animation stopped after Ctrl+C ---"
            )
            return 0

    finally:
        cleanup_proc(proc, master)


def cleanup_proc(proc: subprocess.Popen, master: int) -> None:
    """Ensure the child process and PTY are cleaned up."""
    try:
        os.write(master, b"\x03\x03")
    except OSError:
        pass
    try:
        proc.terminate()
        proc.wait(timeout=5)
    except Exception:
        try:
            proc.kill()
            proc.wait(timeout=5)
        except Exception:
            pass
    try:
        os.close(master)
    except OSError:
        pass


if __name__ == "__main__":
    sys.exit(main())
