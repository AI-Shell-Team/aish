from __future__ import annotations

import importlib.util
from pathlib import Path


MODULE_PATH = Path(__file__).resolve().parents[3] / "packaging" / "scripts" / "update_release_files.py"
SPEC = importlib.util.spec_from_file_location("update_release_files", MODULE_PATH)
assert SPEC is not None
assert SPEC.loader is not None
update_release_files = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(update_release_files)


def test_update_release_files_updates_uv_lock_package_version(tmp_path, monkeypatch):
    pyproject = tmp_path / "pyproject.toml"
    runtime = tmp_path / "__init__.py"
    changelog = tmp_path / "CHANGELOG.md"
    uv_lock = tmp_path / "uv.lock"

    pyproject.write_text('[project]\nversion = "0.1.0"\n', encoding="utf-8")
    runtime.write_text('__version__ = "0.1.0"\n', encoding="utf-8")
    changelog.write_text(
        """# Changelog

## [0.1.1] - 2026-03-13

### Added

- Previous release note
""",
        encoding="utf-8",
    )
    uv_lock.write_text(
        """version = 1

[[package]]
name = "aish"
version = "0.1.0"
source = { editable = "." }

[[package]]
name = "other"
version = "0.1.0"
""",
        encoding="utf-8",
    )

    monkeypatch.setattr(update_release_files, "PYPROJECT_PATH", pyproject)
    monkeypatch.setattr(update_release_files, "RUNTIME_VERSION_PATH", runtime)
    monkeypatch.setattr(update_release_files, "CHANGELOG_PATH", changelog)
    monkeypatch.setattr(update_release_files, "UV_LOCK_PATH", uv_lock)

    update_release_files.update_release_files("0.1.2", "2026-03-16")

    assert 'version = "0.1.2"' in uv_lock.read_text(encoding="utf-8")
    assert 'name = "other"\nversion = "0.1.0"' in uv_lock.read_text(encoding="utf-8")
    assert "## [0.1.2] - 2026-03-16\n\n## [0.1.1] - 2026-03-13" in changelog.read_text(encoding="utf-8")


def test_update_uv_lock_is_noop_when_lockfile_missing(tmp_path, monkeypatch):
    pyproject = tmp_path / "pyproject.toml"
    runtime = tmp_path / "__init__.py"
    changelog = tmp_path / "CHANGELOG.md"
    missing_lock = tmp_path / "missing.lock"

    pyproject.write_text('[project]\nversion = "0.1.0"\n', encoding="utf-8")
    runtime.write_text('__version__ = "0.1.0"\n', encoding="utf-8")
    changelog.write_text(
        """# Changelog

## [0.1.1] - 2026-03-13

### Added

- Previous release note
""",
        encoding="utf-8",
    )

    monkeypatch.setattr(update_release_files, "PYPROJECT_PATH", pyproject)
    monkeypatch.setattr(update_release_files, "RUNTIME_VERSION_PATH", runtime)
    monkeypatch.setattr(update_release_files, "CHANGELOG_PATH", changelog)
    monkeypatch.setattr(update_release_files, "UV_LOCK_PATH", missing_lock)

    update_release_files.update_release_files("0.1.2", "2026-03-16")

    assert not missing_lock.exists()


def test_update_release_files_rejects_existing_version_section(tmp_path, monkeypatch):
    pyproject = tmp_path / "pyproject.toml"
    runtime = tmp_path / "__init__.py"
    changelog = tmp_path / "CHANGELOG.md"
    uv_lock = tmp_path / "uv.lock"

    pyproject.write_text('[project]\nversion = "0.1.0"\n', encoding="utf-8")
    runtime.write_text('__version__ = "0.1.0"\n', encoding="utf-8")
    changelog.write_text(
        """# Changelog

## [0.1.2] - 2026-03-16

### Added

- Existing notes
""",
        encoding="utf-8",
    )
    uv_lock.write_text(
        """version = 1

[[package]]
name = "aish"
version = "0.1.0"
source = { editable = "." }
""",
        encoding="utf-8",
    )

    monkeypatch.setattr(update_release_files, "PYPROJECT_PATH", pyproject)
    monkeypatch.setattr(update_release_files, "RUNTIME_VERSION_PATH", runtime)
    monkeypatch.setattr(update_release_files, "CHANGELOG_PATH", changelog)
    monkeypatch.setattr(update_release_files, "UV_LOCK_PATH", uv_lock)

    try:
        update_release_files.update_release_files("0.1.2", "2026-03-16")
    except ValueError as exc:
        assert "already contains a section" in str(exc)
    else:
        raise AssertionError("Expected duplicate changelog version section to be rejected")


def test_update_release_files_inserts_new_version_section(tmp_path, monkeypatch):
    pyproject = tmp_path / "pyproject.toml"
    runtime = tmp_path / "__init__.py"
    changelog = tmp_path / "CHANGELOG.md"
    uv_lock = tmp_path / "uv.lock"

    pyproject.write_text('[project]\nversion = "0.1.0"\n', encoding="utf-8")
    runtime.write_text('__version__ = "0.1.0"\n', encoding="utf-8")
    changelog.write_text(
        """# Changelog

## [0.1.1] - 2026-03-13

### Added

- Previous release note
""",
        encoding="utf-8",
    )
    uv_lock.write_text(
        """version = 1

[[package]]
name = "aish"
version = "0.1.0"
source = { editable = "." }
""",
        encoding="utf-8",
    )

    monkeypatch.setattr(update_release_files, "PYPROJECT_PATH", pyproject)
    monkeypatch.setattr(update_release_files, "RUNTIME_VERSION_PATH", runtime)
    monkeypatch.setattr(update_release_files, "CHANGELOG_PATH", changelog)
    monkeypatch.setattr(update_release_files, "UV_LOCK_PATH", uv_lock)

    update_release_files.update_release_files("0.1.2", "2026-03-16")

    changelog_text = changelog.read_text(encoding="utf-8")
    assert "## [0.1.2] - 2026-03-16" in changelog_text
    assert "## [0.1.2] - 2026-03-16\n\n## [0.1.1] - 2026-03-13" in changelog_text
