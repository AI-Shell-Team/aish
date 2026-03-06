from __future__ import annotations

from pathlib import Path

from aish.security.sandbox import SandboxConfig, SandboxExecutor


def _make_executor(repo_root: Path) -> SandboxExecutor:
    return SandboxExecutor(SandboxConfig(repo_root=repo_root))


def _changes_by_path_and_kind(changes: list) -> set[tuple[str, str]]:
    return {(c.path, c.kind) for c in changes}


def test_collect_fs_changes_reports_mode_delta_for_file_and_dir(tmp_path: Path) -> None:
    repo_root = tmp_path / "repo"
    lowerdir = repo_root / "lower"
    upperdir = repo_root / "upper"
    lowerdir.mkdir(parents=True)
    upperdir.mkdir(parents=True)

    lower_file = lowerdir / "a.txt"
    lower_file.write_text("v1", encoding="utf-8")
    lower_file.chmod(0o644)

    upper_file = upperdir / "a.txt"
    upper_file.write_text("v1", encoding="utf-8")
    upper_file.chmod(0o600)

    (lowerdir / "d").mkdir()
    (upperdir / "d").mkdir()
    (lowerdir / "d").chmod(0o755)
    (upperdir / "d").chmod(0o700)

    executor = _make_executor(repo_root=repo_root)
    changes = executor._collect_fs_changes(
        lowerdir=lowerdir, upperdir=upperdir, repo_root=repo_root
    )

    by_pk = {(c.path, c.kind): c for c in changes}
    assert ("lower/a.txt", "modified") in by_pk
    assert by_pk[("lower/a.txt", "modified")].detail == {"mode": "644->600"}

    assert ("lower/d", "modified") in by_pk
    assert by_pk[("lower/d", "modified")].detail == {"mode": "755->700"}


def test_collect_fs_changes_expands_non_empty_dir_deletion_from_whiteout(
    tmp_path: Path,
) -> None:
    repo_root = tmp_path / "repo"
    lowerdir = repo_root / "lower"
    upperdir = repo_root / "upper"
    lowerdir.mkdir(parents=True)
    upperdir.mkdir(parents=True)

    (lowerdir / "docs" / "sub").mkdir(parents=True)
    (lowerdir / "docs" / "a.txt").write_text("a", encoding="utf-8")
    (lowerdir / "docs" / "sub" / "b.txt").write_text("b", encoding="utf-8")

    # Simulate overlay whiteout for deleting an existing directory.
    (upperdir / ".wh.docs").write_text("", encoding="utf-8")

    executor = _make_executor(repo_root=repo_root)
    changes = executor._collect_fs_changes(
        lowerdir=lowerdir, upperdir=upperdir, repo_root=repo_root
    )

    deleted = {
        path for path, kind in _changes_by_path_and_kind(changes) if kind == "deleted"
    }
    assert "lower/docs" in deleted
    assert "lower/docs/a.txt" in deleted
    assert "lower/docs/sub/b.txt" in deleted


def test_collect_fs_changes_adds_missing_lower_files_for_opaque_dir(
    tmp_path: Path, monkeypatch
) -> None:
    repo_root = tmp_path / "repo"
    lowerdir = repo_root / "lower"
    upperdir = repo_root / "upper"
    lower_data = lowerdir / "data"
    upper_data = upperdir / "data"
    lower_data.mkdir(parents=True)
    upper_data.mkdir(parents=True)

    (lower_data / "keep.txt").write_text("k", encoding="utf-8")
    (lower_data / "gone.txt").write_text("g", encoding="utf-8")
    (upper_data / "keep.txt").write_text("k2", encoding="utf-8")

    real_getxattr = __import__("os").getxattr

    def fake_getxattr(path, name):
        if Path(path) == upper_data and name == b"trusted.overlay.opaque":
            return b"y"
        return real_getxattr(path, name)

    monkeypatch.setattr("os.getxattr", fake_getxattr)

    executor = _make_executor(repo_root=repo_root)
    changes = executor._collect_fs_changes(
        lowerdir=lowerdir, upperdir=upperdir, repo_root=repo_root
    )

    by_pk = {(c.path, c.kind): c for c in changes}
    assert ("lower/data/keep.txt", "modified") in by_pk
    assert ("lower/data/gone.txt", "deleted") in by_pk
