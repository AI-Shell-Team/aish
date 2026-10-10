#!/usr/bin/env python3
"""Regression checks for AI-Shell-Team/aish issue #591.

Enter used to only *switch* the model: the picker looped back into the panel
instead of returning, so the success message was followed by a fresh list that
kept eating keystrokes (typing went into the search box, ``No matches``) until
the user pressed Esc. These checks pin the contract from the issue:

1. switching to another model closes the picker and gives the shell back
2. re-selecting the current model closes the picker too
3. Esc still cancels without changing the model
4. the direct ``/model <name>`` form still switches in one shot

The session runs inside tmux (a real terminal emulator, so the line editor's
cursor queries and the picker's full-screen panel behave as they do for a user)
with an isolated config that points at a loopback OpenAI-compatible mock that
offers ``model-a`` and ``model-b``. Inline completion is off, each run gets its
own tmux socket, and no daemon is left behind.

Usage:
    AISH_BIN=target/debug/aish python3 tests/repro_591_model_picker.py

Environment:
    AISH_BIN       binary under test (default ``target/debug/aish``)
    AISH_591_DAEMON=1  rerun the checks with ``pty_daemon_enabled: true``
    AISH_591_PORT  mock endpoint port (default 38813)
"""

import http.server
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import time

# A per-run socket keeps concurrent/leftover servers from feeding this run a
# stale session; AISH_591_PORT allows overriding the mock's fixed port.
SOCK = f"s591repro{os.getpid()}"
PORT = int(os.environ.get("AISH_591_PORT", "38813"))
BIN = os.path.abspath(os.environ.get("AISH_BIN", "target/debug/aish"))
PICKER_TITLE = "Switch model / account"
MARKER = "BACK591"
# `AISH_591_DAEMON=1` reruns the checks against a daemon-backed PTY session
# (the issue was only reported against the non-daemon path).
DAEMON = os.environ.get("AISH_591_DAEMON") == "1"


