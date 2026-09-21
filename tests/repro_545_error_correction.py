#!/usr/bin/env python3
"""Deterministic reproduction for GitHub issue #545 (aish).

Issue: the `;` quick-fix (error correction) agent runs with the main tool
set and NO read-only enforcement (`handle_error_correction` uses the main
LlmSession with PromptContext::MainChat). During "analysis" it can execute
side-effecting bash commands BEFORE the user confirms any fix, and the
original failed command can be echoed back as the "corrected command".

This script replaces the LLM provider with a scripted OpenAI-compatible
mock (same SSE wire format as crates/aish-llm/src/probe.rs tests), so no
real model is needed. The mock drives the correction agent to:

  request 1: call the `bash` tool with `touch <marker>`  (side effect)
  request 2: return the ORIGINAL failed command as the corrected command

Verdict logic:
  BUG_REPRODUCED (exit 1) when BOTH hold:
    A) <marker> appears after `;` was the only keystroke sent. The tool
       loop blocks on any Confirm panel until a key is pressed, so file
       creation without further input proves unconfirmed execution.
    B) The shell offers the ORIGINAL failed command as the "corrected
       command" (no same-command rejection).
  FIXED (exit 0) when either observable is absent.

Usage:
    AISH_BIN=target/release/aish python3 tests/repro_545_error_correction.py
"""

import fcntl
import json
import os
import re
import select
import shutil
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import pty

FAILED_CMD = "nosuchcmd_545_xyz"
MARKER = "/tmp/aish545_side_effect.marker"
TRANSCRIPT_LOG = "/tmp/aish545_transcript.log"

REQUEST_COUNT = [0]
REQUEST_BODIES = []
STATE_LOCK = threading.Lock()


# ---- OpenAI-compatible SSE helpers (mirrors probe.rs test format) ----

def sse(events):
    return "".join(events)


def content_delta(text):
    payload = json.dumps({
        "choices": [{"index": 0, "delta": {"content": text}, "finish_reason": None}],
    })
    return f"data: {payload}\n\n"


def tool_call_delta(index, call_id, name, args):
    payload = json.dumps({
        "choices": [{"index": 0, "delta": {"tool_calls": [{
            "index": index, "id": call_id, "type": "function",
            "function": {"name": name, "arguments": args},
        }]}, "finish_reason": None}],
    })
    return f"data: {payload}\n\n"


def finish_chunk(reason):
    payload = json.dumps({
        "choices": [{"index": 0, "delta": {}, "finish_reason": reason}],
    })
    return f"data: {payload}\n\n"


def done():
    return "data: [DONE]\n\n"


def scenario_for(n):
    """Return (tool_calls, content) for mock request n."""
    if n == 1:
        # "Analysis" phase: the correction agent executes a side-effecting
        # command via the bash tool. Default policy classifies this LOW
        # (no rule matches touch /tmp), so it should run with NO panel.
        args = json.dumps({"command": f"touch {MARKER}"})
        return [("call_545_1", "bash", args)], None
    if n == 2:
        # Final answer: echo the ORIGINAL failed command back as the fix.
        fix = json.dumps({
            "type": "corrected_command",
            "command": FAILED_CMD,
            "description": "retry the original command",
        }, ensure_ascii=False)
        return [], "```json\n" + fix + "\n```"
    return [], "unexpected request"


def sse_for(n):
    calls, content = scenario_for(n)
    events = []
    for i, (call_id, name, args) in enumerate(calls):
        events.append(tool_call_delta(i, call_id, name, args))
    events.append(finish_chunk("tool_calls" if calls else "stop"))
    if content is not None:
        events.append(content_delta(content))
    events.append(done())
    return sse(events)


def json_for(n):
    """Non-streaming (stream=false) reply: OpenAI plain JSON message format."""
    calls, content = scenario_for(n)
    message: dict = {"role": "assistant", "content": content or ""}
    if calls:
        message["tool_calls"] = [
            {
                "id": call_id,
                "type": "function",
                "function": {"name": name, "arguments": args},
            }
            for call_id, name, args in calls
        ]
    return json.dumps({
        "choices": [{
            "index": 0,
            "message": message,
            "finish_reason": "tool_calls" if calls else "stop",
        }],
    })


class MockHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_POST(self):
        length = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(length)
        try:
            wants_stream = json.loads(body).get("stream", False)
        except Exception:
            wants_stream = True
        with STATE_LOCK:
            REQUEST_COUNT[0] += 1
            n = REQUEST_COUNT[0]
            REQUEST_BODIES.append(body.decode("utf-8", errors="replace"))
        if wants_stream:
            data = sse_for(n).encode()
            ctype = "text/event-stream"
        else:
            data = json_for(n).encode()
            ctype = "application/json"
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *args):
        pass


# ---- PTY driving helpers (same pattern as tests/repro_subagent_anim_leak.py) ----

def read_available(fd, timeout=0.5):
    data = b""
    end = time.time() + timeout
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.1)
        if r:
            try:
                chunk = os.read(fd, 65536)
            except OSError:
                break
            if not chunk:
                break
            data += chunk
            end = time.time() + 0.2
    return data


def cleanup_proc(proc, master):
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
        except Exception:
            pass
    try:
        os.close(master)
    except OSError:
        pass


def main() -> int:
    aish_bin = os.environ.get("AISH_BIN", "target/release/aish")
    if not os.path.isfile(aish_bin):
        print(f"aish binary not found: {aish_bin}")
        return 2

    for path in (MARKER,):
        if os.path.exists(path):
            os.remove(path)

    REQUEST_COUNT[0] = 0
    REQUEST_BODIES.clear()

    tmp = tempfile.mkdtemp(prefix="aish545-")
    config_dir = os.path.join(tmp, "config", "aish")
    data_dir = os.path.join(tmp, "data")
    os.makedirs(config_dir, exist_ok=True)
    os.makedirs(data_dir, exist_ok=True)

    server = ThreadingHTTPServer(("127.0.0.1", 0), MockHandler)
    port = server.server_address[1]
    threading.Thread(target=server.serve_forever, daemon=True).start()

    with open(os.path.join(config_dir, "config.yaml"), "w") as f:
        f.write(
            "model: gpt-4o\n"
            f"api_base: http://127.0.0.1:{port}/v1\n"
            "api_key: test-key-545\n"
            "theme: dark\n"
        )

    env = os.environ.copy()
    env["XDG_CONFIG_HOME"] = os.path.join(tmp, "config")
    env["XDG_DATA_HOME"] = data_dir
    env["AISH_LATEST_URL"] = "http://127.0.0.1:1/"
    env["TERM"] = "xterm-256color"
    env["RUST_LOG"] = "warn"

    master, slave = pty.openpty()
    winsize = struct.pack("HHHH", 40, 160, 0, 0)
    fcntl.ioctl(master, termios.TIOCSWINSZ, winsize)

    proc = subprocess.Popen(
        [aish_bin], stdin=slave, stdout=slave, stderr=slave,
        env=env, close_fds=True,
    )
    os.close(slave)
    flags = fcntl.fcntl(master, fcntl.F_GETFL)
    fcntl.fcntl(master, fcntl.F_SETFL, flags | os.O_NONBLOCK)

    transcript = bytearray()

    def drain(seconds):
        end = time.time() + seconds
        while time.time() < end:
            data = read_available(master, 0.3)
            transcript.extend(data)
        return bytes(transcript)

    def wait_for(pred, timeout, label):
        end = time.time() + timeout
        while time.time() < end:
            drain(0.4)
            result = pred(bytes(transcript))
            if result is not None:
                return result
        print(f"--- timeout waiting for: {label} ---")
        return None

    def dump_transcript():
        with open(TRANSCRIPT_LOG, "w") as f:
            f.write(bytes(transcript).decode("utf-8", errors="replace"))

    try:
        # 1. Wait for the REPL prompt: welcome banner "AI Shell v" or the
        #    status line ("aish" + branch "main"). Depends on terminal size.
        def prompt_seen(t):
            text = t.decode("utf-8", errors="replace")
            if "AI Shell v" in text or ("aish" in text and "main" in text):
                return True
            return None
        if wait_for(prompt_seen, 30, "REPL prompt") is None:
            return 4
        print("--- aish prompt reached ---")

        # 2. Make the original command fail (exit != 0) as a user command.
        #    Hint text is "命令执行失败。输入 ; 快速修复..." (zh-CN) or the
        #    English "Command execution failed." — match stable fragments.
        os.write(master, (FAILED_CMD + "\r").encode())
        hint_zh = "命令执行失败".encode("utf-8")
        hint = wait_for(
            lambda t: True
            if (b"Command execution failed" in t or hint_zh in t)
            else None,
            25, "failure hint",
        )
        dump_transcript()
        if hint is None:
            return 4
        print("--- original command failed; correction hint shown ---")

        # 3. Trigger quick fix. From here on we send NOTHING except the
        #    final 'n' answer, so any state change is attributable to the
        #    correction agent alone.
        t0 = time.time()
        os.write(master, b";\r")
        print("--- ';' sent, polling for side effect ---")

        # 4. Evidence A: marker appears with no further user input.
        marker_ok = False
        end = time.time() + 45
        while time.time() < end:
            drain(0.4)
            if os.path.exists(MARKER) and os.path.getmtime(MARKER) >= t0 - 1:
                marker_ok = True
                break
        drain(1.0)
        text = bytes(transcript).decode("utf-8", errors="replace")
        with open(TRANSCRIPT_LOG, "w") as f:
            f.write(text)
        with STATE_LOCK:
            nreq = REQUEST_COUNT[0]
        print(f"--- marker exists: {marker_ok}; mock requests so far: {nreq} ---")

        # 5. Evidence B: what did the shell do with the echo-back fix?
        #    - BUG:   title "纠正后的命令/Corrected command:" + the ORIGINAL
        #             command, followed by a Y/n execute prompt.
        #    - FIXED: the same_as_failed warning is shown instead, or the
        #             title never offers the original command.
        outcome = wait_for(
            lambda t: True
            if re.search(
                r"Corrected command|纠正后的命令|same_as_failed|修复建议与失败命令相同|Execute|拒绝重复",
                t.decode("utf-8", errors="replace"),
            )
            else None,
            30, "correction outcome",
        )
        if outcome is None:
            # No outcome within the window means the correction turn never
            # completed — verdicts below would silently pass. Hard-fail.
            return 4
        drain(2.0)
        text = bytes(transcript).decode("utf-8", errors="replace")
        with open(TRANSCRIPT_LOG, "w") as f:
            f.write(text)

        title_match = None
        for m in re.finditer(r"(?:Corrected command|纠正后的命令)[^\n]*", text):
            title_match = m
        same_command = False
        if title_match:
            window = text[title_match.start():title_match.start() + 400]
            same_command = FAILED_CMD in window
        rejected = ("修复建议与失败命令相同" in text) or ("same_as_failed" in text)
        print(f"--- corrected command equals original: {same_command} ---")
        print(f"--- echo-back rejection warning shown: {rejected} ---")

        # 6. Decline the fix and quit.
        if re.search(r"Y/n", text):
            os.write(master, b"n\r")
            time.sleep(1.0)
        drain(1.0)
        os.write(master, b"exit\r")
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            pass

        with STATE_LOCK:
            nreq = REQUEST_COUNT[0]
            body1 = REQUEST_BODIES[0] if len(REQUEST_BODIES) > 0 else ""

        print(f"--- final mock request count: {nreq} ---")
        evidence_a = marker_ok and nreq >= 2 and "bash" in body1
        evidence_b = same_command and not rejected

        print()
        if evidence_a:
            print("BUG REPRODUCED (A): correction agent executed a side-effecting")
            print(f"  command ('touch {MARKER}') with NO user confirmation between")
            print("  ';' and the effect. Read-only enforcement is not applied.")
        else:
            print("FIXED (A): no unconfirmed side effect observed.")
        if evidence_b:
            print("BUG REPRODUCED (B): the ORIGINAL failed command was offered back")
            print("  as the 'corrected command' (no same-command rejection).")
        else:
            print("FIXED (B): original command was not echoed back as the fix.")
        print(f"transcript: {TRANSCRIPT_LOG}")

        if evidence_a and evidence_b:
            return 1
        if evidence_a or evidence_b:
            return 1
        return 0

    finally:
        cleanup_proc(proc, master)
        server.shutdown()
        shutil.rmtree(tmp, ignore_errors=True)
        if os.path.exists(MARKER):
            os.remove(MARKER)


if __name__ == "__main__":
    sys.exit(main())
