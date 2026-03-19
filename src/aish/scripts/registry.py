"""Script registry for managing loaded scripts with hot reload support."""

from __future__ import annotations

import logging
import shutil
import threading
from pathlib import Path
from typing import Optional

from .loader import ScriptLoader
from .models import Script

logger = logging.getLogger("aish.scripts.registry")


class ScriptRegistry:
    """Registry for loaded scripts with hot reload support."""

    def __init__(self, scripts_dir: Optional[Path] = None):
        """Initialize the script registry.

        Args:
            scripts_dir: Custom scripts directory. If None, uses default location.
        """
        self._scripts: dict[str, Script] = {}
        self._loader = ScriptLoader(scripts_dir)
        self._lock = threading.Lock()
        self._invalidate_seq = 0
        self._loaded_seq = 0
        self._scripts_version = 0
        # Cache for system command checks to avoid repeated shutil.which calls
        self._system_cmd_cache: dict[str, bool] = {}
        # Track already-warned scripts to avoid duplicate warnings
        self._warned_scripts: set[str] = set()

    @property
    def scripts_version(self) -> int:
        """Get current scripts version (incremented on each reload)."""
        with self._lock:
            return self._scripts_version

    @property
    def is_dirty(self) -> bool:
        """Check if scripts need to be reloaded."""
        with self._lock:
            return self._loaded_seq != self._invalidate_seq

    def invalidate(self, changed_path: str | Path | None = None) -> None:
        """Mark scripts as dirty for lazy reload.

        Args:
            changed_path: Path that changed (for logging/debugging).
        """
        _ = changed_path  # For future diagnostics
        with self._lock:
            self._invalidate_seq += 1

    def reload_if_dirty(self) -> bool:
        """Reload scripts if invalidated.

        Returns:
            True if a reload happened, False otherwise.
        """
        with self._lock:
            target_seq = self._invalidate_seq
            if self._loaded_seq == target_seq:
                return False

        # Rebuild scripts dict outside lock
        scripts = self._loader.scan_scripts()

        with self._lock:
            self._scripts = scripts
            self._loaded_seq = target_seq
            self._scripts_version += 1
            self._warned_scripts.clear()

        # Check for conflicts with system commands
        self._check_script_conflicts(scripts)

        logger.debug(
            "Reloaded %d scripts (version %d)", len(scripts), self._scripts_version
        )
        return True

    def load_all_scripts(self) -> dict[str, Script]:
        """Load all scripts from scripts directory.

        Returns:
            Dictionary mapping script names to Script objects.
        """
        with self._lock:
            target_seq = self._invalidate_seq

        scripts = self._loader.scan_scripts()

        with self._lock:
            self._scripts = scripts
            self._loaded_seq = target_seq
            self._scripts_version += 1
            self._warned_scripts.clear()

        # Check for conflicts with system commands
        self._check_script_conflicts(scripts)

        return dict(self._scripts)

    def _check_script_conflicts(self, scripts: dict[str, Script]) -> None:
        """Check if any scripts shadow system commands and log warnings.

        Args:
            scripts: Dictionary of loaded scripts.
        """
        for name in scripts:
            if name in self._warned_scripts:
                continue
            if self._is_system_command(name):
                logger.warning(
                    "Script '%s' shadows a system command. "
                    "Consider renaming to avoid confusion (e.g., 'my_%s').",
                    name,
                    name,
                )
                self._warned_scripts.add(name)

    def _is_system_command(self, name: str) -> bool:
        """Check if a command exists in system PATH.

        Args:
            name: Command name to check.

        Returns:
            True if command exists in PATH.
        """
        if name not in self._system_cmd_cache:
            self._system_cmd_cache[name] = shutil.which(name) is not None
        return self._system_cmd_cache[name]

    def has_script(self, name: str) -> bool:
        """Check if a script exists by name.

        Args:
            name: Script name.

        Returns:
            True if script exists.
        """
        with self._lock:
            return name in self._scripts

    def get_script(self, name: str) -> Optional[Script]:
        """Get a script by name.

        Args:
            name: Script name.

        Returns:
            Script object if found, None otherwise.
        """
        with self._lock:
            return self._scripts.get(name)

    def list_scripts(self) -> list[Script]:
        """List all loaded scripts.

        Returns:
            List of Script objects.
        """
        with self._lock:
            return list(self._scripts.values())

    def get_scripts_dir(self) -> Path:
        """Get the scripts directory path."""
        return self._loader.get_scripts_dir()

    def get_script_names(self) -> list[str]:
        """Get all script names.

        Returns:
            List of script names.
        """
        with self._lock:
            return list(self._scripts.keys())

    def get_hook_scripts(self, event: str) -> list[Script]:
        """Get all hook scripts for a specific event.

        Args:
            event: Hook event name (e.g., "prompt", "precmd").

        Returns:
            List of hook scripts for the event.
        """
        with self._lock:
            return [
                script
                for script in self._scripts.values()
                if script.is_hook and script.hook_event == event
            ]
