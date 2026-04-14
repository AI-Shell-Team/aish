from __future__ import annotations

import re
from pathlib import Path

from aish.tools.base import ToolBase
from aish.tools.result import ToolResult

# Directories excluded by default (VCS and common large generated trees).
_DEFAULT_EXCLUDE_DIRS: frozenset[str] = frozenset(
    {
        ".git",
        ".svn",
        ".hg",
        ".bzr",
        ".jj",
        ".sl",
        "node_modules",
        "__pycache__",
        ".tox",
        ".mypy_cache",
        ".pytest_cache",
        ".ruff_cache",
        ".venv",
        "venv",
    }
)

_DEFAULT_MAX_RESULTS: int = 200
_MAX_LINE_LENGTH: int = 500


def _normalize_root(root: str | None) -> Path:
    if isinstance(root, str) and root.strip():
        return Path(root).expanduser().resolve()
    return Path.cwd().resolve()


def _is_relative_to(path: Path, root: Path) -> bool:
    try:
        path.relative_to(root)
        return True
    except ValueError:
        return False


def _walk_files(
    base: Path,
    *,
    include_glob: str | None = None,
    exclude_dirs: frozenset[str] = _DEFAULT_EXCLUDE_DIRS,
) -> list[Path]:
    """Yield files under *base*, skipping excluded directories.

    When *include_glob* is provided, only files matching that glob (relative
    to *base*) are returned.  Otherwise all regular files are returned.
    """
    if include_glob:
        # Let pathlib handle the glob matching – still need to filter
        # out excluded dirs from results.
        results: list[Path] = []
        for candidate in base.glob(include_glob):
            try:
                resolved = candidate.resolve()
            except Exception:
                continue
            if not _is_relative_to(resolved, base) or not resolved.is_file():
                continue
            # Check that none of the path components are excluded.
            try:
                rel = resolved.relative_to(base)
            except ValueError:
                continue
            if exclude_dirs.intersection(rel.parts):
                continue
            results.append(resolved)
        return results

    results = []
    stack: list[Path] = [base]
    while stack:
        current = stack.pop()
        try:
            entries = sorted(current.iterdir())
        except PermissionError:
            continue
        for entry in entries:
            if entry.is_dir():
                if entry.name not in exclude_dirs:
                    stack.append(entry)
            elif entry.is_file():
                try:
                    resolved = entry.resolve()
                except Exception:
                    continue
                if _is_relative_to(resolved, base):
                    results.append(resolved)
    return results


class GrepTool(ToolBase):
    def __init__(self) -> None:
        super().__init__(
            name="grep",
            description=(
                "Search file contents with a regular expression. "
                "Automatically excludes VCS directories (.git, .svn, …) and "
                "common generated trees (node_modules, __pycache__, .venv)."
            ),
            parameters={
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Regular expression pattern to search for.",
                    },
                    "root": {
                        "type": "string",
                        "description": (
                            "Optional search root directory. "
                            "Defaults to the current working directory."
                        ),
                    },
                    "include": {
                        "type": "string",
                        "description": (
                            "Optional glob filter relative to root, "
                            "e.g. *.py or src/**/*.yaml."
                        ),
                    },
                },
                "required": ["pattern"],
            },
        )

    def __call__(
        self,
        pattern: str,
        root: str | None = None,
        include: str | None = None,
    ) -> ToolResult:
        if not isinstance(pattern, str) or not pattern.strip():
            return ToolResult(ok=False, output="Error: pattern is required")

        base = _normalize_root(root)
        if not base.exists() or not base.is_dir():
            return ToolResult(
                ok=False,
                output=f"Error: root directory not found: {base}",
            )

        try:
            regex = re.compile(pattern)
        except re.error as exc:
            return ToolResult(ok=False, output=f"Error: invalid regex: {exc}")

        include_glob: str | None = None
        if isinstance(include, str) and include.strip():
            include_glob = include.strip()

        candidates = _walk_files(base, include_glob=include_glob)

        matches: list[str] = []
        for resolved in candidates:
            try:
                with resolved.open("r", encoding="utf-8") as handle:
                    for line_no, line in enumerate(handle, start=1):
                        if regex.search(line):
                            display_line = line.rstrip()
                            if len(display_line) > _MAX_LINE_LENGTH:
                                display_line = display_line[:_MAX_LINE_LENGTH] + "…"
                            matches.append(
                                f"{resolved}:{line_no}: {display_line}"
                            )
                            if len(matches) >= _DEFAULT_MAX_RESULTS:
                                break
                    if len(matches) >= _DEFAULT_MAX_RESULTS:
                        break
            except (OSError, UnicodeDecodeError):
                continue

        if not matches:
            return ToolResult(ok=True, output="No matches found.")

        truncated = len(matches) >= _DEFAULT_MAX_RESULTS
        output_lines = list(matches)
        if truncated:
            output_lines.append(f"(results truncated at {_DEFAULT_MAX_RESULTS})")
        return ToolResult(ok=True, output="\n".join(output_lines))
