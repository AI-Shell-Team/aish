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
5. the next AI request is sent with the model chosen in the picker

The session runs inside tmux (a real terminal emulator, so the line editor's
cursor queries and the picker's full-screen panel behave as they do for a user)
with an isolated config that points at a loopback OpenAI-compatible mock that
offers ``model-a`` and ``model-b``. The mock records the model of every
``/chat/completions`` request so check 5 can assert it. Inline completion is
off, each run gets its own tmux socket, and no daemon is left behind.

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
REQUEST_TIMEOUT = 30.0
# Per-invocation deadline for tmux commands; a stalled server must not wedge the
# script (see `tmux()`).
TMUX_TIMEOUT = 30.0
# `AISH_591_DAEMON=1` reruns the checks against a daemon-backed PTY session
# (the issue was only reported against the non-daemon path).
DAEMON = os.environ.get("AISH_591_DAEMON") == "1"


def other_model(current):
    """The mock offers exactly two models; return the one that is not current."""
    return "model-b" if current == "model-a" else "model-a"


class MockModels(http.server.BaseHTTPRequestHandler):
    """Serves the model list and records the model of every chat request."""

    requests = []
    requests_lock = threading.Lock()

    @classmethod
    def reset_requests(cls):
        """Drop the chat requests recorded so far."""
        with cls.requests_lock:
            cls.requests = []

    @classmethod
    def wait_for_request(cls, timeout=REQUEST_TIMEOUT):
        """Return the model of the first recorded chat request, or None."""
        end = time.time() + timeout
        while time.time() < end:
            with cls.requests_lock:
                if cls.requests:
                    return cls.requests[0]
            time.sleep(0.3)
        return None

    def do_GET(self):
        body = json.dumps(
            {"object": "list", "data": [{"id": "model-a"}, {"id": "model-b"}]}
        ).encode()
        self._reply(body, "application/json")

    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length) if length else b""
        try:
            payload = json.loads(raw or b"{}")
        except ValueError:
            payload = {}
        model = payload.get("model")
        with MockModels.requests_lock:
            MockModels.requests.append(model)
        if payload.get("stream"):
            body = self._stream_body(model).encode()
            self._reply(body, "text/event-stream")
        else:
            body = json.dumps(
                {
                    "id": "cmpl-1",
                    "object": "chat.completion",
                    "created": 0,
                    "model": model,
                    "choices": [
                        {
                            "index": 0,
                            "message": {"role": "assistant", "content": "ok"},
                            "finish_reason": "stop",
                        }
                    ],
                    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
                }
            ).encode()
            self._reply(body, "application/json")

    @staticmethod
    def _stream_body(model):
        """Minimal OpenAI SSE stream: one content chunk, then the stop chunk."""
        def chunk(delta, finish_reason):
            return (
                "data: "
                + json.dumps(
                    {
                        "id": "cmpl-1",
                        "object": "chat.completion.chunk",
                        "created": 0,
                        "model": model,
                        "choices": [
                            {"index": 0, "delta": delta, "finish_reason": finish_reason}
                        ],
                    }
                )
                + "\n\n"
            )

        return (
            chunk({"role": "assistant", "content": "ok"}, None)
            + chunk({}, "stop")
            + "data: [DONE]\n\n"
        )

    def _reply(self, body, content_type):
        self.send_response(200)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


def tmux(*args, check=True, timeout=TMUX_TIMEOUT):
    """Run a tmux command against this run's private socket.

    The deadline keeps a wedged tmux server from hanging the whole run: with
    ``check=False`` (pane reads and keystrokes) a timeout is reported as empty
    output, so the owning check fails with a reason instead of blocking, while
    setup commands still raise.
    """
    try:
        return subprocess.run(
            ["tmux", "-L", SOCK, *args],
            capture_output=True,
            text=True,
            check=check,
            timeout=timeout,
        ).stdout
    except subprocess.TimeoutExpired:
        if check:
            raise
        return ""


