"""PTY command execution implementation for the shell core."""

from __future__ import annotations

import fcntl
import os
import pty
import select
import signal
import subprocess
import sys
import termios
import threading
import time
from typing import Any

from anyio import to_thread

from ...i18n import t
from ...offload.pty_output_offload import PtyOutputOffload
from ...tools.shell_state_capture import (apply_changes, cleanup_state_file,
                                          create_state_file, detect_changes,
                                          get_current_state, parse_state_file,
                                          wrap_command_with_state_capture)
from ..types import CommandResult, CommandStatus


def _build_passthrough_stdin_termios(settings: list[Any]) -> list[Any]:
    """Build a raw-like stdin mode that preserves input bytes for PTY forwarding."""
    new_settings = list(settings)
    if len(new_settings) >= 7 and isinstance(new_settings[6], list):
        new_settings[6] = list(new_settings[6])

    iflag_mask = (
        termios.BRKINT
        | termios.ICRNL
        | termios.INPCK
        | termios.ISTRIP
        | termios.IXON
        | getattr(termios, "INLCR", 0)
        | getattr(termios, "IGNCR", 0)
    )
    new_settings[0] &= ~iflag_mask
    new_settings[1] &= ~termios.OPOST
    new_settings[2] |= termios.CS8
    new_settings[3] &= ~(termios.ECHO | termios.ICANON | termios.IEXTEN | termios.ISIG)
    new_settings[6][termios.VMIN] = 1
    new_settings[6][termios.VTIME] = 0
    return new_settings


