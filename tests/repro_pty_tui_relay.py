#!/usr/bin/env python3
"""Regression tests for the PTY relay contract that TUI commands rely on.

These four checks came out of debugging omp's duplicated/garbled animation
output inside aish:

1. LF translation: a command's ``\\n`` must reach the terminal exactly once
   (``A\\r\\nB\\r\\n``, not ``A\\r\\r\\nB\\r\\r\\n``). The child's PTY slave used
   to translate it and the real terminal translated it again.
2. Device queries: a child's ``ESC[c`` (DA1) / ``ESC[6n`` (CPR) requests must
   reach the real terminal. They used to be deleted for every command that was
   not on the interactive whitelist, so full-screen programs never got an
   answer and fell back to degraded layout/anchor logic.
3. Resize: a running command is resized (and signalled) when the window
   changes; otherwise TUIs keep drawing for a stale geometry, which wraps their
   frames on the real terminal.
4. Report filter: answers to device queries must not be injected into a
   line-oriented child (it would echo/execute them), but must reach a child that
   shows full-screen behaviour.

Usage:
    AISH_BIN=target/debug/aish python3 tests/repro_pty_tui_relay.py
"""

import fcntl
import os
import pty
import re
import select
import shutil
import struct
import subprocess
import tempfile
import termios
import time

PROMPT_RE = re.compile(rb"aish .{0,80}->")
REPORT = b"\x1b[1;17R"


def read_available(fd: int, timeout: float = 0.5) -> bytes:
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
    return data


class AishSession:
    """aish running in a PTY, with helpers to type commands and drain output."""

    def __init__(self, aish_bin: str, rows: int = 30, cols: int = 100):
        self.tmp_config = tempfile.mkdtemp(prefix="aish-repro-pty-")
        real_config = os.path.join(os.path.expanduser("~/.config"), "aish")
        dst = os.path.join(self.tmp_config, "aish")
        if not os.path.exists(dst):
            os.symlink(real_config, dst)

        env = os.environ.copy()
        env["XDG_CONFIG_HOME"] = self.tmp_config
        env["TERM"] = "xterm-256color"
        env["RUST_LOG"] = "warn"

        self.master, slave = pty.openpty()
        self.set_size(rows, cols)
        self.proc = subprocess.Popen(
            [aish_bin], stdin=slave, stdout=slave, stderr=slave, env=env, close_fds=True
        )
        os.close(slave)
        flags = fcntl.fcntl(self.master, fcntl.F_GETFL)
        fcntl.fcntl(self.master, fcntl.F_SETFL, flags | os.O_NONBLOCK)
        self.buffer = b""

    def set_size(self, rows: int, cols: int) -> None:
        fcntl.ioctl(self.master, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))

    def drain(self, timeout: float = 0.5) -> bytes:
        data = read_available(self.master, timeout)
        self.buffer += data
        return data

    def wait_prompt(self, timeout: float = 20.0) -> bool:
        end = time.time() + timeout
        quiet_since = None
        while time.time() < end:
            if self.drain(0.3):
                quiet_since = None
            elif PROMPT_RE.search(self.buffer):
                quiet_since = quiet_since or time.time()
                if time.time() - quiet_since > 0.5:
                    return True
        return False

    def run_command(self, command: str, wait: float = 3.0) -> bytes:
        self.buffer = b""
        os.write(self.master, (command + "\n").encode())
        out = b""
        end = time.time() + wait
        while time.time() < end:
            out += self.drain(0.2)
        return out + self.drain(0.3)

    def close(self) -> None:
        # Ask the inner shell to exit first: a daemon-backed session reaps its
        # own daemon on shell exit, while killing the client would leave the
        # daemon running (it is designed to survive client detach).
        try:
            os.write(self.master, b"exit\n")
        except OSError:
            pass
        try:
            self.proc.wait(timeout=3)
        except subprocess.TimeoutExpired:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait(timeout=5)
        os.close(self.master)
        shutil.rmtree(self.tmp_config, ignore_errors=True)


def check_lf_translation(aish_bin: str) -> bool:
    """A command's newline must reach the terminal exactly once."""
    s = AishSession(aish_bin)
    try:
        if not s.wait_prompt():
            print("  ! prompt timeout")
            return False
        out = s.run_command("printf 'A\\nB\\n'", wait=2.0)
        if b"A\r\nB\r\n" not in out:
            print(f"  ! expected 'A\\r\\nB\\r\\n', got {out[-120:]!r}")
            return False
        if b"A\r\r\n" in out:
            print("  ! newline was translated twice (A\\r\\r\\n)")
            return False
        return True
    finally:
        s.close()


def check_device_queries_forwarded(aish_bin: str) -> bool:
    """A child's device queries must not be stripped from the relay."""
    s = AishSession(aish_bin)
    try:
        if not s.wait_prompt():
            print("  ! prompt timeout")
            return False
        out = s.run_command("printf '\\033[c\\033[6n\\033[?2004h'", wait=2.0)
        missing = [seq for seq in (b"\x1b[c", b"\x1b[6n") if seq not in out]
        if missing:
            print(f"  ! queries stripped from the relay: {missing}")
            return False
        return True
    finally:
        s.close()


