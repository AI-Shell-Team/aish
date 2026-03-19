"""Hook manager for aish script hooks.

Hook scripts are special scripts that run at specific events:
- aish_prompt: Generate custom prompt string
"""

from __future__ import annotations

import logging
import os
import subprocess
from typing import TYPE_CHECKING, Optional

from .executor import ScriptExecutor
from .registry import ScriptRegistry

if TYPE_CHECKING:
    from .models import Script

logger = logging.getLogger("aish.scripts.hooks")


class HookManager:
    """Manager for script-based hooks."""

    PROMPT = "prompt"

    def __init__(self, registry: ScriptRegistry, executor: ScriptExecutor):
        """Initialize the hook manager.

        Args:
            registry: ScriptRegistry to look up hook scripts.
            executor: ScriptExecutor to run hook scripts.
        """
        self.registry = registry
        self.executor = executor

    def has_hook(self, event: str) -> bool:
        """Check if a hook script exists for an event."""
        return self.registry.has_script(f"aish_{event}")

    def get_hook(self, event: str) -> Optional["Script"]:
        """Get hook script for an event."""
        return self.registry.get_script(f"aish_{event}")

    def run_prompt_hook(self, last_exit_code: int = 0) -> str:
        """Run the prompt hook to get custom prompt string.

        Args:
            last_exit_code: Exit code from the last command.

        Returns:
            Custom prompt string, or empty string if no hook.
        """
        hook = self.get_hook(self.PROMPT)
        if not hook:
            return ""

        env = self._build_prompt_env(last_exit_code)
        result = self.executor.execute_sync(hook, args=[], env=env)

        if result.success and result.output:
            return result.output.strip()
        if result.error:
            logger.warning("Prompt hook failed: %s", result.error)

        return ""

    def _build_prompt_env(self, last_exit_code: int = 0) -> dict[str, str]:
        """Build environment variables for prompt hook.

        Args:
            last_exit_code: Exit code from the last command.
        """
        env = dict(os.environ)
        cwd = os.getcwd()
        env["AISH_CWD"] = cwd
        env["AISH_EXIT_CODE"] = str(last_exit_code)

        # Git status detection
        try:
            result = subprocess.run(
                ["git", "rev-parse", "--is-inside-work-tree"],
                capture_output=True,
                text=True,
                cwd=cwd,
                timeout=0.5,
            )
            if result.returncode == 0 and result.stdout.strip() == "true":
                env["AISH_GIT_REPO"] = "1"

                result = subprocess.run(
                    ["git", "branch", "--show-current"],
                    capture_output=True,
                    text=True,
                    cwd=cwd,
                    timeout=0.5,
                )
                if result.returncode == 0:
                    env["AISH_GIT_BRANCH"] = result.stdout.strip() or "HEAD"

                result = subprocess.run(
                    ["git", "status", "--porcelain"],
                    capture_output=True,
                    text=True,
                    cwd=cwd,
                    timeout=1,
                )
                if result.returncode == 0:
                    lines = result.stdout.strip().split("\n") if result.stdout.strip() else []
                    staged = sum(1 for line in lines if line and line[0] in "MADRC")
                    modified = sum(1 for line in lines if line and line[1] in "MD")
                    untracked = sum(1 for line in lines if line.startswith("??"))

                    env["AISH_GIT_STAGED"] = str(staged)
                    env["AISH_GIT_MODIFIED"] = str(modified)
                    env["AISH_GIT_UNTRACKED"] = str(untracked)

                    if staged > 0:
                        env["AISH_GIT_STATUS"] = "staged"
                    elif modified > 0 or untracked > 0:
                        env["AISH_GIT_STATUS"] = "dirty"
                    else:
                        env["AISH_GIT_STATUS"] = "clean"

                result = subprocess.run(
                    ["git", "rev-list", "--left-right", "--count", "@{upstream}...HEAD"],
                    capture_output=True,
                    text=True,
                    cwd=cwd,
                    timeout=0.5,
                )
                if result.returncode == 0:
                    parts = result.stdout.strip().split()
                    if len(parts) == 2:
                        env["AISH_GIT_BEHIND"] = parts[0]
                        env["AISH_GIT_AHEAD"] = parts[1]
        except (OSError, subprocess.SubprocessError, subprocess.TimeoutExpired):
            pass

        # Virtual environment (exclude aish's own venv)
        if venv := os.environ.get("VIRTUAL_ENV"):
            # Skip if it's aish's own development venv
            if not venv.endswith("/aish/.venv") and "/aish/.venv/" not in venv:
                env["AISH_VIRTUAL_ENV"] = os.path.basename(venv)
        elif conda := os.environ.get("CONDA_DEFAULT_ENV"):
            env["AISH_VIRTUAL_ENV"] = conda

        return env


def build_prompt_from_script(
    registry: ScriptRegistry,
    executor: ScriptExecutor,
    default_prompt: str = "🚀",
    last_exit_code: int = 0,
) -> str:
    """Build shell prompt using hook script if available.

    Args:
        registry: ScriptRegistry to look up hook scripts.
        executor: ScriptExecutor to run hook scripts.
        default_prompt: Default prompt string if no hook.
        last_exit_code: Exit code from the last command.

    Returns:
        Custom prompt string or default prompt.
    """
    hook_manager = HookManager(registry, executor)
    custom_prompt = hook_manager.run_prompt_hook(last_exit_code=last_exit_code)
    if custom_prompt:
        return custom_prompt
    return f"{default_prompt} {os.path.basename(os.getcwd())} > "
