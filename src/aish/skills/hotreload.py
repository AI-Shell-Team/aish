from __future__ import annotations

import asyncio
import logging
import os
from pathlib import Path
from typing import Callable

import anyio
from watchfiles import awatch

from .manager import SkillManager

logger = logging.getLogger("aish.skills.hotreload")


def _list_skill_dirs(roots: list[Path]) -> set[Path]:
    skill_dirs: set[Path] = set()
    visited: set[tuple[int, int]] = set()
    for root in roots:
        try:
            if not root.is_dir():
                continue
        except Exception:
            continue

        # os.walk(followlinks=True) is required to traverse directory symlinks.
        for dirpath, dirnames, filenames in os.walk(str(root), followlinks=True):
            # Avoid scanning git metadata.
            if ".git" in dirnames:
                dirnames[:] = [d for d in dirnames if d != ".git"]

            try:
                st = os.stat(dirpath)
                inode_key = (int(st.st_dev), int(st.st_ino))
                if inode_key in visited:
                    dirnames[:] = []
                    continue
                visited.add(inode_key)
            except OSError:
                pass

            for filename in filenames:
                if filename.upper() != "SKILL.MD":
                    continue
                skill_dir = Path(dirpath)
                skill_dirs.add(skill_dir)
                try:
                    skill_dirs.add(skill_dir.resolve())
                except Exception:
                    pass
    return skill_dirs


def _nearest_existing_parent(path: Path) -> Path | None:
    current = path
    while True:
        parent = current.parent
        if parent == current:
            return None
        if parent.exists():
            return parent
        current = parent


def _pending_watch_dirs(pending_roots: list[Path]) -> set[Path]:
    watch_dirs: set[Path] = set()
    for root in pending_roots:
        parent = _nearest_existing_parent(root)
        if parent is not None:
            watch_dirs.add(parent)
    return watch_dirs


class _PendingRootFilter:
    def __init__(self, pending_roots: list[Path]) -> None:
        names: set[str] = set()
        for root in pending_roots:
            names.add(root.name)
            names.add(root.parent.name)
        self._names = names

    def __call__(self, _change, path: str) -> bool:
        return Path(path).name in self._names


