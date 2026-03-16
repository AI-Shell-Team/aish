#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import re
import subprocess
from pathlib import Path


ROOT_DIR = Path(__file__).resolve().parents[2]
PYPROJECT_PATH = ROOT_DIR / "pyproject.toml"
RUNTIME_VERSION_PATH = ROOT_DIR / "src" / "aish" / "__init__.py"
CHANGELOG_PATH = ROOT_DIR / "CHANGELOG.md"
SEMVER_RE = re.compile(r"^\d+\.\d+\.\d+$")
RUNTIME_VERSION_RE = re.compile(r'^__version__\s*=\s*"([^"]+)"\s*$')


def _load_pyproject_version() -> str:
    in_project_section = False
    for raw_line in PYPROJECT_PATH.read_text(encoding="utf-8").splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        if line.startswith("["):
            in_project_section = line == "[project]"
            continue
        if in_project_section:
            match = re.match(r'^version\s*=\s*"([^"]+)"\s*$', line)
            if match:
                return match.group(1)
    raise ValueError(f"Could not find project.version in {PYPROJECT_PATH}")


def _load_runtime_version() -> str:
    for line in RUNTIME_VERSION_PATH.read_text(encoding="utf-8").splitlines():
        match = RUNTIME_VERSION_RE.match(line.strip())
        if match:
            return match.group(1)
    raise ValueError(f"Could not find __version__ in {RUNTIME_VERSION_PATH}")


def _extract_changelog_section(section_name: str) -> str:
    lines = CHANGELOG_PATH.read_text(encoding="utf-8").splitlines()
    target_heading = f"## [{section_name}]"
    in_section = False
    collected: list[str] = []

    for line in lines:
        if line.startswith(target_heading):
            in_section = True
            continue
        if in_section and line.startswith("## ["):
            break
        if in_section:
            collected.append(line)

    if not in_section:
        raise ValueError(f"Could not find changelog section {target_heading} in {CHANGELOG_PATH}")

    notes = "\n".join(collected).strip()
    if not notes:
        raise ValueError(f"Changelog section {target_heading} is empty in {CHANGELOG_PATH}")
    return notes


def _extract_release_notes(version: str | None) -> str:
    if version:
        return _extract_changelog_section(version)

    lines = CHANGELOG_PATH.read_text(encoding="utf-8").splitlines()
    for line in lines:
        match = re.match(r"^## \[(\d+\.\d+\.\d+)\]", line)
        if match:
            return _extract_changelog_section(match.group(1))

    raise ValueError(f"Could not find any versioned changelog sections in {CHANGELOG_PATH}")


def _normalize_version(raw_version: str) -> str:
    version = raw_version.strip()
    if version.startswith("v"):
        version = version[1:]
    return version


def _get_previous_stable_tag(excluded_tag: str | None = None) -> str:
    try:
        output = subprocess.check_output(
            ["git", "tag", "-l", "v*"],
            cwd=ROOT_DIR,
            text=True,
        )
    except (OSError, subprocess.CalledProcessError):
        return ""

    tags = [tag.strip() for tag in output.splitlines() if tag.strip()]
    stable_tags = [tag for tag in tags if re.fullmatch(r"v\d+\.\d+\.\d+", tag)]
    if excluded_tag:
        stable_tags = [tag for tag in stable_tags if tag != excluded_tag]
    if not stable_tags:
        return ""
    return sorted(stable_tags, key=lambda value: tuple(int(part) for part in value[1:].split(".")))[-1]


def _write_github_output(path: Path, metadata: dict[str, str]) -> None:
    with path.open("a", encoding="utf-8") as handle:
        for key, value in metadata.items():
            if "\n" in value:
                handle.write(f"{key}<<__AISH_EOF__\n{value}\n__AISH_EOF__\n")
            else:
                handle.write(f"{key}={value}\n")


def _write_summary(path: Path, metadata: dict[str, str]) -> None:
    summary = [
        "# Release Metadata Summary",
        "",
        f"- Version: {metadata['version']}",
        f"- Tag: {metadata['tag']}",
        f"- Pyproject version: {metadata['pyproject_version']}",
        f"- Runtime version: {metadata['runtime_version']}",
    ]

    if metadata["previous_stable_tag"]:
        summary.append(f"- Previous stable tag: {metadata['previous_stable_tag']}")

    summary.extend(["", "## Release Notes", "", metadata["release_notes"], ""])
    path.write_text("\n".join(summary), encoding="utf-8")


def _write_json(path: Path, metadata: dict[str, str]) -> None:
    path.write_text(json.dumps(metadata, indent=2) + "\n", encoding="utf-8")


def main() -> int:
    parser = argparse.ArgumentParser(description="Compute and validate release metadata.")
    parser.add_argument("--expected-version", help="Version that must match the repository version")
    parser.add_argument("--github-output", help="Path to append GitHub Actions outputs")
    parser.add_argument("--summary-file", help="Path to write a markdown release summary")
    parser.add_argument("--json-file", help="Path to write release metadata as JSON")
    parser.add_argument("--print-json", action="store_true", help="Print metadata as JSON")
    args = parser.parse_args()

    pyproject_version = _load_pyproject_version()
    runtime_version = _load_runtime_version()
    if pyproject_version != runtime_version:
        raise SystemExit(
            "Repository version mismatch: "
            f"pyproject.toml={pyproject_version}, src/aish/__init__.py={runtime_version}"
        )

    version = _normalize_version(args.expected_version or pyproject_version)
    if not SEMVER_RE.fullmatch(version):
        raise SystemExit(
            f"Invalid version '{version}'. Expected format: X.Y.Z"
        )

    if args.expected_version and version != pyproject_version:
        raise SystemExit(
            "Requested version does not match repository version: "
            f"requested={version}, repository={pyproject_version}"
        )

    metadata = {
        "version": version,
        "tag": f"v{version}",
        "pyproject_version": pyproject_version,
        "runtime_version": runtime_version,
        "previous_stable_tag": _get_previous_stable_tag(excluded_tag=f"v{version}"),
        "release_notes": _extract_release_notes(version if args.expected_version else None),
    }

    if args.github_output:
        _write_github_output(Path(args.github_output), metadata)
    if args.summary_file:
        _write_summary(Path(args.summary_file), metadata)
    if args.json_file:
        _write_json(Path(args.json_file), metadata)
    if args.print_json:
        print(json.dumps(metadata, indent=2))

    return 0


if __name__ == "__main__":
    raise SystemExit(main())