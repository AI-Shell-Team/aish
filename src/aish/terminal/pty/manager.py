"""PTY manager using direct pty.fork() - pyxtermjs style."""

import fcntl
import json
import os
import pty
import select
import signal
import struct
import termios
import threading
import time
from typing import Callable, Optional

from .command_state import CommandResult, CommandState
from .control_protocol import BackendControlEvent, decode_control_chunk


def set_winsize(fd: int, row: int, col: int, xpix: int = 0, ypix: int = 0) -> None:
    """Set terminal window size."""
    winsize = struct.pack("HHHH", row, col, xpix, ypix)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, winsize)


class PTYManager:
    """Manage PTY connection to bash using direct pty.fork().

    This is the pyxtermjs approach: simple, reliable, perfect bash compatibility.

    Usage:
        manager = PTYManager()
        manager.start()

        # With callback
        manager.set_output_callback(lambda data: print(data, end=''))
        manager.send("ls -la\n")

        # Later
        manager.stop()
    """

    def __init__(
        self,
        rows: int = 24,
        cols: int = 80,
        cwd: Optional[str] = None,
        env: Optional[dict] = None,
        use_output_thread: bool = True,
    ):
        self._rows = rows
        self._cols = cols
        self._cwd = cwd or os.getcwd()
        self._env = env or {}
        self._use_output_thread = use_output_thread

        self._master_fd: Optional[int] = None
        self._child_pid: Optional[int] = None
        self._control_fd: Optional[int] = None
        self._control_write_fd: Optional[int] = None
        self._metadata_read_fd: Optional[int] = None
        self._metadata_write_fd: Optional[int] = None
        self._running = False
        self._output_thread: Optional[threading.Thread] = None

        # Callbacks
        self._output_callback: Optional[Callable[[bytes], None]] = None
        self._exit_code_callback: Optional[Callable[[int], None]] = None

        # Event-based command lifecycle state
        self._command_state = CommandState()
        self._control_buffer = b""
        self._protocol_issues: list[str] = []
        self._continuation_prompt = "> "
        self._completed_results: list[CommandResult] = []
        self._completion_condition = threading.Condition()
        self._next_backend_command_seq = -1
        self._startup_session_ready = False
        self._startup_prompt_ready = False
        self._startup_cwd: Optional[str] = None

        # Lock for thread-safe operations
        self._lock = threading.RLock()

        # exec mode: when active, output thread buffers instead of forwarding
        self._exec_mode = threading.Event()
        self._exec_buffer: bytearray = bytearray()

    @property
    def is_running(self) -> bool:
        """Check if PTY is active."""
        return self._running and self._child_pid is not None

    @property
    def command_state(self) -> CommandState:
        """Get command lifecycle state."""
        return self._command_state

    @property
    def last_command(self) -> str:
        """Return the last completed command text."""
        return self._command_state.last_command

    @property
    def last_exit_code(self) -> int:
        """Return the last completed command exit code."""
        return self._command_state.last_exit_code

    @property
    def can_correct_last_error(self) -> bool:
        """Return whether the last completed command should offer AI correction."""
        return self._command_state.can_correct_last_error

    @property
    def control_fd(self) -> Optional[int]:
        """Get the read end of the backend control channel."""
        return self._control_fd

    @property
    def startup_session_ready(self) -> bool:
        """Whether the startup handshake emitted session_ready."""
        return self._startup_session_ready

    @property
    def startup_prompt_ready(self) -> bool:
        """Whether the startup handshake emitted an initial prompt_ready."""
        return self._startup_prompt_ready

    @property
    def startup_ready(self) -> bool:
        """Whether startup reached an interactive prompt before start() returned."""
        return self._startup_session_ready and self._startup_prompt_ready

    @property
    def startup_cwd(self) -> Optional[str]:
        """Initial cwd reported by the backend startup handshake."""
        return self._startup_cwd

    def set_output_callback(self, callback: Callable[[bytes], None]) -> None:
        """Set callback for PTY output."""
        self._output_callback = callback


    def set_exit_code_callback(self, callback: Callable[[int], None]) -> None:
        """Set callback for exit code changes."""
        self._exit_code_callback = callback

    def start(self) -> None:
        """Start bash process with PTY."""
        if self._running:
            return

        self._startup_session_ready = False
        self._startup_prompt_ready = False
        self._startup_cwd = None

        self._control_fd, self._control_write_fd = os.pipe()
        os.set_inheritable(self._control_write_fd, True)
        self._metadata_read_fd, self._metadata_write_fd = os.pipe()
        os.set_inheritable(self._metadata_read_fd, True)

        self._child_pid, self._master_fd = pty.fork()

        if self._child_pid == 0:
            # Child process: exec bash
            os.chdir(self._cwd)
            if self._control_fd is not None:
                try:
                    os.close(self._control_fd)
                except OSError:
                    pass
            if self._metadata_write_fd is not None:
                try:
                    os.close(self._metadata_write_fd)
                except OSError:
                    pass

            # Build environment
            env = dict(os.environ)
            env.update(self._env)
            env["TERM"] = "xterm-256color"
            if self._control_write_fd is not None:
                env["AISH_CONTROL_FD"] = str(self._control_write_fd)
            if self._metadata_read_fd is not None:
                env["AISH_COMMAND_METADATA_FD"] = str(self._metadata_read_fd)

            # Use our rcfile wrapper to set up exit code tracking while preserving user's config
            rcfile_path = os.path.join(os.path.dirname(__file__), "bash_rc_wrapper.sh")
            if os.path.exists(rcfile_path):
                os.execvpe(
                    "/bin/bash",
                    ["/bin/bash", "--rcfile", rcfile_path, "-i"],
                    env,
                )
            else:
                # Fallback without rcfile
                os.execvpe(
                    "/bin/bash",
                    ["/bin/bash"],
                    env,
                )
            os._exit(1)

        if self._control_write_fd is not None:
            try:
                os.close(self._control_write_fd)
            except OSError:
                pass
            self._control_write_fd = None
        if self._metadata_read_fd is not None:
            try:
                os.close(self._metadata_read_fd)
            except OSError:
                pass
            self._metadata_read_fd = None

        # Set non-blocking
        flags = fcntl.fcntl(self._master_fd, fcntl.F_GETFL)
        fcntl.fcntl(self._master_fd, fcntl.F_SETFL, flags | os.O_NONBLOCK)
        if self._control_fd is not None:
            control_flags = fcntl.fcntl(self._control_fd, fcntl.F_GETFL)
            fcntl.fcntl(self._control_fd, fcntl.F_SETFL, control_flags | os.O_NONBLOCK)

        # Set window size
        set_winsize(self._master_fd, self._rows, self._cols)

        self._running = True

        # Drain initial shell readiness before exposing the PTY to callers.
        self._wait_ready(timeout=1.0)

        # Start output reader thread (optional - disabled when main loop reads directly)
        if self._use_output_thread:
            self._output_thread = threading.Thread(target=self._output_loop, daemon=True)
            self._output_thread.start()

    def _wait_ready(self, timeout: float = 0.3) -> None:
        """Wait for bash to initialize."""
        if self._control_fd is None:
            return

        start = time.monotonic()
        saw_session_ready = self._startup_session_ready
        saw_startup_prompt_ready = self._startup_prompt_ready
        quiet_since: float | None = None

        while time.monotonic() - start < timeout:
            ready, _, _ = select.select([self._control_fd], [], [], 0.05)
            if not ready and saw_session_ready and saw_startup_prompt_ready:
                if quiet_since is None:
                    quiet_since = time.monotonic()
                    continue
                if time.monotonic() - quiet_since >= 0.05:
                    break
                continue

            quiet_since = None
            for fd in ready:
                try:
                    data = os.read(fd, 4096)
                except OSError:
                    break
                if not data:
                    continue
                events, errors = self.decode_control_events(data)
                if errors:
                    self._protocol_issues.extend(errors)
                    self._protocol_issues = self._protocol_issues[-50:]
                for event in events:
                    if event.type == "session_ready":
                        ps2 = event.payload.get("ps2")
                        if isinstance(ps2, str) and ps2:
                            self._continuation_prompt = ps2
                        cwd = event.payload.get("cwd")
                        if isinstance(cwd, str) and cwd:
                            self._startup_cwd = cwd
                        saw_session_ready = True
                    elif event.type == "prompt_ready" and event.payload.get("command_seq") in {None, "", "null"}:
                        saw_startup_prompt_ready = True

        self._startup_session_ready = saw_session_ready
        self._startup_prompt_ready = saw_startup_prompt_ready

    def _output_loop(self) -> None:
        """Background thread to read and forward PTY output."""
        max_read_bytes = 1024 * 20

        while self._running:
            try:
                # Poll for data
                read_fds = [self._master_fd]
                if self._control_fd is not None:
                    read_fds.append(self._control_fd)
                ready, _, _ = select.select(read_fds, [], [], 0.01)
                if not ready:
                    continue

                for fd in ready:
                    data = os.read(fd, max_read_bytes)
                    if not data:
                        if fd == self._master_fd:
                            self._running = False
                            break
                        continue

                    if fd == self._control_fd:
                        self._dispatch_control_chunk(data)
                        continue

                    if self._exec_mode.is_set():
                        self._exec_buffer.extend(data)
                    else:
                        if data and self._output_callback:
                            try:
                                self._output_callback(data)
                            except Exception:
                                pass
                else:
                    continue
                break

            except OSError:
                self._running = False
                break

    def send(self, data: bytes) -> int:
        """Send input to bash."""
        if not self._running or self._master_fd is None:
            return 0

        with self._lock:
            try:
                return os.write(self._master_fd, data)
            except OSError:
                return 0

    def send_command(
        self,
        command: str,
        command_seq: int | None = None,
        source: str = "backend",
    ) -> None:
        """Send a command (with newline) to bash.

        Command lifecycle is tracked via the backend control channel.
        """
        stripped_command = command.strip()
        command_to_send = command if source == "user" else f" {command}"
        with self._lock:
            submission = self._command_state.register_command(
                stripped_command, source=source, command_seq=command_seq
            )
            self._write_command_metadata(
                command=stripped_command,
                source=source,
                command_seq=command_seq,
                submission_id=submission.submission_id if submission is not None else None,
            )
            self.send((command_to_send + "\n").encode())

    def register_user_command(self, command: str) -> None:
        """Record a user-submitted command before it reaches bash."""
        self._command_state.register_user_command(command)

    def clear_error_correction(self) -> None:
        """Dismiss the current error-correction hint cycle."""
        self._command_state.clear_error_correction()

    def consume_error(self) -> tuple[str, int] | None:
        """Consume the latest user-facing command failure if available."""
        return self._command_state.consume_error()

    def handle_backend_event(
        self, event: BackendControlEvent
    ) -> CommandResult | None:
        """Update internal command state from a decoded backend event."""
        result = self._command_state.handle_backend_event(event)
        if result is not None:
            if self._exit_code_callback:
                try:
                    self._exit_code_callback(result.exit_code)
                except Exception:
                    pass
            with self._completion_condition:
                self._completed_results.append(result)
                self._completed_results = self._completed_results[-50:]
                self._completion_condition.notify_all()
        return result

    def decode_control_events(
        self, chunk: bytes
    ) -> tuple[list[BackendControlEvent], list[str]]:
        """Decode NDJSON control events from a raw pipe read."""
        events, remainder, errors = decode_control_chunk(self._control_buffer, chunk)
        self._control_buffer = remainder
        return events, errors

    @property
    def protocol_issues(self) -> tuple[str, ...]:
        """Return decoded protocol and command-state issues."""
        return tuple(self._protocol_issues) + self._command_state.protocol_issues

    def consume_protocol_issues(self) -> tuple[str, ...]:
        """Consume and clear protocol and command-state issues."""
        issues = tuple(self._protocol_issues)
        self._protocol_issues.clear()
        return issues + self._command_state.consume_protocol_issues()

    def _dispatch_control_chunk(self, chunk: bytes) -> list[BackendControlEvent]:
        events, errors = self.decode_control_events(chunk)
        if errors:
            self._protocol_issues.extend(errors)
            self._protocol_issues = self._protocol_issues[-50:]
        for event in events:
            if event.type == "session_ready":
                ps2 = event.payload.get("ps2")
                if isinstance(ps2, str) and ps2:
                    self._continuation_prompt = ps2
            self.handle_backend_event(event)
        return events

    def resize(self, rows: int, cols: int) -> None:
        """Resize terminal."""
        if self._master_fd is None:
            return

        with self._lock:
            self._rows = rows
            self._cols = cols
            set_winsize(self._master_fd, rows, cols)

    @staticmethod
    def _strip_continuation_prompt(output_line: str, continuation_prompt: str | None) -> str:
        candidates: list[str] = []
        if continuation_prompt:
            candidates.append(continuation_prompt)
            stripped_prompt = continuation_prompt.rstrip()
            if stripped_prompt and stripped_prompt != continuation_prompt:
                candidates.append(stripped_prompt)
        candidates.append(">")

        for candidate in candidates:
            if output_line.startswith(candidate):
                return output_line[len(candidate) :].lstrip()
        return output_line

    @staticmethod
    def _strip_echoed_command(
        text: str,
        command: str,
        continuation_prompt: str | None = None,
    ) -> str:
        command_lines = [line.lstrip().rstrip() for line in command.strip().splitlines()]
        if not command_lines:
            return text

        output_lines = text.split("\n")
        first_content_index = 0
        while first_content_index < len(output_lines) and not output_lines[first_content_index].strip():
            first_content_index += 1

        if len(output_lines) - first_content_index < len(command_lines):
            return text

        for index, command_line in enumerate(command_lines):
            output_line = output_lines[first_content_index + index].lstrip()
            if index > 0:
                output_line = PTYManager._strip_continuation_prompt(
                    output_line,
                    continuation_prompt,
                )
            if output_line.rstrip() != command_line:
                return text

        return "\n".join(output_lines[first_content_index + len(command_lines) :])

    def _clean_pty_output(self, raw: bytes, command: str) -> str:
        """Clean PTY output: strip ANSI, echo, prompt, exit markers."""
        import re as _re

        text = raw.decode("utf-8", errors="replace")

        # Remove ANSI escape sequences (including CSI ? and ; variants)
        text = _re.sub(r"\x1b\[[0-9;?]*[a-zA-Z]", "", text)
        text = _re.sub(r"\x1b\].*?\x07", "", text)

        # Remove carriage returns (keep newlines)
        text = text.replace("\r\n", "\n").replace("\r", "")

        text = PTYManager._strip_echoed_command(text, command, self._continuation_prompt)

        # Remove trailing prompt line (contains prompt symbols)
        lines = text.rstrip().split("\n")
        if lines:
            last = lines[-1]
            if "\u279c" in last or _re.match(r"^.*\S+.*\s*[#$>]\s*$", last):
                lines = lines[:-1]
        text = "\n".join(lines)

        return text.strip()

    def execute_command(
        self, command: str, timeout: Optional[float] = None
    ) -> tuple[str, int]:
        """Execute a command via PTY and return (output, exit_code).

        Sends command to bash, buffers output until exit code marker appears,
        then returns cleaned output. During execution, the output thread
        buffers instead of forwarding to the display callback.

        When timeout is None, wait indefinitely for the command to finish.
        """
        if not self.is_running:
            return "", -1

        if self._use_output_thread:
            return self._exec_via_thread(command, timeout)
        else:
            return self._exec_via_poll(command, timeout)

    def _exec_via_thread(
        self, command: str, timeout: Optional[float]
    ) -> tuple[str, int]:
        """Execute using background output thread for I/O."""
        command_seq = self._allocate_backend_command_seq()

        # Enter exec mode: output thread will buffer
        self._exec_buffer.clear()
        self._exec_mode.set()

        # Send command
        self.send_command(command, command_seq=command_seq)

        # Wait for exit code; only enforce timeout when explicitly requested.
        result = self._wait_for_completed_result(command_seq, timeout)

        # Exit exec mode
        self._exec_mode.clear()

        # Grab buffered output
        raw_output = bytes(self._exec_buffer)
        self._exec_buffer.clear()

        if result is None:
            if timeout is not None:
                # Timeout - send Ctrl+C
                self.send(b"\x03")
            cleaned = self._clean_pty_output(raw_output, command)
            return cleaned, -1

        cleaned = self._clean_pty_output(raw_output, command)
        return cleaned, result.exit_code

    def _exec_via_poll(
        self, command: str, timeout: Optional[float]
    ) -> tuple[str, int]:
        """Execute by directly polling PTY fd (when no output thread)."""
        command_seq = self._allocate_backend_command_seq()

        # First, drain any existing output from PTY to avoid confusion
        # with previous command's prompt/output.
        drain_deadline = time.monotonic() + 0.2
        quiet_since: float | None = None
        try:
            while time.monotonic() < drain_deadline:
                read_fds = [self._master_fd]
                if self._control_fd is not None:
                    read_fds.append(self._control_fd)
                ready, _, _ = select.select(read_fds, [], [], 0.01)
                if not ready:
                    if quiet_since is None:
                        quiet_since = time.monotonic()
                        continue
                    if time.monotonic() - quiet_since >= 0.05:
                        break
                    continue
                quiet_since = None
                for fd in ready:
                    try:
                        data = os.read(fd, 4096)
                    except OSError:
                        continue
                    if fd == self._control_fd and data:
                        self._dispatch_control_chunk(data)
        except (ValueError, OSError):
            pass

        # Send command
        self.send_command(command, command_seq=command_seq)

        # Read output directly from PTY/control fds until prompt_ready appears.
        deadline = None if timeout is None else time.monotonic() + timeout
        raw_output = bytearray()

        while True:
            if deadline is not None and time.monotonic() >= deadline:
                break

            read_fds = [self._master_fd]
            if self._control_fd is not None:
                read_fds.append(self._control_fd)
            try:
                ready, _, _ = select.select(read_fds, [], [], 0.05)
            except (ValueError, OSError):
                self._running = False
                cleaned_output = self._clean_pty_output(bytes(raw_output), command)
                return cleaned_output, -1
            for fd in ready:
                try:
                    data = os.read(fd, 4096)
                except OSError:
                    if fd == self._master_fd:
                        self._running = False
                        cleaned_output = self._clean_pty_output(bytes(raw_output), command)
                        return cleaned_output, -1
                    continue
                if not data:
                    if fd == self._master_fd:
                        self._running = False
                        cleaned_output = self._clean_pty_output(bytes(raw_output), command)
                        return cleaned_output, -1
                    continue
                if fd == self._control_fd:
                    self._dispatch_control_chunk(data)
                else:
                    raw_output.extend(data)

            result = self._pop_completed_result(command_seq)
            if result is not None:
                cleaned_output = self._clean_pty_output(bytes(raw_output), command)
                return cleaned_output, result.exit_code

        # Timeout - send Ctrl+C only when explicitly requested.
        if timeout is not None:
            self.send(b"\x03")
        cleaned_output = self._clean_pty_output(bytes(raw_output), command)
        return cleaned_output, -1

    def _allocate_backend_command_seq(self) -> int:
        seq = self._next_backend_command_seq
        self._next_backend_command_seq -= 1
        return seq

    def _wait_for_completed_result(
        self, command_seq: int, timeout: Optional[float]
    ) -> CommandResult | None:
        deadline = None if timeout is None else time.monotonic() + timeout
        with self._completion_condition:
            while True:
                result = self._pop_completed_result(command_seq)
                if result is not None:
                    return result

                if not self.is_running:
                    return None

                if deadline is None:
                    self._completion_condition.wait(timeout=0.05)
                    continue

                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    return None
                self._completion_condition.wait(timeout=min(remaining, 0.05))

    def _pop_completed_result(self, command_seq: int) -> CommandResult | None:
        for index, result in enumerate(self._completed_results):
            if result.command_seq == command_seq:
                return self._completed_results.pop(index)
        return None

    def stop(self) -> None:
        """Stop bash and close PTY."""
        self._running = False

        if self._child_pid is not None:
            try:
                os.kill(self._child_pid, signal.SIGTERM)
                time.sleep(0.1)
                os.waitpid(self._child_pid, os.WNOHANG)
            except (ProcessLookupError, ChildProcessError, OSError):
                pass

        if self._master_fd is not None:
            try:
                os.close(self._master_fd)
            except OSError:
                pass

        if self._control_fd is not None:
            try:
                os.close(self._control_fd)
            except OSError:
                pass

        if self._control_write_fd is not None:
            try:
                os.close(self._control_write_fd)
            except OSError:
                pass
        if self._metadata_read_fd is not None:
            try:
                os.close(self._metadata_read_fd)
            except OSError:
                pass
        if self._metadata_write_fd is not None:
            try:
                os.close(self._metadata_write_fd)
            except OSError:
                pass

        self._master_fd = None
        self._control_fd = None
        self._control_write_fd = None
        self._metadata_read_fd = None
        self._metadata_write_fd = None
        self._child_pid = None

    @staticmethod
    def _serialize_command_metadata(
        *,
        command: str,
        source: str,
        command_seq: int | None,
        submission_id: str | None = None,
    ) -> bytes:
        payload = {
            "submission_id": submission_id,
            "command_seq": command_seq,
            "source": source,
            "original_command": command,
        }
        return (json.dumps(payload, separators=(",", ":")) + "\n").encode("utf-8")

    def _write_command_metadata_pipe(self, payload: bytes) -> None:
        fd = getattr(self, "_metadata_write_fd", None)
        if fd is None:
            return

        view = memoryview(payload)
        while view:
            try:
                written = os.write(fd, view)
            except OSError:
                return
            if written <= 0:
                return
            view = view[written:]

    def _write_command_metadata(
        self,
        *,
        command: str,
        source: str,
        command_seq: int | None,
        submission_id: str | None = None,
    ) -> None:
        payload = self._serialize_command_metadata(
            command=command,
            source=source,
            command_seq=command_seq,
            submission_id=submission_id,
        )
        self._write_command_metadata_pipe(payload)

    def __enter__(self) -> "PTYManager":
        self.start()
        return self

    def __exit__(self, exc_type, exc_val, exc_tb) -> None:
        self.stop()