def check_resize_followed(aish_bin: str) -> bool:
    """A running command must see window-size changes."""
    tmpdir = tempfile.mkdtemp(prefix="aish-repro-resize-")
    probe = os.path.join(tmpdir, "size_probe.py")
    with open(probe, "w", encoding="utf-8") as handle:
        handle.write(
            "import fcntl, struct, termios, time\n"
            "for _ in range(14):\n"
            "    rows, cols = struct.unpack('HHHH',"
            " fcntl.ioctl(0, termios.TIOCGWINSZ, b'\\0' * 8))[:2]\n"
            "    print('SIZE %sx%s' % (rows, cols), flush=True)\n"
            "    time.sleep(0.4)\n"
        )

    s = AishSession(aish_bin, rows=30, cols=100)
    try:
        if not s.wait_prompt():
            print("  ! prompt timeout")
            return False
        s.buffer = b""
        os.write(s.master, f"python3 {probe}\n".encode())
        out = b""
        resized = False
        end = time.time() + 8.0
        while time.time() < end:
            out += s.drain(0.2)
            if not resized and b"SIZE 30x100" in out:
                s.set_size(20, 60)
                resized = True
        sizes = re.findall(rb"SIZE (\d+)x(\d+)", out)
        if not sizes:
            print("  ! probe produced no size output")
            return False
        last = (int(sizes[-1][0]), int(sizes[-1][1]))
        if last != (20, 60):
            print(f"  ! child kept the stale geometry: {last}")
            return False
        return True
    finally:
        s.close()
        shutil.rmtree(tmpdir, ignore_errors=True)


def _report_probe(
    aish_bin: str, command: str, await_bytes: bytes, chunks: tuple = (REPORT,)
) -> bytes:
    """Run `command`, inject forged report chunks once its output started.

    `chunks` are written 0.2 s apart, which reproduces a terminal (or a
    multiplexer) relaying one report in several writes.
    """
    s = AishSession(aish_bin)
    try:
        if not s.wait_prompt():
            return b""
        s.buffer = b""
        os.write(s.master, (command + "\n").encode())
        out = b""
        pending = list(chunks)
        end = time.time() + 5.0
        while time.time() < end:
            out += s.drain(0.2)
            if pending and await_bytes in out:
                os.write(s.master, pending.pop(0))
        return out
    finally:
        s.close()


def check_report_filter(aish_bin: str) -> bool:
    """Leaked reports stay out of line-oriented children, reach TUIs.

    The child's tty echoes whatever lands in its input queue, so the echoed
    report bytes prove delivery; their absence proves the filter dropped them.
    """
    echoed = (b"\x1b[1;17R", b"^[[1;17R")

    plain = _report_probe(
        aish_bin, "printf 'plain-marker\\n'; sleep 1", b"plain-marker"
    )
    if any(seq in plain for seq in echoed):
        print("  ! line-oriented child received a leaked report")
        return False

    tui = _report_probe(
        aish_bin, "printf '\\033[?25l'; sleep 1", b"\x1b[?25l"
    )
    if not any(seq in tui for seq in echoed):
        print(f"  ! full-screen child never received the report: {tui[-160:]!r}")
        return False
    return True


def check_split_report_dropped(aish_bin: str) -> bool:
    """A report relayed in several writes must not leak either.

    The ESC can arrive alone (a multiplexer forwards the answer in separate
    writes); it must be held rather than handed to the child.
    """
    out = _report_probe(
        aish_bin,
        "printf 'plain-marker\\n'; sleep 1",
        b"plain-marker",
        chunks=(b"\x1b", b"[1;17R"),
    )
    if any(seq in out for seq in (b"\x1b[1;17R", b"^[[1;17R", b"[1;17R")):
        print(f"  ! split report leaked into a line-oriented child: {out[-160:]!r}")
        return False
    return True


def main() -> int:
    aish_bin = os.environ.get("AISH_BIN", "target/debug/aish")
    if not os.path.isfile(aish_bin):
        print(f"aish binary not found: {aish_bin}")
        return 2

    checks = [
        ("lf translated once", check_lf_translation),
        ("device queries forwarded", check_device_queries_forwarded),
        ("resize followed while running", check_resize_followed),
        ("terminal reports filtered by child type", check_report_filter),
        ("split report stays out of a line-oriented child", check_split_report_dropped),
    ]
    failed = []
    for name, check in checks:
        print(f"--- {name}")
        if check(aish_bin):
            print("    ok")
        else:
            print("    FAILED")
            failed.append(name)

    if failed:
        print(f"\nFAILED: {', '.join(failed)}")
        return 1
    print("\nall PTY relay checks passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
