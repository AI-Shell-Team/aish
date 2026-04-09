"""
Tests for tilde expansion in path handling.

Note: escape_command_with_paths is now a passthrough function.
All escaping and path handling is done by bash directly.
These tests verify that the command is passed through unchanged.
"""


from aish.system_info import escape_command_with_paths


class TestTildeExpansion:
    """Test tilde expansion in path handling utilities."""

    def test_escape_command_with_paths_passthrough(self):
        """Test that escape_command_with_paths passes commands through unchanged."""
        # Since bash handles all escaping, the function should return the command as-is
        commands = [
            "ls ~/Downloads/ai\\ shell\\ deepseek.md",
            'ls "~/Downloads/ai shell deepseek.md"',
            "ls ~/Downloads/ | grep test",
            'cp "~/Documents/my file.txt" ~/Downloads/backup\\ file.txt',
            "ls /path/with\\ spaces/file.txt",
            'cp ~/source\\ file.txt "/absolute/dest path/file.txt"',
        ]

        for command in commands:
            result = escape_command_with_paths(command)
            assert result == command, f"Expected passthrough for: {command}"