class SkillHotReloadService:
    """Detect skill file changes and invalidate SkillManager snapshot (lazy reload).

    This service never reloads skills immediately. It only calls `SkillManager.invalidate()`.
    """

    def __init__(
        self,
        *,
        skill_manager: SkillManager,
        debounce_ms: int = 1000,
    ) -> None:
        self._skill_manager = skill_manager
        self._debounce_ms = int(debounce_ms)
        self._subscribers: set[Callable[[], None]] = set()
        self._skill_dirs: set[Path] = set()
        # Skill roots (e.g., ~/.claude/skills) for detecting root-level changes like symlink add/remove.
        self._skill_roots: set[Path] = set()
        self._stop_event = asyncio.Event()

    def stop(self) -> None:
        """Signal the watcher to stop."""
        self._stop_event.set()

    def subscribe(self, cb: Callable[[], None]) -> Callable[[], None]:
        self._subscribers.add(cb)

        def _unsubscribe() -> None:
            self._subscribers.discard(cb)

        return _unsubscribe

    def _notify(self) -> None:
        for cb in list(self._subscribers):
            try:
                cb()
            except Exception:
                # Never let subscriber errors crash the watcher.
                pass

    def _current_roots(self) -> list[Path]:
        roots: list[Path] = []
        for _, root in self._skill_manager.scan_skill_roots():
            roots.append(root)
        return roots

    def _expected_roots(self) -> list[Path]:
        roots: list[Path] = []
        for _, root in self._skill_manager.skill_root_candidates():
            roots.append(root)
        return roots

    def _is_under_skill_dir(self, path: Path) -> bool:
        for skill_dir in self._skill_dirs:
            try:
                path.relative_to(skill_dir)
                return True
            except ValueError:
                continue
        return False

    def _watch_filter(self, _change, path: str) -> bool:
        p = Path(path)
        if ".git" in p.parts:
            return False
        if p.name.upper() == "SKILL.MD":
            return True
        if self._is_under_skill_dir(p):
            return True
        # Symlinked skill directories won't be traversed by watchfiles; include symlink
        # path events so we can rebuild watch roots when symlinks are added/updated.
        try:
            if p.is_symlink():
                return True
        except Exception:
            pass
        # Catch root-level directory/symlink add/remove/rename events so we can rebuild watch roots.
        try:
            if p.parent in self._skill_roots:
                return True
        except Exception:
            pass
        return False

    def _symlink_watch_roots(self, roots: list[Path]) -> set[Path]:
        """Return resolved directory targets for directory symlinks under the skill roots.

        watchfiles does not traverse directory symlinks when watching a parent directory,
        so we add the symlink targets themselves as additional watch roots.
        """
        targets: set[Path] = set()
        for root in roots:
            try:
                if not root.is_dir():
                    continue
            except Exception:
                continue

            # Walk the real directory tree but do not follow links; collect directory symlinks
            # as separate watch roots.
            for dirpath, dirnames, _filenames in os.walk(str(root), followlinks=False):
                # Avoid scanning git metadata.
                if ".git" in dirnames:
                    dirnames[:] = [d for d in dirnames if d != ".git"]

                for dirname in list(dirnames):
                    candidate = Path(dirpath) / dirname
                    try:
                        if candidate.is_symlink() and candidate.is_dir():
                            targets.add(candidate.resolve())
                            # Do not descend into symlink dirs (even if followlinks changes).
                            dirnames.remove(dirname)
                    except Exception:
                        continue
        return targets

    async def _watch_active_roots(
        self, roots: list[Path], rebuild_event: anyio.Event
    ) -> None:
        async for changes in awatch(
            *roots,
            recursive=True,
            watch_filter=self._watch_filter,
            debounce=self._debounce_ms,
            force_polling=False,
            stop_event=self._stop_event,
        ):
            if rebuild_event.is_set():
                return

            if self._stop_event.is_set():
                return

            if any(not root.is_dir() for root in roots):
                # Roots were removed or replaced. Mark the snapshot dirty so the
                # next safe point reload can drop/add skills accordingly.
                changed_path = None
                try:
                    if changes:
                        changed_path = next(iter(changes))[1]
                except Exception:
                    changed_path = None
                self._skill_manager.invalidate(changed_path)
                self._notify()
                rebuild_event.set()
                return

            if not changes:
                continue

            # Root-level changes (e.g., a new symlinked skill directory) require
            # rebuilding the watch roots set in the outer loop.
            try:
                if any(Path(path).parent in self._skill_roots for _, path in changes):
                    changed_path = next(iter(changes))[1]
                    logger.info("Detected skill root change: %s", changed_path)
                    self._skill_manager.invalidate(changed_path)
                    self._notify()
                    rebuild_event.set()
                    return
            except Exception:
                pass

            # Adding/updating a directory symlink anywhere under the skill roots requires
            # rebuilding the watch roots set so we can watch its target path.
            try:
                for _, path in changes:
                    p = Path(path)
                    if p.exists() and p.is_symlink() and p.is_dir():
                        logger.info("Detected skill symlink change: %s", path)
                        self._skill_manager.invalidate(path)
                        self._notify()
                        rebuild_event.set()
                        return
            except Exception:
                pass

            skill_file_changed = any(
                Path(path).name.upper() == "SKILL.MD" for _, path in changes
            )

            if skill_file_changed:
                self._skill_dirs = _list_skill_dirs(roots)

            changed_path = next(iter(changes))[1]
            logger.debug("Detected skill change: %s", changed_path)
            self._skill_manager.invalidate(changed_path)
            self._notify()

    async def _watch_pending_roots(
        self,
        pending_roots: list[Path],
        watch_dirs: list[Path],
        rebuild_event: anyio.Event,
    ) -> None:
        pending_filter = _PendingRootFilter(pending_roots)
        async for changes in awatch(
            *watch_dirs,
            recursive=False,
            watch_filter=pending_filter,
            debounce=self._debounce_ms,
            force_polling=False,
            stop_event=self._stop_event,
        ):
            if rebuild_event.is_set():
                return

            if self._stop_event.is_set():
                return
            changed_path = None
            try:
                if changes:
                    changed_path = next(iter(changes))[1]
            except Exception:
                changed_path = None
            # A previously-missing root (or one of its parents) changed. Invalidate
            # so the next safe reload can discover any newly-created skills.
            self._skill_manager.invalidate(changed_path)
            self._notify()
            rebuild_event.set()
            return

    async def run(self) -> None:
        while True:
            expected_roots = self._expected_roots()
            active_roots = [root for root in expected_roots if root.is_dir()]
            pending_roots = [root for root in expected_roots if not root.is_dir()]
            pending_dirs = sorted(_pending_watch_dirs(pending_roots))

            if not active_roots and not pending_dirs:
                return

            # Watch skill roots plus resolved targets of directory symlinks (watchfiles doesn't
            # traverse directory symlinks when watching a parent directory).
            extra_roots = self._symlink_watch_roots(active_roots)
            watch_roots = sorted(set(active_roots) | extra_roots)

            self._skill_dirs = _list_skill_dirs(active_roots)
            self._skill_roots = set(active_roots)
            for root in list(active_roots):
                try:
                    self._skill_roots.add(root.resolve())
                except Exception:
                    pass
            rebuild_event = anyio.Event()

            async with anyio.create_task_group() as tg:
                if watch_roots:
                    tg.start_soon(self._watch_active_roots, watch_roots, rebuild_event)
                if pending_dirs:
                    tg.start_soon(
                        self._watch_pending_roots,
                        pending_roots,
                        pending_dirs,
                        rebuild_event,
                    )

                await rebuild_event.wait()
                tg.cancel_scope.cancel()
