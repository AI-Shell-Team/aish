"""Shell command registry for aish.

This module provides a registry of built-in commands that cannot be executed
with subprocess.run because they require shell state manipulation or PTY.
"""

from typing import Dict, Optional, Set

from .handlers import BuiltinHandlers, BuiltinResult, DirectoryStack

# Commands that modify shell state and need special handling
# Also includes commands that are shell built-ins (not executable via subprocess)
STATE_MODIFYING_COMMANDS: Set[str] = {
    "cd",
    "pushd",
    "popd",
    "export",
    "unset",
    "dirs",
    "pwd",
    "history",
}

# Commands that require PTY for proper execution
PTY_REQUIRING_COMMANDS: Set[str] = {
    "su",
    "sudo",
}

# Commands that should be rejected (not supported in tool context)
REJECTED_COMMANDS: Set[str] = {
    "exit",
    "logout",
}


class BuiltinRegistry:
    """Registry for built-in command detection and handling.

    This class provides utilities to:
    1. Detect if a command is a built-in that requires special handling
    2. Execute built-in commands through the appropriate handler
    3. Provide helpful error messages for unsupported commands
    """

    @staticmethod
    def is_state_modifying_command(command: str) -> bool:
        """Check if a command modifies shell state.

        These commands cannot be executed with subprocess.run because
        they need to modify the parent shell's environment.

        Args:
            command: The command string to check

        Returns:
            True if the command modifies shell state
        """
        cmd_parts = command.strip().split()
        if not cmd_parts:
            return False

        cmd_name = cmd_parts[0].lower()
        return cmd_name in STATE_MODIFYING_COMMANDS

    @staticmethod
    def is_pty_requiring_command(command: str) -> bool:
        """Check if a command requires PTY for proper execution.

        These commands need an interactive terminal and cannot be
        executed with subprocess.run in capture_output mode.

        Args:
            command: The command string to check

        Returns:
            True if the command requires PTY
        """
        cmd_parts = command.strip().split()
        if not cmd_parts:
            return False

        cmd_name = cmd_parts[0].lower()

        # Check for sudo/su commands that spawn interactive shells
        if cmd_name in PTY_REQUIRING_COMMANDS:
            if len(cmd_parts) >= 2:
                # su commands: su, su -, su user, su - user
                if cmd_name == "su":
                    return True
                # sudo with shell flag or user switch: sudo -i, sudo su, sudo -u user
                elif cmd_name == "sudo":
                    second_cmd = cmd_parts[1].lower()
                    if second_cmd in ["-i", "su", "-u", "-s"]:
                        return True

        return False

    @staticmethod
    def is_rejected_command(command: str) -> bool:
        """Check if a command should be rejected in tool context.

        These commands would not work properly when executed via tool
        and should return an error message instead.

        Args:
            command: The command string to check

        Returns:
            True if the command should be rejected
        """
        cmd_parts = command.strip().split()
        if not cmd_parts:
            return False

        cmd_name = cmd_parts[0].lower()
        return cmd_name in REJECTED_COMMANDS

    @staticmethod
    def is_builtin_command(command: str) -> bool:
        """Check if a command is any type of built-in.

        Args:
            command: The command string to check

        Returns:
            True if the command is a built-in that requires special handling
        """
        return (
            BuiltinRegistry.is_state_modifying_command(command)
            or BuiltinRegistry.is_pty_requiring_command(command)
            or BuiltinRegistry.is_rejected_command(command)
        )

    @staticmethod
    def get_rejected_command_message(command: str) -> Optional[str]:
        """Get error message for a rejected command.

        Args:
            command: The command string

        Returns:
            Error message or None if command is not rejected
        """
        cmd_parts = command.strip().split()
        if not cmd_parts:
            return None

        cmd_name = cmd_parts[0].lower()

        if cmd_name in ("exit", "logout"):
            return (
                "Error: The 'exit' command cannot be used through the AI tool. "
                "This command would exit the entire shell session. "
                "If you want to end the conversation, just say so or use Ctrl+C."
            )

        return None

    @staticmethod
    def get_pty_command_message(command: str) -> Optional[str]:
        """Get message for a PTY-requiring command.

        Args:
            command: The command string

        Returns:
            Informational message or None if command doesn't require PTY
        """
        if not BuiltinRegistry.is_pty_requiring_command(command):
            return None

        return (
            f"Notice: The command '{command.split()[0]}' requires an interactive terminal. "
            "This command has been executed with PTY support for proper handling."
        )

    @classmethod
    def execute_builtin(
        cls,
        command: str,
        cwd: str,
        directory_stack: DirectoryStack,
        env_manager=None,
    ) -> Optional[BuiltinResult]:
        """Execute a built-in command.

        Args:
            command: The command string to execute
            cwd: Current working directory
            directory_stack: Directory stack for pushd/popd
            env_manager: EnvironmentManager instance (for export/unset)

        Returns:
            BuiltinResult if command was handled, None if not a built-in
        """
        cmd_parts = command.strip().split()
        if not cmd_parts:
            return None

        cmd_name = cmd_parts[0].lower()

        # State modifying commands
        if cmd_name == "cd":
            return BuiltinHandlers.handle_cd(command, cwd, directory_stack)
        elif cmd_name == "pushd":
            return BuiltinHandlers.handle_pushd(command, cwd, directory_stack)
        elif cmd_name == "popd":
            return BuiltinHandlers.handle_popd(command, cwd, directory_stack)
        elif cmd_name == "dirs":
            return BuiltinHandlers.handle_dirs(command, cwd, directory_stack)
        elif cmd_name == "pwd":
            return BuiltinHandlers.handle_pwd(command, cwd)
        elif cmd_name == "export" and env_manager:
            return BuiltinHandlers.handle_export(
                command,
                env_manager.get_exported_vars,
                env_manager.set_var,
                env_manager.remove_export,
            )
        elif cmd_name == "unset" and env_manager:
            return BuiltinHandlers.handle_unset(
                command,
                env_manager.unset_var,
            )

        # Not a built-in we handle
        return None


# Pre-built command sets for easy access
ALL_BUILTIN_COMMANDS = (
    STATE_MODIFYING_COMMANDS | PTY_REQUIRING_COMMANDS | REJECTED_COMMANDS
)

# Command description for help messages
COMMAND_DESCRIPTIONS: Dict[str, str] = {
    "cd": "Change the current working directory",
    "pushd": "Push directory to stack and change to it",
    "popd": "Pop directory from stack and change to it",
    "dirs": "Display the directory stack",
    "pwd": "Print the current working directory",
    "export": "Set environment variable for export to child processes",
    "unset": "Unset environment variables",
    "history": "Display or manipulate command history",
    "su": "Substitute user identity (requires PTY)",
    "sudo": "Execute command as another user (requires PTY for interactive use)",
    "exit": "Exit the shell (not supported in tool context)",
}
