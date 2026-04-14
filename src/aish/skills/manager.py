from __future__ import annotations

import logging
import os
import re
import threading
from pathlib import Path
from typing import Optional

import yaml

from .models import Skill, SkillList, SkillMetadataInfo, SkillSource
from .validator import SkillValidationError, validate_frontmatter

# Regex to extract YAML frontmatter from markdown files
_FRONTMATTER_RE = re.compile(r"^---\s*\n(.*?)\n---\s*\n", re.DOTALL)

logger = logging.getLogger("aish.skills.manager")


def _iter_skill_files(skill_root: Path) -> list[Path]:
    """Iterate SKILL.md files under a root, following directory symlinks safely.

    pathlib.Path.rglob does not traverse into directory symlinks on Python 3.10.
    We use os.walk(followlinks=True) with cycle detection to support symlinked skills.
    """

    if not skill_root.is_dir():
        return []

    skill_files: list[Path] = []
    visited: set[tuple[int, int]] = set()

    for dirpath, dirnames, filenames in os.walk(str(skill_root), followlinks=True):
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
            # Best-effort; if we can't stat, skip cycle detection for this node.
            pass

        for filename in filenames:
            if filename.upper() == "SKILL.MD":
                skill_files.append(Path(dirpath) / filename)

    return skill_files


