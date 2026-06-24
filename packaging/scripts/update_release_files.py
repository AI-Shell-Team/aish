#!/usr/bin/env python3
"""Update repository version files for a release (Rust-based AI Shell)."""
from __future__ import annotations

import argparse
import datetime as dt
import re
import sys
from pathlib import Path


ROOT_DIR = Path(__file__).resolve().parents[2]
CARGO_TOML_PATH = ROOT_DIR / "Cargo.toml"
CHANGELOG_PATH = ROOT_DIR / "CHANGELOG.md"
SEMVER_RE = re.compile(
    r"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)"
    r"(?:-(?:0|[1-9]\d*|[0-9A-Za-z-]*[A-Za-z-][0-9A-Za-z-]*)(?:\.(?:0|[1-9]\d*|[0-9A-Za-z-]*[A-Za-z-][0-9A-Za-z-]*))*)?$"
)
CARGO_VERSION_RE = re.compile(r'^(version\s*=\s*")([^"]+)("\s*$)', re.MULTILINE)
CHANGELOG_SECTION_RE = re.compile(r"^## \[", re.MULTILINE)
CHANGELOG_VERSION_HEADING_RE = re.compile(r"^## \[([^\]]+)\](?: - .*)?$", re.MULTILINE)
UNRELEASED_HEADING_RE = re.compile(r"^## \[Unreleased\](?: - .*)?$", re.MULTILINE)
UNRELEASED_PLACEHOLDER = "### Added\n\n- No unreleased changes yet.\n"


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

    original = cargo_lock.read_text(encoding="utf-8")
    package_re = re.compile(
        r'(\[\[package\]\]\nname = "aish-[^"]+"\nversion = ")([^"]+)(")',
        re.MULTILINE,
    )
    updated, replacements = package_re.subn(rf'\g<1>{version}\g<3>', original)
    if replacements == 0:
        print("Warning: Cargo.lock does not contain any aish-* packages to update", file=sys.stderr)
        return

    cargo_lock.write_text(updated, encoding="utf-8")


def _find_section_bounds(content: str, heading_re: re.Pattern[str]) -> tuple[int, int, int] | None:
    match = heading_re.search(content)
    if match is None:
        return None

    next_match = CHANGELOG_SECTION_RE.search(content, match.end())
    end = next_match.start() if next_match else len(content)
    return match.start(), match.end(), end


def _normalize_section_body(body: str) -> str:
    return body.strip("\n")


def _is_placeholder_body(body: str) -> bool:
    return _normalize_section_body(body) == UNRELEASED_PLACEHOLDER.strip()


def _extract_release_body(original: str, version: str) -> str | None:
    unreleased_bounds = _find_section_bounds(original, UNRELEASED_HEADING_RE)
    if unreleased_bounds is not None:
        _, unreleased_heading_end, unreleased_end = unreleased_bounds
        unreleased_body = original[unreleased_heading_end:unreleased_end]
        normalized = _normalize_section_body(unreleased_body)
        if normalized and not _is_placeholder_body(unreleased_body):
            return normalized

    if "-" in version:
        return None

    stable_base = version
    for match in CHANGELOG_VERSION_HEADING_RE.finditer(original):
        candidate = match.group(1)
        if candidate.startswith(f"{stable_base}-"):
            bounds = _find_section_bounds(
                original,
                re.compile(rf"^## \[{re.escape(candidate)}\](?: - .*)?$", re.MULTILINE),
            )
            if bounds is None:
                continue
            _, heading_end, end = bounds
            normalized = _normalize_section_body(original[heading_end:end])
            if normalized:
                return normalized

    return None


def _update_changelog(version: str, release_date: str) -> None:
    original = CHANGELOG_PATH.read_text(encoding="utf-8")
    if f"## [{version}] - {release_date}" in original or f"## [{version}]" in original:
        raise ValueError(f"Changelog already contains a section for version {version}")

    release_body = _extract_release_body(original, version)
    if release_body is None:
        raise ValueError(
            "Could not determine release notes for the new changelog section. "
            "Add notes under [Unreleased] or ensure a matching prerelease section exists."
        )

    new_section = f"## [{version}] - {release_date}\n\n{release_body}\n\n"
    unreleased_bounds = _find_section_bounds(original, UNRELEASED_HEADING_RE)
    if unreleased_bounds is not None:
        unreleased_start, _, unreleased_end = unreleased_bounds
        updated = (
            f"{original[:unreleased_start]}"
            f"{new_section}"
            f"{original[unreleased_end:]}"
        )
    else:
        match = CHANGELOG_SECTION_RE.search(original)
        if match is None:
            separator = "" if original.endswith("\n\n") else "\n\n"
            updated = f"{original.rstrip()}{separator}{new_section}"
        else:
            updated = f"{original[:match.start()]}{new_section}{original[match.start():]}"

    CHANGELOG_PATH.write_text(updated, encoding="utf-8")


def update_release_files(version: str, release_date: str) -> None:
    if not SEMVER_RE.fullmatch(version):
        raise ValueError(
            f"Invalid version '{version}'. Expected format: X.Y.Z or X.Y.Z-prerelease"
        )
    _update_cargo_toml(version)
    _update_cargo_lock(version)
    _update_changelog(version, release_date)


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Update repository version files for a release."
    )
    parser.add_argument(
        "--version",
        required=True,
        help="Release version, for example 0.2.0 or 1.0.0-beta.1",
    )
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
