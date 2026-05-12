from __future__ import annotations

from aish.tools.glob_tool import GlobTool
from aish.tools.grep_tool import GrepTool


def test_glob_rejects_root_outside_current_workspace(tmp_path, monkeypatch):
    workspace = tmp_path / "workspace"
    outside = tmp_path / "outside"
    workspace.mkdir()
    outside.mkdir()
    secret = outside / "token.txt"
    secret.write_text("AISH_VALIDATION_SECRET=sk-validation\n", encoding="utf-8")
    monkeypatch.chdir(workspace)

    result = GlobTool()(pattern="**/*.txt", root=str(outside))

    assert result.ok is False
    assert "root must be within the current workspace" in result.output
    assert "token.txt" not in result.output


def test_grep_rejects_root_outside_current_workspace(tmp_path, monkeypatch):
    workspace = tmp_path / "workspace"
    outside = tmp_path / "outside"
    workspace.mkdir()
    outside.mkdir()
    secret = outside / "token.txt"
    secret.write_text("AISH_VALIDATION_SECRET=sk-validation\n", encoding="utf-8")
    monkeypatch.chdir(workspace)

    result = GrepTool()(pattern="AISH_VALIDATION_SECRET", root=str(outside))

    assert result.ok is False
    assert "root must be within the current workspace" in result.output
    assert "sk-validation" not in result.output


def test_search_tools_allow_roots_inside_current_workspace(tmp_path, monkeypatch):
    workspace = tmp_path / "workspace"
    source = workspace / "src"
    source.mkdir(parents=True)
    file_path = source / "example.py"
    file_path.write_text("needle = True\n", encoding="utf-8")
    monkeypatch.chdir(workspace)

    glob_result = GlobTool()(pattern="**/*.py", root="src")
    grep_result = GrepTool()(pattern="needle", root="src")

    assert glob_result.ok is True
    assert str(file_path) in glob_result.output
    assert grep_result.ok is True
    assert f"{file_path}:1: needle = True" in grep_result.output
