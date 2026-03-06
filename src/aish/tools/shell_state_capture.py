"""Shell state capture module - reusable across different executors.

This module provides a unified mechanism for capturing and applying shell state
changes (working directory and environment variables) after command execution.

Usage:
    from aish.tools.shell_state_capture import (
        wrap_command_with_state_capture,
        create_state_file,
        get_current_state,
        parse_state_file,
        detect_changes,
        apply_changes,
        cleanup_state_file,
    )
"""

import os
import tempfile
from typing import Any, Dict, Optional

# Environment variables to ignore when detecting changes
IGNORED_ENV_VARS = {
    "_",  # Python private variable
    "SHLVL",  # Shell nesting level (changes with each execution)
    "MEMORY_PRESSURE_WATCH",  # System auto variable
}


def wrap_command_with_state_capture(command: str, state_file: str) -> str:
    """Wrap a command with state capture logic using trap EXIT.

    This ensures that the working directory and environment variables are
    captured regardless of how the command exits (normal exit, error, signal).

    Args:
        command: The shell command to wrap
        state_file: Path to the temporary file for storing state

    Returns:
        Wrapped command string with state capture logic
    """
    return f"""
# Set up EXIT trap to capture state regardless of exit method
_capture_state() {{
    printf "PWD_AISH_MARKER:%s\\n" "$PWD" > {state_file}
    env >> {state_file}
}}
trap _capture_state EXIT

# User command
{command}

# Ensure trap fires on normal exit
exit $?
"""


def create_state_file() -> str:
    """Create a temporary file for storing shell state.

    Returns:
        Path to the created temporary file
    """
    fd, path = tempfile.mkstemp(prefix="aish_state_")
    os.close(fd)
    return path


def cleanup_state_file(state_file: str) -> None:
    """Clean up the temporary state file.

    Args:
        state_file: Path to the state file to remove
    """
    try:
        os.unlink(state_file)
    except Exception:
        pass


def get_current_state(env_vars: Optional[Dict[str, str]] = None) -> Dict[str, Any]:
    """Get current shell state snapshot.

    Args:
        env_vars: Environment variables to use (defaults to os.environ)

    Returns:
        Dictionary with 'pwd' and 'env' keys
    """
    if env_vars is None:
        env_vars = dict(os.environ)

    filtered_env = {
        k: v
        for k, v in env_vars.items()
        if not k.startswith("_") and k not in IGNORED_ENV_VARS
    }
    return {
        "pwd": os.getcwd(),
        "env": filtered_env,
    }


def parse_state_file(state_file: str) -> Dict[str, Any]:
    """Parse the state file created by the trap.

    Args:
        state_file: Path to the state file

    Returns:
        Dictionary with 'pwd' and 'env' keys
    """
    try:
        with open(state_file, "r") as f:
            content = f.read()
    except Exception:
        return {"pwd": None, "env": {}}

    state = {"pwd": None, "env": {}}

    for line in content.split("\n"):
        if not line:
            continue

        if line.startswith("PWD_AISH_MARKER:"):
            # Remove "PWD_AISH_MARKER:" prefix (16 characters)
            state["pwd"] = line[16:]
        elif "=" in line:
            key, value = line.split("=", 1)
            # Filter ignored variables
            if not key.startswith("_") and key not in IGNORED_ENV_VARS:
                state["env"][key] = value

    return state


def detect_changes(
    old_state: Dict[str, Any], new_state: Dict[str, Any]
) -> Dict[str, Any]:
    """Detect changes between old and new shell states.

    Args:
        old_state: State before command execution
        new_state: State after command execution

    Returns:
        Dictionary describing the changes:
        - cwd_changed: bool
        - old_cwd: str or None
        - new_cwd: str or None
        - env_added: Dict of new variables
        - env_modified: Dict of {old, new} for modified variables
        - env_removed: Dict of removed variables
    """
    changes = {
        "cwd_changed": False,
        "old_cwd": None,
        "new_cwd": None,
        "env_added": {},
        "env_modified": {},
        "env_removed": {},
    }

    # Safety check: if new_state["pwd"] is empty, the state file is invalid
    # (possibly command timeout, EXIT trap didn't execute)
    if not new_state.get("pwd"):
        return changes

    # Detect directory changes
    if new_state.get("pwd"):
        changes["old_cwd"] = old_state.get("pwd")
        changes["new_cwd"] = new_state["pwd"]
        changes["cwd_changed"] = old_state.get("pwd") != new_state["pwd"]

    # Detect environment variable changes
    old_env = old_state.get("env", {})
    new_env = new_state.get("env", {})

    for key, new_value in new_env.items():
        old_value = old_env.get(key)
        if old_value is None:
            changes["env_added"][key] = new_value
        elif old_value != new_value:
            changes["env_modified"][key] = {"old": old_value, "new": new_value}

    for key in old_env:
        if key not in new_env:
            changes["env_removed"][key] = old_env[key]

    return changes


def apply_changes(changes: Dict[str, Any], env_manager: Optional[Any] = None) -> None:
    """Apply detected state changes to the current process.

    Args:
        changes: Changes dictionary from detect_changes()
        env_manager: Optional EnvironmentManager for tracking env changes
    """
    # Apply directory change
    if changes["cwd_changed"] and changes["new_cwd"]:
        try:
            os.chdir(changes["new_cwd"])
        except Exception:
            pass

    # Apply environment variable changes
    if env_manager:
        for key, value in changes["env_added"].items():
            env_manager.set_var(key, value, export=True)

        for key, change in changes["env_modified"].items():
            env_manager.set_var(key, change["new"], export=True)

        for key in changes["env_removed"]:
            env_manager.unset_var(key)


class StateCaptureContext:
    """Context manager for state capture in command execution.

    Usage:
        with StateCaptureContext(env_manager) as ctx:
            wrapped_cmd = ctx.wrap_command(command)
            # execute wrapped_cmd...
            ctx.apply()
    """

    def __init__(
        self,
        env_manager: Optional[Any] = None,
        env_vars: Optional[Dict[str, str]] = None,
    ):
        self.env_manager = env_manager
        self.env_vars = env_vars
        self.state_file: Optional[str] = None
        self.old_state: Optional[Dict[str, Any]] = None
        self.changes: Optional[Dict[str, Any]] = None

    def __enter__(self) -> "StateCaptureContext":
        self.state_file = create_state_file()
        self.old_state = get_current_state(self.env_vars)
        return self

    def __exit__(self, exc_type, exc_val, exc_tb) -> None:
        if self.state_file:
            cleanup_state_file(self.state_file)

    def wrap_command(self, command: str) -> str:
        """Wrap command with state capture logic."""
        if not self.state_file:
            raise RuntimeError("StateCaptureContext not entered")
        return wrap_command_with_state_capture(command, self.state_file)

    def capture_and_apply(self) -> Dict[str, Any]:
        """Parse state file, detect changes, and apply them."""
        if not self.state_file or not self.old_state:
            raise RuntimeError("StateCaptureContext not entered")

        new_state = parse_state_file(self.state_file)
        self.changes = detect_changes(self.old_state, new_state)
        apply_changes(self.changes, self.env_manager)
        return self.changes