class MockModels(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = json.dumps(
            {"object": "list", "data": [{"id": "model-a"}, {"id": "model-b"}]}
        ).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


def tmux(*args, check=True):
    return subprocess.run(
        ["tmux", "-L", SOCK, *args], capture_output=True, text=True, check=check
    ).stdout


class Session:
    def __init__(self):
        tmux("kill-server", check=False)
        self.root = tempfile.mkdtemp(prefix="aish-591-")
        config_dir = os.path.join(self.root, "aish")
        os.makedirs(config_dir, exist_ok=True)
        self.config = os.path.join(config_dir, "config.yaml")
        with open(self.config, "w", encoding="utf-8") as handle:
            handle.write(
                "model: model-a\n"
                "api_key: test-key\n"
                f"api_base: http://127.0.0.1:{PORT}/v1\n"
                f"pty_daemon_enabled: {'true' if DAEMON else 'false'}\n"
                "check_update_on_startup: false\n"
                "inline_completion:\n"
                "  enabled: false\n"
            )
        tmux(
            "new-session", "-d", "-s", "aish", "-x", "100", "-y", "40",
            "-e", f"XDG_CONFIG_HOME={self.root}",
            "-e", "LANG=en_US.UTF-8", "-e", "LANGUAGE=en_US",
            "-e", "TERM=xterm-256color",
            BIN,
        )
        if not self.wait_for("➜", timeout=25):
            raise RuntimeError(f"shell prompt never appeared; pane:\n{self.capture()}")

    def capture(self):
        # The pane can disappear (aish exited) mid-check; that is a failure to
        # report, not a crash of the script.
        return tmux("capture-pane", "-p", "-t", "aish", check=False)

    def wait_for(self, needle, timeout=15.0):
        end = time.time() + timeout
        while time.time() < end:
            if needle in self.capture():
                return True
            time.sleep(0.3)
        return False

    def type_literal(self, text, per_char=0.12):
        for ch in text:
            tmux("send-keys", "-t", "aish", "-l", ch, check=False)
            time.sleep(per_char)

    def key(self, *names, delay=0.5):
        tmux("send-keys", "-t", "aish", *names, check=False)
        time.sleep(delay)

    def open_picker(self, attempts=3):
        """/model through the completion popup, retrying if the keystrokes raced
        it: the first Enter may reach the shell before the popup renders, which
        would execute the command and leave the second Enter for the panel."""
        for _ in range(attempts):
            self.type_literal("/model")
            self.key("Enter")
            if self.wait_for(PICKER_TITLE, timeout=6):
                return True
            self.key("Enter", delay=2.0)
            if self.wait_for(PICKER_TITLE, timeout=6):
                return True
            self.key("Escape", delay=0.8)
            self.key("C-u", delay=0.5)
        return False

    def reset_shell(self):
        """Leave any panel and clear the input line before the next check."""
        self.key("Escape", delay=0.6)
        self.key("C-u", delay=0.3)

    def shell_takes_input(self):
        """True only when the shell (not a panel) consumes the keystrokes."""
        self.type_literal(f"echo {MARKER}")
        self.key("Enter", delay=1.5)
        return self.wait_for(MARKER, timeout=8.0)

    def config_model(self):
        with open(self.config, encoding="utf-8") as handle:
            for line in handle:
                if line.startswith("model:"):
                    return line.split(":", 1)[1].strip()
        return None

    def close(self):
        tmux("kill-server", check=False)
        shutil.rmtree(self.root, ignore_errors=True)


def check_switch_closes_picker(session, report):
    session.reset_shell()
    if not session.open_picker():
        report("picker did not open after /model")
        return False
    session.key("Down")
    session.key("Enter", delay=2.0)
    if not session.wait_for("Switched to model: model-b", timeout=8):
        report("no switch confirmation")
        return False
    if session.wait_for(PICKER_TITLE, timeout=3):
        report("picker reopened after the switch")
        return False
    if not session.shell_takes_input():
        report("shell did not take the keyboard back")
        return False
    if session.config_model() != "model-b":
        report(f"config model is {session.config_model()!r}, expected model-b")
        return False
    return True


def check_current_model_closes_picker(session, report):
    session.reset_shell()
    if not session.open_picker():
        report("picker did not open after /model")
        return False
    # The highlighted row is the current model.
    session.key("Enter", delay=2.0)
    if session.wait_for(PICKER_TITLE, timeout=3):
        report("picker reopened after re-selecting the current model")
        return False
    return session.shell_takes_input()


def check_esc_cancels(session, report):
    session.reset_shell()
    if not session.open_picker():
        report("picker did not open after /model")
        return False
    session.key("Escape", delay=1.5)
    if session.wait_for(PICKER_TITLE, timeout=3):
        report("picker survived Esc")
        return False
    if not session.shell_takes_input():
        report("shell did not take the keyboard back after Esc")
        return False
    if session.config_model() != "model-b":
        report(f"Esc changed the model to {session.config_model()!r}")
        return False
    return True


def check_direct_command(session, report):
    session.reset_shell()
    session.type_literal("/model model-b")
    session.key("Enter")
    session.key("Enter", delay=2.0)
    if not session.wait_for("Switched to model: model-b", timeout=8):
        report("direct /model model-b did not switch")
        return False
    if session.wait_for(PICKER_TITLE, timeout=3):
        report("direct /model opened the picker")
        return False
    return session.shell_takes_input()


def main():
    if shutil.which("tmux") is None:
        print("tmux is required for these checks")
        return 2
    if not os.path.exists(BIN):
        print(f"binary not found: {BIN} (build it or set AISH_BIN)")
        return 2

    server = http.server.ThreadingHTTPServer(("127.0.0.1", PORT), MockModels)
    threading.Thread(target=server.serve_forever, daemon=True).start()

    reasons = []

    def report(message):
        reasons.append(message)
        print(f"  ! {message}")

    checks = (
        ("switching to another model closes the picker", check_switch_closes_picker),
        ("re-selecting the current model closes the picker",
         check_current_model_closes_picker),
        ("Esc cancels without switching", check_esc_cancels),
        ("direct /model <name> still switches", check_direct_command),
    )
    results = {}
    session = Session()
    try:
        for name, check in checks:
            print(f"[{name}]")
            try:
                results[name] = check(session, report)
            except Exception as exc:  # noqa: BLE001
                report(f"exception: {exc!r}")
                results[name] = False
    finally:
        session.close()
        server.shutdown()

    failures = [name for name, ok in results.items() if not ok]
    print(f"\n{len(results) - len(failures)}/{len(results)} checks passed")
    for name in failures:
        print(f"  FAIL  {name}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
