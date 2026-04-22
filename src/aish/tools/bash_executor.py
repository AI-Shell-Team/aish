"""
Unified Bash Executor - Uses trap EXIT + temp file for state capture

Core design:
1. Unified state capture mechanism (trap EXIT + temp file)
2. Two execution modes: normal (subprocess.run) and PTY (pty + select)
3. No command syntax parsing, no string matching - let bash handle everything

This module uses the shared state_capture module for state management.
"""

import os
import subprocess
import sys
import termios
from typing import Any, Dict, Optional, Tuple

from ..shell.environment import sanitize_subprocess_loader_env
from .shell_state_capture import (apply_changes, cleanup_state_file,
                                  create_state_file, detect_changes,
                                  get_current_state, parse_state_file,
                                  wrap_command_with_state_capture)


class UnifiedBashExecutor:
    """Unified Bash executor with state capture support."""

    def __init__(self, env_manager=None, history_manager=None):
        self.env_manager = env_manager
        self.history_manager = history_manager

    @staticmethod
    def _build_passthrough_stdin_termios(settings: list[Any]) -> list[Any]:
        """Build a raw-like stdin mode that keeps bytes unchanged for PTY forwarding."""
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
        new_settings[3] &= ~(
            termios.ECHO | termios.ICANON | termios.IEXTEN | termios.ISIG
        )
        new_settings[6][termios.VMIN] = 1
        new_settings[6][termios.VTIME] = 0
        return new_settings

    def execute(
        self,
        command: str,
        source: str = "ai",
        timeout: Optional[int] = None,
        use_pty: bool = False,
        cancel_event: Optional[Any] = None,
    ) -> Tuple[bool, str, str, int, Dict]:
        """
        Execute command and detect state changes.

        Args:
            command: Shell command to execute
            source: Command source ("ai" or "user")
            timeout: Optional timeout in seconds; None disables timeout
            use_pty: Whether to use PTY (for interactive commands)
            cancel_event: Cancellation event (PTY mode only)

        Returns:
            (success, stdout, stderr, returncode, changes)
        """
        # Every shell command may spawn arbitrary system binaries, so this path
        # always uses the sanitized subprocess environment instead of trying to
        # classify commands as "internal" or "external".
        if self.env_manager:
            env_vars = self.env_manager.get_subprocess_env()
        else:
            env_vars = sanitize_subprocess_loader_env(os.environ.copy())

        # Get state before execution
        old_state = get_current_state(env_vars)

        # Create temp file for state storage
        state_file = create_state_file()

        try:
            if use_pty:
                # PTY execution (interactive commands)
                success, stdout, stderr, returncode = self._execute_with_pty(
                    command, state_file, old_state["pwd"], env_vars, cancel_event
                )
            else:
                # Normal execution (AI tool calls)
                success, stdout, stderr, returncode = self._execute_normal(
                    command, state_file, old_state["pwd"], env_vars, timeout
                )

            # Read new state (unified logic)
            new_state = parse_state_file(state_file)

            # Detect changes (unified logic)
            changes = detect_changes(old_state, new_state)

            # Apply changes to parent process (unified logic)
            apply_changes(changes, self.env_manager)

            # Record to history
            if self.history_manager:
                self._record_history(command, source, returncode, stdout, stderr)

            return (success, stdout, stderr, returncode, changes)

        finally:
            cleanup_state_file(state_file)

    def _execute_normal(
        self,
        command: str,
        state_file: str,
        cwd: str,
        env_vars: Dict[str, str],
        timeout: Optional[int],
    ) -> Tuple[bool, str, str, int]:
        """Normal execution using subprocess.run."""
        wrapped_command = wrap_command_with_state_capture(command, state_file)

        try:
            run_kwargs: Dict[str, Any] = {
                "args": wrapped_command,
                "shell": True,
                "executable": "/bin/bash",
                "capture_output": True,
                "text": False,
                "cwd": cwd,
                "env": env_vars,
            }
            if timeout is not None:
                run_kwargs["timeout"] = timeout

            result = subprocess.run(
                **run_kwargs,
            )

            stdout = (result.stdout or b"").decode("utf-8", errors="replace")
            stderr = (result.stderr or b"").decode("utf-8", errors="replace")

            return (result.returncode == 0, stdout, stderr, result.returncode)

        except subprocess.TimeoutExpired:
            return False, "", "Command execution timed out", -1
        except Exception as e:
            return False, "", f"Error: {str(e)}", -1

    def _execute_with_pty(
        self,
        command: str,
        state_file: str,
        cwd: str,
        env_vars: Dict[str, str],
        cancel_event: Optional[Any],
    ) -> Tuple[bool, str, str, int]:
        """PTY execution for interactive commands."""
        import pty
        import select
        import signal

        wrapped_command = wrap_command_with_state_capture(command, state_file)

        master_fd = None
        slave_fd = None
        process = None
        old_settings = None

        try:
            # Save terminal settings
            try:
                old_settings = termios.tcgetattr(sys.stdin)
            except (OSError, termios.error):
                old_settings = None

            # Create PTY
            master_fd, slave_fd = pty.openpty()

            # Set non-blocking mode
            import fcntl

            flags = fcntl.fcntl(master_fd, fcntl.F_GETFL)
            fcntl.fcntl(master_fd, fcntl.F_SETFL, flags | os.O_NONBLOCK)

            def preexec_setup():
                os.setsid()
                # Set up controlling terminal
                os.close(slave_fd)
                os.close(0)
                os.close(1)
                os.close(2)
                os.open(os.ttyname(0), os.O_RDWR)
                os.open(os.ttyname(0), os.O_RDWR)
                os.open(os.ttyname(0), os.O_RDWR)

            # Start process
            process = subprocess.Popen(
                wrapped_command,
                shell=True,
                executable="/bin/bash",
                stdin=slave_fd,
                stdout=slave_fd,
                stderr=slave_fd,  # PTY merges stderr
                preexec_fn=preexec_setup,
                env=env_vars,
            )

            os.close(slave_fd)
            slave_fd = None

            # Set raw mode
            if old_settings:
                new_settings = self._build_passthrough_stdin_termios(
                    termios.tcgetattr(sys.stdin.fileno())
                )
                termios.tcsetattr(sys.stdin.fileno(), termios.TCSANOW, new_settings)

            # I/O multiplexing
            stdout_buffer = b""
            KEEP_LEN = 1024 * 16

            while True:
                process_alive = process.poll() is None

                # Check cancellation
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
                    return (
                        False,
                        stdout_buffer.decode("utf-8", errors="replace"),
                        "",
                        -1,
                    )

                # Build read list
                read_list = []
                if process_alive:
                    read_list.append(sys.stdin)
                read_list.append(master_fd)

                if not read_list:
                    break

                try:
                    ready, _, _ = select.select(read_list, [], [], 0.1)
                except (OSError, ValueError):
                    break

                # Handle stdin -> PTY
                if sys.stdin in ready and process_alive:
                    try:
                        data = os.read(sys.stdin.fileno(), 1024)
                        if data:
                            os.write(master_fd, data)
                    except (OSError, IOError):
                        pass

                # Handle PTY -> stdout
                if master_fd in ready:
                    try:
                        data = os.read(master_fd, 1024)
                        if data:
                            stdout_buffer += data
                            if len(stdout_buffer) > KEEP_LEN:
                                stdout_buffer = stdout_buffer[-KEEP_LEN:]
                            os.write(sys.stdout.fileno(), data)
                            sys.stdout.flush()
                        else:
                            break
                    except OSError:
                        break

            # Wait for process to end
            returncode = process.wait(timeout=0.1)
            if returncode is None:
                returncode = 0

            return (
                returncode == 0,
                stdout_buffer.decode("utf-8", errors="replace"),
                "",
                returncode,
            )

        except Exception as e:
            return False, "", f"Error: {str(e)}", -1

        finally:
            # Restore terminal settings
            if old_settings:
                termios.tcsetattr(sys.stdin.fileno(), termios.TCSANOW, old_settings)

            # Clean up file descriptors
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

    def _record_history(
        self, command: str, source: str, returncode: int, stdout: str, stderr: str
    ):
        """Record command to history (async)."""
        try:
            import asyncio

            loop = asyncio.get_event_loop()
            if loop.is_running():
                asyncio.create_task(
                    self.history_manager.add_entry(
                        command=command,
                        source=source,
                        returncode=returncode,
                        stdout=stdout,
                        stderr=stderr,
                    )
                )
        except Exception:
            # If async not available, skip history recording
            pass
