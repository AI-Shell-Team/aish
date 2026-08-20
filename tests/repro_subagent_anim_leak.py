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
import termios
import time

import pty


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
    tmp_config = os.environ.get("XDG_CONFIG_HOME", "/tmp/aish-repro-config")
    os.makedirs(tmp_config, exist_ok=True)
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
            if "●" in text and "->" in text:
                print("--- aish prompt reached ---")
                break
        else:
            print("--- timeout waiting for prompt ---")
            return 4

        # Send AI prompt that triggers explore sub-agent (long-running task
        # so the animation thread has time to build up frames).
        prompt = ("; Please use the explore sub-agent to do a deep dive into "
                  "the entire workspace structure, read all Cargo.toml files, "
                  "and provide a comprehensive summary of every crate and its "
                  "dependencies. This requires thorough exploration.\r")
        os.write(master, prompt.encode())
        print("--- AI prompt sent ---")

        # Wait for sub-agent thinking line.
        end = time.time() + 30
        saw_sub_agent = False
        while time.time() < end:
            data = read_available(master, 2.0)
            if b"explore" in data and b"\xe6\x80\x9d\xe8\x80\x83" in data:
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
        read_available(master, 0.5)

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

        new_text = new_data.decode("utf-8", errors="replace")
        new_timers = re.findall(r"(\d+\.\d+)s", new_text)
        print(f"--- new output after drain: {len(new_data)} bytes, {len(new_timers)} timer frames ---")

        if len(new_data) > 100 or len(new_timers) > 0:
            print("\n--- BUG REPRODUCED: animation still running after Ctrl+C ---")
            return 1
        else:
            print("\n--- FIX VERIFIED: animation stopped after Ctrl+C ---")
            return 0

    finally:
        try:
            os.write(master, b"\x03\x03")
            proc.terminate()
            proc.wait(timeout=5)
        except Exception:
            pass
        os.close(master)


if __name__ == "__main__":
    sys.exit(main())
