"""Tests for builtin path handling shared by the PTY shell core."""

from __future__ import annotations

import os
import tempfile

import pytest

from aish.shell.commands.handlers import BuiltinHandlers, DirectoryStack


def test_cd_with_unquoted_spaces_returns_tip_and_target_dir():
    with tempfile.TemporaryDirectory() as temp_dir:
        target = os.path.join(temp_dir, "dir with spaces")
        os.makedirs(target)

        result = BuiltinHandlers.handle_cd(
            f"cd {target}",
            cwd=temp_dir,
            directory_stack=DirectoryStack(),
        )

        assert result.success is True
        assert "Use quotes for paths with spaces" in result.output
        assert result.new_cwd == os.path.abspath(target)
        assert result.env_vars_to_set["PWD"] == os.path.abspath(target)


def test_cd_with_nonexistent_unquoted_spaces_returns_helpful_error():
    result = BuiltinHandlers.handle_cd(
        "cd /tmp/path with spaces that does not exist",
        cwd="/tmp",
        directory_stack=DirectoryStack(),
    )

    assert result.success is False
    assert "Use quotes for paths with spaces" in result.error


def test_pushd_with_unquoted_spaces_returns_tip_and_pushes_stack():
    with tempfile.TemporaryDirectory() as temp_dir:
        target = os.path.join(temp_dir, "pushd dir with spaces")
        os.makedirs(target)

        result = BuiltinHandlers.handle_pushd(
            f"pushd {target}",
            cwd=temp_dir,
            directory_stack=DirectoryStack(),
        )

        assert result.success is True
        assert "Use quotes for paths with spaces" in result.output
        assert result.new_cwd == os.path.abspath(target)
        assert result.directory_stack_push == temp_dir


@pytest.mark.skipif(not hasattr(os, "symlink"), reason="symlink not supported")
def test_cd_physical_mode_resolves_symlink():
    with tempfile.TemporaryDirectory() as temp_dir:
        actual_dir = os.path.join(temp_dir, "actual")
        link_dir = os.path.join(temp_dir, "link")
        os.makedirs(actual_dir)
        os.symlink(actual_dir, link_dir)

        result = BuiltinHandlers.handle_cd(
            f'cd -P "{link_dir}"',
            cwd=temp_dir,
            directory_stack=DirectoryStack(),
        )

        assert result.success is True
        assert result.new_cwd == os.path.realpath(link_dir)
        assert result.env_vars_to_set["PWD"] == os.path.realpath(link_dir)
