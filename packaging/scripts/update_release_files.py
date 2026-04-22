#!/usr/bin/env python3
"""Update repository version files for a stable release (Rust-based AI Shell)."""
from __future__ import annotations

import argparse
import datetime as dt
import re
import subprocess
import sys
from pathlib import Path


ROOT_DIR = Path(__file__).resolve().parents[2]
CARGO_TOML_PATH = ROOT_DIR / "Cargo.toml"
CHANGELOG_PATH = ROOT_DIR / "CHANGELOG.md"
VERSION_RE = re.compile(r"^\d+\.\d+\.\d+$")
CARGO_VERSION_RE = re.compile(r'^(version\s*=\s*")([^"]+)("\s*$)', re.MULTILINE)
CHANGELOG_SECTION_RE = re.compile(r"^## \[", re.MULTILINE)


def _update_cargo_toml(version: str) -> None:
    """Update version in [workspace.package] section of Cargo.toml."""
    original = CARGO_TOML_PATH.read_text(encoding="utf-8")

    # Find the [workspace.package] section and update version within it
    in_workspace_package = False
    lines = original.split("\n")
    updated = False
    new_lines = []

    for line in lines:
        stripped = line.strip()
        if stripped.startswith("["):
            in_workspace_package = stripped == "[workspace.package]"
        if in_workspace_package and not updated:
            match = re.match(r'^(version\s*=\s*")([^"]+)(".*)$', line)
            if match:
                line = f"{match.group(1)}{version}{match.group(3)}"
                updated = True
        new_lines.append(line)

    if not updated:
        raise ValueError("Could not find version in [workspace.package] section of Cargo.toml")

    CARGO_TOML_PATH.write_text("\n".join(new_lines), encoding="utf-8")


def _update_cargo_lock(version: str) -> None:
    """Update aish packages in Cargo.lock to match the new version."""
    cargo_lock = ROOT_DIR / "Cargo.lock"
    if not cargo_lock.exists():
        return

    try:
        subprocess.check_call(["cargo", "generate-lockfile"], cwd=ROOT_DIR)
    except (OSError, subprocess.CalledProcessError) as exc:
        print(f"Warning: cargo generate-lockfile failed: {exc}", file=sys.stderr)


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
    _update_cargo_toml(version)
    _update_cargo_lock(version)
    _update_changelog(version, release_date)


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Update repository version files for a stable release."
    )
    parser.add_argument("--version", required=True, help="Stable release version, for example 0.2.0")
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