class SkillManager:
    """Manager for skill discovery and loading with priority-based deduplication

    Skills are loaded from multiple sources with priority: USER > CLAUDE.
    When duplicate skill names are found, the higher priority skill is kept.
    """

    def __init__(self) -> None:
        """Initialize the skill manager"""
        self._loaded_skills: dict[str, Skill] = {}  # skill_name -> Skill
        self._skill_lists: list[SkillList] = []
        self._lock = threading.Lock()
        self._invalidate_seq = 0
        self._loaded_seq = 0
        self._skills_version = 0

    @property
    def skills_version(self) -> int:
        with self._lock:
            return self._skills_version

    @property
    def is_dirty(self) -> bool:
        with self._lock:
            return self._loaded_seq != self._invalidate_seq

    def invalidate(self, changed_path: str | Path | None = None) -> None:
        """Mark the current in-memory skills snapshot as stale.

        This does not reload immediately; callers should invoke `reload_if_dirty()`
        at a safe point (e.g., before building a new model request).
        """
        # changed_path is currently for future diagnostics/logging.
        _ = changed_path
        with self._lock:
            self._invalidate_seq += 1

    def _build_all_skills(self) -> tuple[dict[str, Skill], list[SkillList]]:
        loaded_skills: dict[str, Skill] = {}
        skill_lists: list[SkillList] = []

        skill_roots = self.scan_skill_roots()
        for source, root_path in skill_roots:
            skill_list = self.load_skills(source, root_path)
            skill_lists.append(skill_list)

            for skill in skill_list.skills:
                skill_name = skill.metadata.name
                if skill_name not in loaded_skills:
                    # First occurrence wins (higher priority)
                    loaded_skills[skill_name] = skill
                # else: Skip duplicate skill from lower priority source

        return loaded_skills, skill_lists

    def reload_if_dirty(self) -> bool:
        """Reload skills if the in-memory snapshot was invalidated.

        Returns:
            True if a reload happened, False otherwise.
        """
        with self._lock:
            target_seq = self._invalidate_seq
            if self._loaded_seq == target_seq:
                return False

        loaded_skills, skill_lists = self._build_all_skills()

        with self._lock:
            self._loaded_skills = loaded_skills
            self._skill_lists = skill_lists
            self._loaded_seq = target_seq
            self._skills_version += 1

        return True

    def scan_skill_roots(self) -> list[tuple[SkillSource, Path]]:
        """Scan and return skill root directories in priority order

        Returns:
            List of (source, path) tuples in priority order: USER > CLAUDE
        """
        roots: list[tuple[SkillSource, Path]] = []

        for source, root in self.skill_root_candidates():
            if root.is_dir():
                roots.append((source, root))

        return roots

    def skill_root_candidates(self) -> list[tuple[SkillSource, Path]]:
        """Return expected skill root directories regardless of existence."""
        roots: list[tuple[SkillSource, Path]] = []

        # 1. USER: $AISH_CONFIG_DIR/skills or ~/.config/aish/skills
        # Skills from debian/skills are auto-installed to ~/.config/aish/skills/
        config_dir = os.environ.get("AISH_CONFIG_DIR")
        if config_dir:
            user_skills_dir = Path(config_dir) / "skills"
        else:
            user_skills_dir = Path.home() / ".config" / "aish" / "skills"
        roots.append((SkillSource.USER, user_skills_dir))

        # 2. CLAUDE: $HOME/.claude/skills
        claude_skills_dir = Path.home() / ".claude" / "skills"
        roots.append((SkillSource.CLAUDE, claude_skills_dir))

        return roots

    def load_all_skills(self) -> dict[str, Skill]:
        """Load all skills from all sources with priority-based deduplication

        Returns:
            Dictionary mapping skill names to Skill objects
        """
        with self._lock:
            target_seq = self._invalidate_seq
        loaded_skills, skill_lists = self._build_all_skills()

        with self._lock:
            self._loaded_skills = loaded_skills
            self._skill_lists = skill_lists
            self._loaded_seq = target_seq
            self._skills_version += 1
            return self._loaded_skills

    def load_skills(self, source: SkillSource, skill_root: Path) -> SkillList:
        """Load all skills from a specific directory

        Args:
            source: Source type of the skills
            skill_root: Root directory containing skill files

        Returns:
            SkillList containing all valid skills from this directory
        """
        skills: list[Skill] = []

        if not skill_root.is_dir():
            return SkillList(
                source=source, skills=[], root_path=str(skill_root.absolute())
            )

        # Find all SKILL.md files (case-insensitive), following directory symlinks.
        for skill_file in sorted(_iter_skill_files(skill_root)):
            try:
                skill = self.parse_skill_file(source, skill_file)
                skills.append(skill)
            except Exception as e:
                # Log error but continue loading other skills
                logger.warning("Failed to load skill from %s: %s", skill_file, e)

        return SkillList(
            source=source, skills=skills, root_path=str(skill_root.absolute())
        )

    def parse_skill_file(self, source: SkillSource, skill_path: Path) -> Skill:
        """Parse a single SKILL.md file and extract metadata and content

        Args:
            source: Source type of the skill
            skill_path: Path to the SKILL.md file

        Returns:
            Skill object with parsed metadata and content

        Raises:
            ValueError: If file format is invalid or metadata is missing
            FileNotFoundError: If skill file doesn't exist
        """
        if not skill_path.is_file():
            raise FileNotFoundError(f"Skill file not found: {skill_path}")

        content = skill_path.read_text(encoding="utf-8")

        # Extract frontmatter
        match = _FRONTMATTER_RE.match(content)
        if not match:
            raise ValueError(
                f"Invalid skill file format: {skill_path}. "
                "Must start with YAML frontmatter (---)"
            )

        frontmatter_yaml = match.group(1)
        skill_content = content[match.end() :].strip()

        # Parse YAML frontmatter
        try:
            frontmatter_data = yaml.safe_load(frontmatter_yaml)
        except yaml.YAMLError as e:
            raise ValueError(f"Invalid YAML frontmatter in {skill_path}: {e}")

        validation = validate_frontmatter(frontmatter_data)
        for warning in validation.warnings:
            logger.warning(
                "Skill validation warning in %s: %s",
                skill_path,
                warning,
            )
        if validation.errors:
            raise SkillValidationError(validation.errors)
        if validation.metadata is None:
            raise SkillValidationError(["frontmatter validation failed"])
        metadata = validation.metadata

        return Skill(
            metadata=metadata,
            content=skill_content,
            source=source,
            file_path=str(skill_path.absolute()),
            base_dir=str(skill_path.parent.absolute()),
        )

    def get_skill(self, name: str) -> Optional[Skill]:
        """Get a loaded skill by name

        Args:
            name: Skill name

        Returns:
            Skill object if found, None otherwise
        """
        with self._lock:
            return self._loaded_skills.get(name)

    def list_skills(self, source: Optional[SkillSource] = None) -> list[Skill]:
        """List all loaded skills, optionally filtered by source

        Args:
            source: Optional source filter

        Returns:
            List of Skill objects
        """
        with self._lock:
            skills = list(self._loaded_skills.values())
        if source is None:
            return skills
        return [skill for skill in skills if skill.source == source]

    @property
    def skill_lists(self) -> list[SkillList]:
        """Get all loaded skill lists (one per source directory)"""
        with self._lock:
            return list(self._skill_lists)

    def to_skill_infos(self) -> list[SkillMetadataInfo]:
        """Convert loaded skills to a list of SkillMetadataInfo objects for proto serialization

        Returns:
            List of SkillMetadataInfo objects suitable for constructing SkillMetadataInfo proto messages
        """
        with self._lock:
            skills = list(self._loaded_skills.values())
        result: list[SkillMetadataInfo] = []
        for skill in skills:
            result.append(
                SkillMetadataInfo(
                    name=skill.metadata.name,
                    description=skill.metadata.description,
                    source=skill.source.value,
                    file_path=skill.file_path,
                    license=skill.metadata.license or "",
                    compatibility=skill.metadata.compatibility or "",
                    allowed_tools=skill.metadata.allowed_tools or [],
                )
            )
        return result
