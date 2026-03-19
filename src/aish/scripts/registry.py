"""Script registry for managing loaded scripts with hot reload support."""

from __future__ import annotations

import logging
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
            return dict(self._scripts)

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
