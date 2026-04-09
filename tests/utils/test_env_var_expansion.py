"""
Tests for environment variable expansion in command handling.
"""

import fcntl
import os
import pty
import subprocess
import termios

import pytest

from aish.system_info import escape_command_with_paths


class TestEnvVarExpansion:
    """Test environment variable expansion in command handling."""

    def test_escape_command_preserves_unquoted_var(self):
        """Test that unquoted environment variables are preserved."""
        command = "echo $A"
        result = escape_command_with_paths(command)
        # Should preserve $A without quoting to allow shell expansion
        assert result == "echo $A"

    def test_escape_command_preserves_quoted_var(self):
        """Test that double-quoted environment variables are preserved."""
        command = 'echo "$A"'
        result = escape_command_with_paths(command)
        # Should preserve the original quoting
        assert result == 'echo "$A"'

    def test_escape_command_preserves_braced_var(self):
        """Test that braced environment variables are preserved."""
        command = "echo ${VAR}"
        result = escape_command_with_paths(command)
        # Should preserve ${VAR} without quoting
        assert result == "echo ${VAR}"

    def test_escape_command_preserves_var_with_args(self):
        """Test that commands with variables and other args work correctly."""
        command = "echo $A extra"
        result = escape_command_with_paths(command)
        # Should preserve $A without quoting
        assert result == "echo $A extra"

    def test_escape_command_preserves_multiple_vars(self):
        """Test that multiple environment variables are preserved."""
        command = "echo $A $B $C"
        result = escape_command_with_paths(command)
        # Should preserve all variables without quoting
        assert result == "echo $A $B $C"

    def test_escape_command_quotes_paths_with_vars(self):
        """Test that paths containing variables at the end are handled."""
        command = 'echo "/path/$A"'
        result = escape_command_with_paths(command)
        # Should preserve the original quoting with variable
        assert result == 'echo "/path/$A"'

    def test_escape_command_var_with_default(self):
        """Test that variables with default values are preserved."""
        command = "echo ${VAR:-default}"
        result = escape_command_with_paths(command)
        # Should preserve the braced variable with default
        assert result == "echo ${VAR:-default}"

    def test_actual_bash_expansion_unquoted(self):
        """Test that bash actually expands unquoted variables through PTY."""
        os.environ["TEST_VAR"] = "test_value"

        # Use PTY to simulate actual shell execution
        master_fd, slave_fd = pty.openpty()

        def preexec_setup():
            os.setsid()
            try:
                fcntl.ioctl(slave_fd, termios.TIOCSCTTY, 0)
            except (OSError, IOError):
                pass

        process = subprocess.Popen(
            "echo $TEST_VAR",
            shell=True,
            executable="/bin/bash",
            stdin=slave_fd,
            stdout=slave_fd,
            stderr=subprocess.PIPE,
            preexec_fn=preexec_setup,
            env=os.environ.copy(),
        )

        os.close(slave_fd)

        output = b""
        while True:
            try:
                data = os.read(master_fd, 1024)
                if not data:
                    break
                output += data
            except OSError:
                break

        os.close(master_fd)
        process.wait()

        result = output.decode().replace("\r\n", "\n").strip()
        assert result == "test_value"

    def test_actual_bash_expansion_double_quoted(self):
        """Test that bash expands double-quoted variables through PTY."""
        os.environ["TEST_VAR2"] = "another_value"

        master_fd, slave_fd = pty.openpty()

        def preexec_setup():
            os.setsid()
            try:
                fcntl.ioctl(slave_fd, termios.TIOCSCTTY, 0)
            except (OSError, IOError):
                pass

        process = subprocess.Popen(
            'echo "$TEST_VAR2"',
            shell=True,
            executable="/bin/bash",
            stdin=slave_fd,
            stdout=slave_fd,
            stderr=subprocess.PIPE,
            preexec_fn=preexec_setup,
            env=os.environ.copy(),
        )

        os.close(slave_fd)

        output = b""
        while True:
            try:
                data = os.read(master_fd, 1024)
                if not data:
                    break
                output += data
            except OSError:
                break

        os.close(master_fd)
        process.wait()

        result = output.decode().replace("\r\n", "\n").strip()
        assert result == "another_value"


if __name__ == "__main__":
    pytest.main([__file__])
