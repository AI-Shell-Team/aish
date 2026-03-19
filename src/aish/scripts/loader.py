"""Script loader for scanning and parsing .aish files."""

from __future__ import annotations

import logging
import os
import re
from pathlib import Path
from typing import Optional

import yaml

from .models import Script, ScriptMetadata

# Regex to extract YAML frontmatter from script files
_FRONTMATTER_RE = re.compile(r"^---\s*\n(.*?)\n---\s*\n", re.DOTALL)

logger = logging.getLogger("aish.scripts.loader")


def _iter_script_files(script_root: Path) -> list[Path]:
    """Iterate .aish files under a root, following directory symlinks safely."""
    if not script_root.is_dir():
        return []

    script_files: list[Path] = []
    visited: set[tuple[int, int]] = set()

    for dirpath, dirnames, filenames in os.walk(str(script_root), followlinks=True):
        # Avoid scanning git metadata
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
            if filename.endswith(".aish"):
                script_files.append(Path(dirpath) / filename)

    return script_files


class ScriptLoader:
    """Loader for .aish script files."""

    def __init__(self, scripts_dir: Optional[Path] = None):
        """Initialize the script loader.

        Args:
            scripts_dir: Custom scripts directory. If None, uses default location.
        """
        self._scripts_dir = scripts_dir

    def get_scripts_dir(self) -> Path:
        """Get the scripts directory path."""
        if self._scripts_dir:
            return self._scripts_dir

        # Default: $AISH_CONFIG_DIR/scripts or ~/.config/aish/scripts
        config_dir = os.environ.get("AISH_CONFIG_DIR")
        if config_dir:
            return Path(config_dir) / "scripts"
        return Path.home() / ".config" / "aish" / "scripts"

    def scan_scripts(self) -> dict[str, Script]:
        """Scan all .aish files in scripts directory.

        Returns:
            Dictionary mapping script names to Script objects.
        """
        scripts: dict[str, Script] = {}
        scripts_dir = self.get_scripts_dir()

        if not scripts_dir.is_dir():
            logger.debug("Scripts directory does not exist: %s", scripts_dir)
            return scripts

        for script_file in _iter_script_files(scripts_dir):
            try:
                script = self.parse_script_file(script_file)
                if script:
                    # First script with a name wins (no priority override for now)
                    if script.name not in scripts:
                        scripts[script.name] = script
                    else:
                        logger.debug(
                            "Skipping duplicate script '%s' from %s (already loaded)",
                            script.name,
                            script_file,
                        )
            except Exception as e:
                logger.warning("Failed to load script from %s: %s", script_file, e)

        return scripts

    def parse_script_file(self, script_path: Path) -> Optional[Script]:
        """Parse a single .aish file and extract metadata and content.

        Args:
            script_path: Path to the .aish file.

        Returns:
            Script object if valid, None otherwise.

        Raises:
            ValueError: If file format is invalid.
            FileNotFoundError: If script file doesn't exist.
        """
        if not script_path.is_file():
            raise FileNotFoundError(f"Script file not found: {script_path}")

        content = script_path.read_text(encoding="utf-8")

        # Extract frontmatter
        match = _FRONTMATTER_RE.match(content)
        if not match:
            # Allow scripts without frontmatter - use filename as name
            script_name = script_path.stem
            script_content = content.strip()
            metadata = ScriptMetadata(name=script_name, description="")
        else:
            frontmatter_yaml = match.group(1)
            script_content = content[match.end() :].strip()

            # Parse YAML frontmatter
            try:
                frontmatter_data = yaml.safe_load(frontmatter_yaml)
            except yaml.YAMLError as e:
                raise ValueError(f"Invalid YAML frontmatter in {script_path}: {e}")

            if not frontmatter_data:
                raise ValueError(f"Empty frontmatter in {script_path}")

            if not isinstance(frontmatter_data, dict):
                raise ValueError(f"Frontmatter must be a YAML mapping in {script_path}")

            # Extract name from frontmatter or use filename
            script_name = frontmatter_data.get("name", script_path.stem)
            frontmatter_data["name"] = script_name

            metadata = ScriptMetadata(**frontmatter_data)

        return Script(
            metadata=metadata,
            content=script_content,
            file_path=str(script_path.absolute()),
            base_dir=str(script_path.parent.absolute()),
        )

    def create_script_template(
        self, name: str, description: str = "", content: str = ""
    ) -> str:
        """Create a script template with frontmatter.

        Args:
            name: Script name.
            description: Script description.
            content: Initial script content.

        Returns:
            Full script content with frontmatter.
        """
        return f'''---
name: {name}
description: {description}
---

{content}
'''
