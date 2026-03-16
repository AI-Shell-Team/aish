#!/usr/bin/env python3
from __future__ import annotations

import argparse
import datetime as dt
import re
from pathlib import Path


ROOT_DIR = Path(__file__).resolve().parents[2]
PYPROJECT_PATH = ROOT_DIR / "pyproject.toml"
RUNTIME_VERSION_PATH = ROOT_DIR / "src" / "aish" / "__init__.py"
CHANGELOG_PATH = ROOT_DIR / "CHANGELOG.md"
UV_LOCK_PATH = ROOT_DIR / "uv.lock"
VERSION_RE = re.compile(r"^\d+\.\d+\.\d+$")
PYPROJECT_VERSION_RE = re.compile(r'^(version\s*=\s*")([^"]+)("\s*)$', re.MULTILINE)
RUNTIME_VERSION_RE = re.compile(r'^(__version__\s*=\s*")([^"]+)("\s*)$', re.MULTILINE)
UV_LOCK_VERSION_RE = re.compile(
    r'(?ms)^(\[\[package\]\]\nname\s*=\s*"aish"\nversion\s*=\s*")([^"]+)("\s*$)'
)
CHANGELOG_SECTION_RE = re.compile(r"^## \[", re.MULTILINE)


def _replace_single(pattern: re.Pattern[str], text: str, replacement_value: str) -> str:
    def _replacement(match: re.Match[str]) -> str:
        return f"{match.group(1)}{replacement_value}{match.group(3)}"

    new_text, count = pattern.subn(_replacement, text, count=1)
    if count != 1:
        raise ValueError("Expected to replace exactly one version string")
    return new_text


def _update_pyproject(version: str) -> None:
    original = PYPROJECT_PATH.read_text(encoding="utf-8")
    updated = _replace_single(PYPROJECT_VERSION_RE, original, version)
    PYPROJECT_PATH.write_text(updated, encoding="utf-8")


def _update_runtime_version(version: str) -> None:
    original = RUNTIME_VERSION_PATH.read_text(encoding="utf-8")
    updated = _replace_single(RUNTIME_VERSION_RE, original, version)
    RUNTIME_VERSION_PATH.write_text(updated, encoding="utf-8")


def _update_uv_lock(version: str) -> None:
    if not UV_LOCK_PATH.exists():
        return

    original = UV_LOCK_PATH.read_text(encoding="utf-8")
    updated = _replace_single(UV_LOCK_VERSION_RE, original, version)
    UV_LOCK_PATH.write_text(updated, encoding="utf-8")


def _update_changelog(version: str, release_date: str) -> None:
    original = CHANGELOG_PATH.read_text(encoding="utf-8")
    if f"## [{version}] - {release_date}" in original or f"## [{version}]" in original:
        raise ValueError(f"Changelog already contains a section for version {version}")

    new_section = f"## [{version}] - {release_date}\n\n"
    match = CHANGELOG_SECTION_RE.search(original)
    if match is None:
        separator = "" if original.endswith("\n\n") else "\n\n"
        updated = f"{original.rstrip()}{separator}{new_section}"
    else:
        updated = f"{original[:match.start()]}{new_section}{original[match.start():]}"

    CHANGELOG_PATH.write_text(updated, encoding="utf-8")


def update_release_files(version: str, release_date: str) -> None:
    if not VERSION_RE.fullmatch(version):
        raise ValueError(f"Invalid version '{version}'. Expected format: X.Y.Z")
    _update_pyproject(version)
    _update_runtime_version(version)
    _update_uv_lock(version)
    _update_changelog(version, release_date)


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Update repository version files for a stable release."
    )
    parser.add_argument("--version", required=True, help="Stable release version, for example 0.1.1")
    parser.add_argument(
        "--date",
        default=dt.date.today().isoformat(),
        help="Release date to use in CHANGELOG.md, default: today",
    )
    args = parser.parse_args()

    update_release_files(args.version.strip(), args.date.strip())
    print(f"Updated release files for version {args.version} ({args.date})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())