async def execute_command_with_pty(shell: Any, command: str) -> CommandResult:
    """Execute a command with PTY support and enhanced error handling."""
    self = shell
    keep_len_raw = getattr(self.config, "pty_output_keep_bytes", 4096)
    try:
        keep_len = int(keep_len_raw)
    except (TypeError, ValueError):
        keep_len = 4096
    if keep_len <= 0:
        keep_len = 4096

    offload_base_dir = None
    bash_offload_config = getattr(self.config, "bash_output_offload", None)
    if bash_offload_config is not None:
        offload_base_dir = getattr(bash_offload_config, "base_dir", None) or None

    session_uuid = "unknown-session"
    if self.history_manager and hasattr(self.history_manager, "get_session_uuid"):
        try:
            session_uuid = self.history_manager.get_session_uuid()
        except Exception:
            session_uuid = "unknown-session"

    try:
        command_cwd = os.getcwd()
    except Exception:
        command_cwd = ""

    # State capture setup for cd/export support in compound commands
    state_file = create_state_file()
    env_vars = (
        self.env_manager.get_exported_vars() if self.env_manager else dict(os.environ)
    )
    old_state = get_current_state(env_vars)

    def build_pty_offload_payload(
        offload_result, *, default_reason: str = "not_offloaded"
    ) -> dict[str, Any]:
        stdout_state = offload_result.stdout
        stderr_state = offload_result.stderr
        status = "inline"
        if stdout_state.status == "offloaded" or stderr_state.status == "offloaded":
            status = "offloaded"
        elif stdout_state.status == "failed" or stderr_state.status == "failed":
            status = "failed"

        payload: dict[str, Any] = {
            "status": status,
            "stdout_status": stdout_state.status or "inline",
            "stderr_status": stderr_state.status or "inline",
            "stdout_path": stdout_state.path or "",
            "stdout_clean_path": getattr(stdout_state, "clean_path", "") or "",
            "stderr_path": stderr_state.path or "",
            "stderr_clean_path": getattr(stderr_state, "clean_path", "") or "",
            "meta_path": offload_result.meta_path or "",
            "keep_bytes": keep_len,
        }
        stdout_clean_error = getattr(stdout_state, "clean_error", "") or ""
        stderr_clean_error = getattr(stderr_state, "clean_error", "") or ""
        if stdout_clean_error:
            payload["stdout_clean_error"] = stdout_clean_error
        if stderr_clean_error:
            payload["stderr_clean_error"] = stderr_clean_error

        if status == "offloaded":
            payload["hint"] = (
                f"showing last {keep_len} bytes; prefer clean offload paths for full output, fallback to raw paths"
            )
        elif status == "failed":
            payload["hint"] = f"showing last {keep_len} bytes (offload failed)"
            if stdout_state.error:
                payload["stdout_error"] = stdout_state.error
            if stderr_state.error:
                payload["stderr_error"] = stderr_state.error
        else:
            payload["reason"] = default_reason
            payload["hint"] = "full output kept inline"

        return payload

    def setup_controlling_terminal(slave_fd: int) -> None:
        # Set up controlling terminal for sudo compatibility
        try:
            # Set the slave PTY as the controlling terminal
            fcntl.ioctl(slave_fd, termios.TIOCSCTTY, 0)
        except (OSError, IOError):
            pass  # Continue without controlling terminal setup

    def handle_stderr(stderr_pipe, stderr_buffer, stderr_truncated, KEEP_LEN):
        data = os.read(stderr_pipe, 1024)
        if data:
            stderr_buffer += data
            # Keep only last KEEP_LEN characters
            if len(stderr_buffer) > KEEP_LEN:
                stderr_buffer = stderr_buffer[-KEEP_LEN:]
                stderr_truncated = True
            os.write(
                sys.stderr.fileno(),
                data.replace(b"\n", b"\r\n"),
            )
            sys.stderr.flush()
            return True, stderr_buffer, stderr_truncated
        return False, stderr_buffer, stderr_truncated

    def extract_last_executable_command(command: str) -> str | None:
        """从复合命令中提取最后执行的命令，用于判断是否需要 TTY

        原理：bash 执行顺序是从左到右，最后执行的命令才是需要交互的

        例如：
        - "ls ; more file" → "more"
        - "cat x | less"   → "less"
        - "cd / && pwd"    → "pwd"
        - "true || echo x" → "echo"
        - "ls > /dev/null && cat file | more" → "more"

        处理的分隔符：
        - 管道: |
        - 顺序: ;
        - 逻辑与: &&
        - 逻辑或: ||
        - 后台: &
        """
        if not command:
            return None

        # 按优先级定义分隔符（管道优先，因为管道有特殊语义）
        # 使用正则表达式来正确分割，避免分割引号内的内容
        import re

        # 首先处理管道（管道优先级最高）
        # 管道会连接命令的输出到下一个命令的输入
        pipe_parts = re.split(r"\s*\|\s*", command)
        if len(pipe_parts) > 1:
            # 取管道的最后一部分
            command = pipe_parts[-1]

        # 然后处理其他命令分隔符（按顺序执行）
        # 顺序：;  > /  >> /  < /  && /  || /  &
        # 需要正确处理重定向，避免被重定向的文件名被误认为是命令
        separators_pattern = (
            r"\s*;|\s*&&|\s*\|\||\s*&(?!\d)|(?<!\d)>|(?<!\d)>>|(?<!\d)<|<<"
        )

        parts = re.split(separators_pattern, command)
        if not parts:
            return None

        # 取最后一个非空部分
        for part in reversed(parts):
            part = part.strip()
            if part:
                # 提取命令名（第一个单词）
                # 处理可能的引号包裹
                match = re.match(r'^["\']?([^\s"\']+)', part)
                if match:
                    cmd_name = match.group(1).lower()
                    # 去掉路径前缀（如 /usr/bin/vi -> vi）
                    cmd_name = os.path.basename(cmd_name)
                    return cmd_name

                # 如果正则匹配失败，使用简单的 split
                cmd_name = part.split()[0].strip().lower() if part.split() else ""
                if cmd_name:
                    cmd_name = os.path.basename(cmd_name)
                    return cmd_name

        return None

    def run_with_pty(cancel_event=None) -> CommandResult:
        """Run command with PTY support and cancellation checking"""
        master_fd = None
        slave_fd = None
        stderr_pipe = None
        process = None
        old_settings = None
        last_stdout_byte: bytes | None = None
        last_stderr_byte: bytes | None = None

        try:
            # Save terminal settings early to ensure we can restore them
            try:
                old_settings = termios.tcgetattr(sys.stdin)
            except (OSError, termios.error):
                old_settings = None

            # Create master and slave file descriptors for stdout/stdin
            master_fd, slave_fd = pty.openpty()

            # Set master_fd to non-blocking mode to prevent write blocking when PTY buffer is full
            # This fixes the issue where pasting large content in vim causes the shell to hang
            flags = fcntl.fcntl(master_fd, fcntl.F_GETFL)
            fcntl.fcntl(master_fd, fcntl.F_SETFL, flags | os.O_NONBLOCK)

            # NOTE: Do NOT set PTY slave terminal mode here.
            # Keeping PTY slave in default canonical mode allows programs like 'su'
            # to read passwords correctly (waiting for newline instead of single char).
            # Interactive programs (vim, top, ssh, etc.) will set their own mode.
            # The following code was commented out to fix 'su' password input issue:
            # # CRITICAL: Set PTY slave to non-canonical mode IMMEDIATELY after creation
            # # This ensures the terminal is in correct mode before any process uses it
            # try:
            #     settings = termios.tcgetattr(slave_fd)
            #     # Clear canonical mode - this is KEY for more/less to work
            #     settings[3] &= ~termios.ICANON
            #     # Disable echo and signal generation
            #     settings[3] &= ~termios.ECHO
            #     settings[3] &= ~termios.ISIG
            #     # Set to read 1 character immediately
            #     settings[6][termios.VMIN] = 1
            #     settings[6][termios.VTIME] = 0
            #     # Apply settings
            #     termios.tcsetattr(slave_fd, termios.TCSANOW, settings)
            # except (OSError, termios.error):
            #     pass  # Continue if termios setup fails

            # Create a pipe for stderr
            stderr_read, stderr_write = os.pipe()
            stderr_pipe = stderr_read

            last_term_size: tuple[int, int] | None = None
            initial_size = self._read_terminal_size()
            if initial_size is not None:
                rows, cols = initial_size
                if self._set_pty_winsize(slave_fd, rows, cols):
                    last_term_size = initial_size

            def preexec_setup() -> None:
                # Create new session
                os.setsid()
                # Set up controlling terminal properly
                setup_controlling_terminal(slave_fd)

            # Use bash to parse and execute the command directly
            # bash handles all escaping, wildcards, and special characters correctly
            safe_command = command

            # For interactive commands like more/less, we need special handling
            # Use script command to ensure proper terminal environment
            needs_script = False

            # Use the new command parser to extract the last executable command
            # This properly handles compound commands like "ls ; more file"
            last_cmd = extract_last_executable_command(command)

            if last_cmd:
                # Pagers that need TTY for user input
                if last_cmd in ["more", "less", "most", "pg", "view", "ssh"]:
                    needs_script = True
                # Commands that spawn interactive shells needing proper TTY for prompt display
                # sudo su, su, sudo -i, etc. spawn shells that need full TTY environment
                # to properly display prompts like bash PS1
                elif last_cmd in ["su", "sudo"]:
                    # Check if this is sudo/su with shell operations (su, sudo su, sudo -i, etc.)
                    # These commands spawn interactive shells that need proper TTY
                    cmd_parts = command.strip().split()
                    if len(cmd_parts) >= 2:
                        # su commands: su, su -, su user, su - user
                        if last_cmd == "su":
                            needs_script = True
                        # sudo with shell flag or user switch: sudo -i, sudo su, sudo -u user
                        elif last_cmd == "sudo":
                            second_cmd = cmd_parts[1].lower()
                            if second_cmd in ["-i", "su", "-u", "-s"]:
                                needs_script = True

            if needs_script:
                # Wrap with script for proper terminal handling
                # script -q: quiet mode, -c: run command
                # This ensures the pager has proper terminal access for user input
                # Use shlex.quote to properly escape the command for shell
                import shlex

                # Use /dev/null as typescript file to avoid permission issues
                # script command tries to create 'typescript' file by default
                safe_command = f"script -q -c {shlex.quote(safe_command)} /dev/null"

            # Identify session-type commands that need Ctrl+C forwarding instead of SIGINT
            # These commands maintain an interactive session where Ctrl+C should be forwarded
            # to the remote end, not used to kill the local process
            is_session_command = (
                last_cmd in ["ssh", "telnet", "nc", "netcat"] if last_cmd else False
            )

            # Wrap command with state capture for cd/export support in compound commands
            safe_command = wrap_command_with_state_capture(safe_command, state_file)

            # Start the process with PTY for stdin/stdout and pipe for stderr
            process = subprocess.Popen(
                safe_command,
                shell=True,
                executable="/bin/bash",
                stdin=slave_fd,
                stdout=slave_fd,
                stderr=stderr_write,  # Use pipe for stderr
                preexec_fn=preexec_setup,
                env=self.env_manager.get_exported_vars(),  # Use exported environment variables
            )

            # Close slave fd and stderr write end in parent
            os.close(slave_fd)
            os.close(stderr_write)
            slave_fd = None

            # Force one post-spawn resize sync/signal so fullscreen TUIs
            # start with correct geometry instead of waiting for first refresh.
            if master_fd is not None:
                last_term_size = self._sync_pty_resize(
                    master_fd=master_fd,
                    process=process,
                    last_size=None,
                )

            # Modify terminal settings (already saved above)
            if old_settings is not None:
                new_settings = _build_passthrough_stdin_termios(
                    termios.tcgetattr(sys.stdin.fileno())
                )
                termios.tcsetattr(sys.stdin.fileno(), termios.TCSANOW, new_settings)

            # Keep last N bytes in memory and offload the rest to disk.
            KEEP_LEN = keep_len
            stdout_buffer = b""
            stderr_buffer = b""
            stdout_truncated = False
            stderr_truncated = False
            offload_writer = PtyOutputOffload(
                command=command,
                session_uuid=session_uuid,
                cwd=command_cwd,
                keep_len=KEEP_LEN,
                base_dir=offload_base_dir,
            )

            def build_truncated_prefix(stream_name: str, offload_state) -> str:
                if offload_state.status == "offloaded" and offload_state.path:
                    return (
                        f"... [{stream_name} truncated, offloaded to {offload_state.path}, "
                        f"showing last {KEEP_LEN} bytes] ...\n"
                    )
                if offload_state.status == "failed":
                    error_text = offload_state.error or "unknown error"
                    return (
                        f"... [{stream_name} truncated, offload failed: {error_text}, "
                        f"showing last {KEEP_LEN} bytes] ...\n"
                    )
                return f"... [{stream_name} truncated, showing last {KEEP_LEN} bytes] ...\n"

            # Write buffer for non-blocking writes to master_fd
            # This prevents blocking when PTY buffer is full (e.g., when pasting large content in vim)
            write_buffer = b""

            # Track FD open states; continue until they are drained/closed
            stdout_open = True
            stderr_open = True

            try:
                while True:
                    process_alive = process.poll() is None

                    # Exit condition: process exited AND all fds drained/closed
                    if (not process_alive) and (not stdout_open) and (not stderr_open):
                        break

                    # Try to flush write buffer before doing anything else
                    # This is critical for non-blocking writes - we must drain the buffer
                    # before accepting new input, otherwise the buffer can grow unbounded
                    if write_buffer and stdout_open:
                        try:
                            # Write as much as possible without blocking
                            bytes_written = os.write(master_fd, write_buffer)
                            if bytes_written > 0:
                                write_buffer = write_buffer[bytes_written:]
                        except BlockingIOError:
                            # PTY buffer is full, will retry next iteration
                            pass
                        except (OSError, IOError):
                            # PTY closed, stop trying to write
                            write_buffer = b""

                    # Cancellation
                    if cancel_event and cancel_event.is_set():
                        try:
                            if process_alive:
                                os.killpg(process.pid, signal.SIGTERM)
                                process.wait(timeout=1)
                        except (OSError, ProcessLookupError, subprocess.TimeoutExpired):
                            try:
                                os.killpg(process.pid, signal.SIGKILL)
                            except (OSError, ProcessLookupError):
                                pass
                        offload_result = offload_writer.finalize(
                            stdout_tail=stdout_buffer,
                            stderr_tail=stderr_buffer,
                            return_code=-1,
                        )
                        cancel_stdout = stdout_buffer.decode("utf-8", errors="replace")
                        if stdout_truncated:
                            cancel_stdout = (
                                f"{build_truncated_prefix('stdout', offload_result.stdout)}"
                                f"{cancel_stdout}"
                            )
                        cancel_stderr = "Command cancelled by user"
                        if stderr_truncated:
                            cancel_stderr = (
                                f"{build_truncated_prefix('stderr', offload_result.stderr)}"
                                f"{cancel_stderr}"
                            )

                        if (
                            offload_result.stdout.status == "offloaded"
                            or offload_result.stderr.status == "offloaded"
                        ):
                            self.logger.info(
                                "pty output offloaded: session=%s return_code=%s stdout_path=%s stderr_path=%s meta_path=%s",
                                session_uuid,
                                -1,
                                offload_result.stdout.path,
                                offload_result.stderr.path,
                                offload_result.meta_path,
                            )
                        if (
                            offload_result.stdout.status == "failed"
                            or offload_result.stderr.status == "failed"
                        ):
                            self.logger.warning(
                                "pty output offload failed: session=%s return_code=%s stdout_error=%s stderr_error=%s",
                                session_uuid,
                                -1,
                                offload_result.stdout.error,
                                offload_result.stderr.error,
                            )
                        return CommandResult(
                            CommandStatus.CANCELLED,
                            -1,
                            cancel_stdout,
                            cancel_stderr,
                            offload=build_pty_offload_payload(
                                offload_result,
                                default_reason="cancelled_not_offloaded",
                            ),
                        )

                    # Build dynamic read list
                    read_list = []
                    if process_alive and stdout_open:
                        # Only accept stdin while the process is alive
                        read_list.append(sys.stdin)
                    if stdout_open:
                        read_list.append(master_fd)
                    if stderr_open:
                        read_list.append(stderr_pipe)

                    if not read_list:
                        # Nothing to read; short sleep to avoid busy loop
                        time.sleep(0.05)
                        continue

                    if stdout_open:
                        last_term_size = self._sync_pty_resize(
                            master_fd=master_fd,
                            process=process,
                            last_size=last_term_size,
                        )

                    try:
                        ready, _, _ = select.select(read_list, [], [], 0.1)
                    except (OSError, ValueError):
                        break

                    if sys.stdin in ready and process_alive and stdout_open:
                        try:
                            data = os.read(sys.stdin.fileno(), 1024)
                            if data:
                                # Check for Ctrl+C (ETX character)
                                # For session commands (ssh, telnet, etc.), forward the character
                                # directly instead of sending SIGINT, so the remote end can handle it
                                # For regular commands, send SIGINT to the process group
                                if b"\x03" in data:
                                    if is_session_command:
                                        # Forward Ctrl+C character directly for session commands
                                        # This allows ssh/telnet to pass Ctrl+C to the remote shell
                                        pass  # Character will be added to write_buffer below
                                    else:
                                        # Send SIGINT to process group for regular commands
                                        try:
                                            os.killpg(process.pid, signal.SIGINT)
                                        except (OSError, ProcessLookupError):
                                            pass
                                # Add data to write buffer instead of writing directly
                                # The write buffer will be flushed in the next loop iteration
                                write_buffer += data
                        except (OSError, IOError):
                            # Ignore stdin errors during teardown
                            pass

                    if master_fd in ready and stdout_open:
                        try:
                            data = os.read(master_fd, 1024)
                            if data:
                                stdout_buffer += data
                                if len(stdout_buffer) > KEEP_LEN:
                                    overflow = stdout_buffer[:-KEEP_LEN]
                                    stdout_buffer = stdout_buffer[-KEEP_LEN:]
                                    stdout_truncated = True
                                    offload_writer.append_overflow(
                                        stream_name="stdout",
                                        overflow=overflow,
                                    )
                                os.write(sys.stdout.fileno(), data)
                                sys.stdout.flush()
                                last_stdout_byte = data[-1:]
                            else:
                                # EOF on PTY master (rare), treat as closed
                                stdout_open = False
                        except OSError:
                            # EIO often indicates slave closed
                            stdout_open = False

                    if stderr_pipe in ready and stderr_open:
                        try:
                            data = os.read(stderr_pipe, 1024)
                            if data:
                                stderr_buffer += data
                                if len(stderr_buffer) > KEEP_LEN:
                                    overflow = stderr_buffer[:-KEEP_LEN]
                                    stderr_buffer = stderr_buffer[-KEEP_LEN:]
                                    stderr_truncated = True
                                    offload_writer.append_overflow(
                                        stream_name="stderr",
                                        overflow=overflow,
                                    )
                                os.write(
                                    sys.stderr.fileno(),
                                    data.replace(b"\n", b"\r\n"),
                                )
                                sys.stderr.flush()
                                last_stderr_byte = data[-1:]
                            else:
                                # Pipe closed
                                stderr_open = False
                        except (OSError, IOError):
                            stderr_open = False

            finally:
                pass  # No signal handling in thread

            # Ensure the process has fully terminated
            try:
                returncode = process.wait(timeout=0.1)
            except subprocess.TimeoutExpired:
                # Should be rare here; force check
                returncode = process.poll()
                if returncode is None:
                    returncode = 0

            # Ensure prompt starts on a fresh line if command didn't end with newline
            try:
                had_output = (last_stdout_byte is not None) or (
                    last_stderr_byte is not None
                )
                if had_output:
                    if (last_stdout_byte not in (b"\n", b"\r")) and (
                        last_stderr_byte not in (b"\n", b"\r")
                    ):
                        os.write(sys.stdout.fileno(), b"\n")
                        sys.stdout.flush()
            except Exception:
                pass

            offload_result = offload_writer.finalize(
                stdout_tail=stdout_buffer,
                stderr_tail=stderr_buffer,
                return_code=returncode,
            )

            # Build final output with truncation indicators and robust decoding
            try:
                final_stdout = stdout_buffer.decode("utf-8", errors="replace")
            except UnicodeDecodeError:
                # Fallback to latin-1 which can decode any byte sequence
                final_stdout = stdout_buffer.decode("latin-1", errors="replace")

            try:
                final_stderr = stderr_buffer.decode("utf-8", errors="replace")
            except UnicodeDecodeError:
                # Fallback to latin-1 which can decode any byte sequence
                final_stderr = stderr_buffer.decode("latin-1", errors="replace")

            if stdout_truncated:
                final_stdout = f"{build_truncated_prefix('stdout', offload_result.stdout)}{final_stdout}"
            if stderr_truncated:
                final_stderr = f"{build_truncated_prefix('stderr', offload_result.stderr)}{final_stderr}"

            if (
                offload_result.stdout.status == "offloaded"
                or offload_result.stderr.status == "offloaded"
            ):
                self.logger.info(
                    "pty output offloaded: session=%s return_code=%s stdout_path=%s stderr_path=%s meta_path=%s",
                    session_uuid,
                    returncode,
                    offload_result.stdout.path,
                    offload_result.stderr.path,
                    offload_result.meta_path,
                )
            if (
                offload_result.stdout.status == "failed"
                or offload_result.stderr.status == "failed"
            ):
                self.logger.warning(
                    "pty output offload failed: session=%s return_code=%s stdout_error=%s stderr_error=%s",
                    session_uuid,
                    returncode,
                    offload_result.stdout.error,
                    offload_result.stderr.error,
                )

            # Determine status based on exit code and command type
            # Normal case: return code 0 means success
            if returncode == 0:
                status = CommandStatus.SUCCESS
            # Special case: pager commands (git log, man, etc.) that exit due to SIGPIPE (-13)
            # when user presses 'q' to quit should be treated as SUCCESS, not ERROR
            elif returncode == -signal.SIGPIPE:
                # Check if the command likely uses a pager
                # Common pager commands: git log, git diff, man, less, more, etc.
                command_lower = command.lower().strip()
                is_likely_pager = (
                    # Git commands that default to pager
                    command_lower.startswith("git log")
                    or command_lower.startswith("git diff")
                    or command_lower.startswith("git show")
                    or command_lower.startswith("git blame")
                    or
                    # Direct pager commands
                    command_lower.startswith("less ")
                    or command_lower.startswith("more ")
                    or command_lower == "less"
                    or command_lower == "more"
                    or
                    # Man pages
                    command_lower.startswith("man ")
                    or command_lower == "man"
                )
                status = (
                    CommandStatus.SUCCESS if is_likely_pager else CommandStatus.ERROR
                )
            else:
                status = CommandStatus.ERROR

            # Apply state changes (cd/export) from command execution
            new_state = parse_state_file(state_file)
            changes = detect_changes(old_state, new_state)
            apply_changes(changes, self.env_manager)

            return CommandResult(
                status,
                returncode,
                final_stdout,
                final_stderr,
                offload=build_pty_offload_payload(offload_result),
            )

        except UnicodeDecodeError as e:
            # Handle UTF-8 decoding errors specifically
            error_msg = t("shell.error.pty_decode_error", error=str(e))
            return CommandResult(
                CommandStatus.ERROR,
                1,
                "",
                error_msg,
                offload={"status": "inline", "reason": "pty_decode_error"},
            )
        except Exception as e:
            return CommandResult(
                CommandStatus.ERROR,
                1,
                "",
                str(e),
                offload={"status": "inline", "reason": "pty_execution_error"},
            )

        finally:
            # Cleanup resources
            if old_settings is not None:
                try:
                    termios.tcsetattr(sys.stdin, termios.TCSADRAIN, old_settings)
                except (OSError, termios.error):
                    pass

            if master_fd is not None:
                try:
                    os.close(master_fd)
                except OSError:
                    pass

            if slave_fd is not None:
                try:
                    os.close(slave_fd)
                except OSError:
                    pass

            if stderr_pipe is not None:
                try:
                    os.close(stderr_pipe)
                except OSError:
                    pass

            if process and process.poll() is None:
                try:
                    os.killpg(process.pid, signal.SIGTERM)
                    process.wait(timeout=1)
                except (OSError, ProcessLookupError, subprocess.TimeoutExpired):
                    try:
                        os.killpg(process.pid, signal.SIGKILL)
                    except (OSError, ProcessLookupError):
                        pass

            # Cleanup state capture temp file
            cleanup_state_file(state_file)

    # Create a cancellation event for this execution
    cancel_event = threading.Event()
    # Bridge business cancellation into PTY runner
    try:
        self.llm_session.cancellation_token.add_cancellation_callback(cancel_event.set)
    except Exception:
        pass

    # Create task and register it
    self.task_counter += 1

    # Run blocking PTY execution in a worker thread
    future = to_thread.run_sync(run_with_pty, cancel_event)

    try:
        result = await future
        return result
    finally:
        pass
