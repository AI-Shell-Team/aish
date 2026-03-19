"""Hot reload service for scripts.

Similar to SkillHotReloadService, watches the scripts directory for changes
and triggers lazy reload.
"""

from __future__ import annotations

import asyncio
import logging
import threading
from pathlib import Path
from typing import TYPE_CHECKING, Optional

if TYPE_CHECKING:
    from .registry import ScriptRegistry

logger = logging.getLogger("aish.scripts.hotreload")


class ScriptHotReloadService:
    """Service to watch for script file changes and trigger reload."""

    def __init__(
        self,
        script_registry: "ScriptRegistry",
        debounce_ms: int = 500,
    ):
        """Initialize the hot reload service.

        Args:
            script_registry: ScriptRegistry to invalidate on changes.
            debounce_ms: Debounce time in milliseconds.
        """
        self.script_registry = script_registry
        self.debounce_ms = debounce_ms
        self._stop_event = threading.Event()
        self._thread: Optional[threading.Thread] = None
        self._running = False

    def start(self) -> None:
        """Start watching for script changes."""
        if self._running:
            return

        self._stop_event.clear()
        self._thread = threading.Thread(target=self._watch_loop, daemon=True)
        self._thread.start()
        self._running = True
        logger.debug("Script hot reload service started")

    def stop(self) -> None:
        """Stop watching for script changes."""
        if not self._running:
            return

        self._stop_event.set()
        if self._thread:
            self._thread.join(timeout=2.0)
            self._thread = None
        self._running = False
        logger.debug("Script hot reload service stopped")

    def _watch_loop(self) -> None:
        """Main watch loop running in background thread."""
        scripts_dir = self.script_registry.get_scripts_dir()

        # If directory doesn't exist, wait for it to be created
        # instead of immediately returning
        if not scripts_dir.exists():
            logger.debug(
                "Scripts directory does not exist: %s, waiting for creation",
                scripts_dir,
            )
            # Poll for directory creation with 1 second intervals
            while not self._stop_event.is_set() and not scripts_dir.exists():
                self._stop_event.wait(1.0)

            if self._stop_event.is_set():
                return

            logger.debug("Scripts directory now exists: %s", scripts_dir)

        # Check if watchfiles is available
        try:
            import importlib.util

            if importlib.util.find_spec("watchfiles") is None:
                logger.debug("watchfiles not installed, hot reload disabled")
                return
        except Exception:
            logger.debug("watchfiles not available, hot reload disabled")
            return

        try:
            # Run the async watcher in a new event loop
            loop = asyncio.new_event_loop()
            asyncio.set_event_loop(loop)
            try:
                loop.run_until_complete(self._async_watch(scripts_dir))
            finally:
                loop.close()
        except Exception as e:
            logger.warning("Script hot reload error: %s", e)

    async def _async_watch(self, scripts_dir: Path) -> None:
        """Async watch for file changes.

        Args:
            scripts_dir: Directory to watch.
        """
        try:
            import watchfiles
        except ImportError:
            return

        debounce_s = self.debounce_ms / 1000.0

        async for changes in watchfiles.awatch(
            str(scripts_dir),
            stop_event=self._stop_event,
            debounce=debounce_s * 1000,  # watchfiles uses ms
        ):
            if self._stop_event.is_set():
                break

            # Check if any .aish files changed
            aish_changed = False
            for change_type, path in changes:
                if path.endswith(".aish"):
                    aish_changed = True
                    logger.debug(
                        "Script file %s: %s",
                        "modified" if change_type == 1 else "deleted/added",
                        path,
                    )

            if aish_changed:
                self.script_registry.invalidate()
                logger.debug("Scripts invalidated for reload")
