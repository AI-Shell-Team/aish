from __future__ import annotations

import importlib.util
from pathlib import Path


MODULE_PATH = Path(__file__).resolve().parents[3] / "packaging" / "scripts" / "release_metadata.py"
SPEC = importlib.util.spec_from_file_location("release_metadata", MODULE_PATH)
assert SPEC is not None
assert SPEC.loader is not None
release_metadata = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(release_metadata)


def test_extract_release_notes_uses_versioned_section(tmp_path, monkeypatch):
    changelog = tmp_path / "CHANGELOG.md"
    changelog.write_text(
        """# Changelog

## [0.1.1] - 2026-03-13

### Added

- Stable release note
""",
        encoding="utf-8",
    )
    monkeypatch.setattr(release_metadata, "CHANGELOG_PATH", changelog)

    assert release_metadata._extract_release_notes("0.1.1") == "### Added\n\n- Stable release note"


def test_extract_release_notes_uses_latest_version_without_expected_version(tmp_path, monkeypatch):
    changelog = tmp_path / "CHANGELOG.md"
    changelog.write_text(
        """# Changelog

## [0.1.1] - 2026-03-13

### Fixed

- Pending fix

## [0.1.0] - 2025-12-29

### Added

- Initial release
""",
        encoding="utf-8",
    )
    monkeypatch.setattr(release_metadata, "CHANGELOG_PATH", changelog)

    assert release_metadata._extract_release_notes(None) == "### Fixed\n\n- Pending fix"


def test_previous_stable_tag_excludes_current_tag(monkeypatch):
    monkeypatch.setattr(
        release_metadata.subprocess,
        "check_output",
        lambda *args, **kwargs: "v0.1.0\nv0.1.1\nv0.1.2\nv0.1.3-rc1\n",
    )

    assert release_metadata._get_previous_stable_tag(excluded_tag="v0.1.2") == "v0.1.1"