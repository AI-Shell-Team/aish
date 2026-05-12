from __future__ import annotations

from pathlib import Path

from pydantic import Field

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


def _is_relative_to(path: Path, root: Path) -> bool:
    try:
        path.relative_to(root)
        return True
    except ValueError:
        return False


def _normalize_root(root: str | None, workspace_root: Path) -> Path:
    if isinstance(root, str) and root.strip():
        resolved = Path(root).expanduser().resolve()
    else:
        resolved = Path.cwd().resolve()
    if not _is_relative_to(resolved, workspace_root):
        raise ValueError(f"root must be within the current workspace: {workspace_root}")
    return resolved


class GlobTool(ToolBase):
    workspace_root: Path = Field(
        default_factory=lambda: Path.cwd().resolve(),
        exclude=True,
    )

    def __init__(self, workspace_root: str | Path | None = None) -> None:
        resolved_workspace_root = (
            Path(workspace_root).expanduser().resolve()
            if workspace_root is not None
            else Path.cwd().resolve()
        )
        super().__init__(
            name="glob",
            description=(
                "Enumerate files by glob pattern within a directory. "
                "Automatically excludes VCS directories (.git, .svn, …) and "
                "common generated trees (node_modules, __pycache__, .venv)."
            ),
            parameters={
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": ("Glob pattern such as **/*.py or src/**/*.md"),
                    },
                    "root": {
                        "type": "string",
                        "description": (
                            "Optional search root directory. "
                            "Defaults to the current working directory."
                        ),
                    },
                },
                "required": ["pattern"],
            },
        )
        self.workspace_root = resolved_workspace_root

    def __call__(self, pattern: str, root: str | None = None) -> ToolResult:
        if not isinstance(pattern, str) or not pattern.strip():
            return ToolResult(ok=False, output="Error: pattern is required")

        try:
            base = _normalize_root(root, self.workspace_root)
        except ValueError as exc:
            return ToolResult(ok=False, output=f"Error: {exc}")
        if not base.exists() or not base.is_dir():
            return ToolResult(
                ok=False,
                output=f"Error: root directory not found: {base}",
            )

        matches: list[Path] = []
        try:
            for candidate in base.glob(pattern):
                resolved = candidate.resolve()
                if not _is_relative_to(resolved, base):
                    continue
                # Skip excluded directories and files inside them.
                try:
                    rel = resolved.relative_to(base)
                except ValueError:
                    continue
                if _DEFAULT_EXCLUDE_DIRS.intersection(rel.parts):
                    continue
                matches.append(resolved)
        except Exception as exc:
            return ToolResult(ok=False, output=f"Error: {exc}")

        matches = sorted(dict.fromkeys(matches), key=lambda item: str(item))
        if not matches:
            return ToolResult(ok=True, output="No files found.")

        lines = [str(path) for path in matches[:_DEFAULT_MAX_RESULTS]]
        truncated = len(matches) > _DEFAULT_MAX_RESULTS
        if truncated:
            lines.append(
                f"... ({len(matches) - _DEFAULT_MAX_RESULTS} more, "
                f"results truncated at {_DEFAULT_MAX_RESULTS})"
            )
        return ToolResult(ok=True, output="\n".join(lines))