class Session:
    """aish inside a tmux pane, with helpers to drive it and read the pane."""

    def __init__(self):
        tmux("kill-server", check=False)
        self.probe = 0
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
        """Current pane content; empty once aish has exited."""
        # The pane can disappear (aish exited) mid-check; that is a failure to
        # report, not a crash of the script.
        return tmux("capture-pane", "-p", "-t", "aish", check=False)

    def wait_for(self, needle, timeout=15.0):
        """Wait until the pane shows ``needle``."""
        end = time.time() + timeout
        while time.time() < end:
            if needle in self.capture():
                return True
            time.sleep(0.3)
        return False

    def count_of(self, needle):
        """How many times the pane currently shows ``needle``."""
        return self.capture().count(needle)

    def wait_for_new(self, needle, timeout=15.0, seen=None):
        """Wait until the pane shows one more ``needle`` than ``seen`` holds.

        Confirmation lines stay on screen, so a later check must not accept a
        stale one printed by an earlier check. Callers pass the count taken
        *before* the action, because the action can already have printed the
        line while they were waiting for the panel to settle.
        """
        if seen is None:
            seen = self.count_of(needle)
        end = time.time() + timeout
        while time.time() < end:
            if self.count_of(needle) > seen:
                return True
            time.sleep(0.3)
        return False

    def type_literal(self, text, per_char=0.12):
        """Type ``text`` one character at a time so no key is dropped."""
        for ch in text:
            tmux("send-keys", "-t", "aish", "-l", ch, check=False)
            time.sleep(per_char)

    def send_line(self, text):
        """Send a whole line literally, for input that must arrive atomically."""
        tmux("send-keys", "-t", "aish", "-l", text, check=False)
        time.sleep(0.5)

    def key(self, *names, delay=0.5):
        """Send keys (named tmux keys such as ``Enter``/``Escape``) and settle."""
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
            self.key("Space", delay=0.3)
            self.key("C-u", delay=0.5)
        return False

    def reset_shell(self):
        """Leave any panel and clear the input line before the next check."""
        self.key("Escape", delay=0.6)
        # A lone Esc can swallow the key that follows it while the editor is
        # still finishing the escape sequence, so flush it with a throwaway key
        # and clear the line it may have produced.
        self.key("Space", delay=0.3)
        self.key("C-u", delay=0.3)

    def shell_takes_input(self):
        """True only when the shell (not a panel) ran the probe command.

        The typed text and the awaited token differ on purpose: only the
        ``printf`` output joins ``BACK591`` with the per-call number, so a panel
        that swallows the keystrokes (whose search box would echo the typed
        text) and output left over from an earlier probe can neither satisfy
        the wait.
        """
        self.probe += 1
        token = f"{MARKER}-{self.probe}"
        self.type_literal(f"printf '{MARKER}-%s\\n' {self.probe}")
        self.key("Enter", delay=1.5)
        return self.wait_for(token, timeout=8.0)

    def config_model(self):
        """The ``model:`` value persisted in this session's config file."""
        with open(self.config, encoding="utf-8") as handle:
            for line in handle:
                if line.startswith("model:"):
                    return line.split(":", 1)[1].strip()
        return None

    def close(self):
        """Tear down the pane, the temp config and the tmux server."""
        tmux("kill-server", check=False)
        shutil.rmtree(self.root, ignore_errors=True)


def check_switch_closes_picker(session, report):
    """Enter on another model: picker closes, shell returns, config switches."""
    session.reset_shell()
    target = other_model(session.config_model())
    confirm = f"Switched to model: {target}"
    seen = session.count_of(confirm)
    if not session.open_picker():
        report("picker did not open after /model")
        return False
    session.key("Down")
    session.key("Enter", delay=2.0)
    if not session.wait_for_new(confirm, timeout=8, seen=seen):
        report(f"no switch confirmation for {target}")
        return False
    if session.wait_for(PICKER_TITLE, timeout=3):
        report("picker reopened after the switch")
        return False
    if not session.shell_takes_input():
        report("shell did not take the keyboard back")
        return False
    if session.config_model() != target:
        report(f"config model is {session.config_model()!r}, expected {target!r}")
        return False
    return True


def check_current_model_closes_picker(session, report):
    """Enter on the highlighted (current) model also ends the picker."""
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
    """Esc closes the picker and leaves the configured model untouched."""
    session.reset_shell()
    before = session.config_model()
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
    if session.config_model() != before:
        report(
            f"Esc changed the model from {before!r} to {session.config_model()!r}"
        )
        return False
    return True


def check_direct_command(session, report):
    """`/model <name>` switches in one shot without opening the picker."""
    session.reset_shell()
    target = other_model(session.config_model())
    confirm = f"Switched to model: {target}"
    seen = session.count_of(confirm)
    session.type_literal(f"/model {target}")
    session.key("Enter")
    session.key("Enter", delay=2.0)
    if not session.wait_for_new(confirm, timeout=8, seen=seen):
        report(f"direct /model {target} did not switch")
        return False
    if session.wait_for(PICKER_TITLE, timeout=3):
        report("direct /model opened the picker")
        return False
    if session.config_model() != target:
        report(f"config model is {session.config_model()!r}, expected {target!r}")
        return False
    return session.shell_takes_input()


def check_request_uses_selected_model(session, report):
    """The model picked in the panel is the one the next AI request sends."""
    session.reset_shell()
    target = other_model(session.config_model())
    confirm = f"Switched to model: {target}"
    seen = session.count_of(confirm)
    MockModels.reset_requests()
    if not session.open_picker():
        report("picker did not open after /model")
        return False
    session.key("Down")
    session.key("Enter", delay=2.0)
    if not session.wait_for_new(confirm, timeout=8, seen=seen):
        report(f"no switch confirmation for {target}")
        return False
    session.send_line("; say hi")
    session.key("Enter", delay=1.0)
    sent = MockModels.wait_for_request()
    if sent != target:
        report(f"AI request used model {sent!r}, expected {target!r}")
        return False
    return session.shell_takes_input()


def main():
    """Run every check in order and report a summary."""
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
        """Print a failed check's reason and remember it."""
        reasons.append(message)
        print(f"  ! {message}")

    checks = (
        ("switching to another model closes the picker", check_switch_closes_picker),
        ("re-selecting the current model closes the picker",
         check_current_model_closes_picker),
        ("Esc cancels without switching", check_esc_cancels),
        ("direct /model <name> still switches", check_direct_command),
        ("next AI request uses the picked model",
         check_request_uses_selected_model),
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
