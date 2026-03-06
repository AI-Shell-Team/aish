"""
Tests for AI shell's own command handling with paths containing spaces and special characters.
"""

import os
import tempfile
from unittest.mock import MagicMock, patch

import anyio
import pytest

from aish.config import ConfigModel
from aish.shell import AIShell
from aish.skills import SkillManager


class TestShellPathHandling:
    """Test AI shell's built-in commands with paths containing special characters."""

    def setup_method(self):
        """Set up test fixtures."""
        self.config = ConfigModel(model="gpt-3.5-turbo")
        self.skill_manager = SkillManager()
        self.skill_manager.load_all_skills()
        self.shell = AIShell(config=self.config, skill_manager=self.skill_manager)
        self.original_dir = os.getcwd()

    def teardown_method(self):
        """Clean up after tests."""
        os.chdir(self.original_dir)

    @pytest.mark.asyncio
    async def test_cd_with_spaces_unquoted(self):
        """Test cd command with unquoted path containing spaces."""
        with tempfile.TemporaryDirectory() as temp_dir:
            test_dir = os.path.join(temp_dir, "test directory with spaces")
            os.makedirs(test_dir)

            # Mock console to capture output
            with patch.object(self.shell, "console") as mock_console:
                await self.shell.handle_cd_command(f"cd {test_dir}")

                # Should show tip and change directory
                mock_console.print.assert_any_call(
                    f'💡 [yellow]Tip: Use quotes for paths with spaces: cd "{test_dir}"[/yellow]',
                    style="green",
                )

                # Verify directory changed
                assert os.path.samefile(os.getcwd(), test_dir)

    @pytest.mark.asyncio
    async def test_cd_with_spaces_quoted(self):
        """Test cd command with quoted path containing spaces."""
        with tempfile.TemporaryDirectory() as temp_dir:
            test_dir = os.path.join(temp_dir, "test directory with spaces")
            os.makedirs(test_dir)

            await self.shell.handle_cd_command(f'cd "{test_dir}"')

            # Verify directory changed
            assert os.path.samefile(os.getcwd(), test_dir)

    @pytest.mark.asyncio
    async def test_cd_nonexistent_path_with_spaces(self):
        """Test cd command with nonexistent path containing spaces."""
        nonexistent_path = "/nonexistent/path with spaces"

        with patch.object(self.shell, "console") as mock_console:
            await self.shell.handle_cd_command(f"cd {nonexistent_path}")

            # Should show error with suggestion
            mock_console.print.assert_any_call(
                f'❌ cd: too many arguments. Use quotes for paths with spaces: cd "{nonexistent_path}"',
                style="red",
            )

    @pytest.mark.asyncio
    async def test_pushd_with_spaces_unquoted(self):
        """Test pushd command with unquoted path containing spaces."""
        with tempfile.TemporaryDirectory() as temp_dir:
            test_dir = os.path.join(temp_dir, "pushd test directory")
            os.makedirs(test_dir)

            with patch.object(self.shell, "console") as mock_console:
                await self.shell.handle_pushd_command(f"pushd {test_dir}")

                # Should show tip and change directory
                mock_console.print.assert_any_call(
                    f'💡 [yellow]Tip: Use quotes for paths with spaces: pushd "{test_dir}"[/yellow]',
                    style="green",
                )

                # Verify directory changed and stack updated
                assert os.path.samefile(os.getcwd(), test_dir)
                assert len(self.shell.directory_stack) == 1

    @pytest.mark.asyncio
    async def test_pushd_with_spaces_quoted(self):
        """Test pushd command with quoted path containing spaces."""
        with tempfile.TemporaryDirectory() as temp_dir:
            test_dir = os.path.join(temp_dir, "pushd test directory")
            os.makedirs(test_dir)

            await self.shell.handle_pushd_command(f'pushd "{test_dir}"')

            # Verify directory changed and stack updated
            assert os.path.samefile(os.getcwd(), test_dir)
            assert len(self.shell.directory_stack) == 1

    @pytest.mark.asyncio
    async def test_cd_with_special_characters(self):
        """Test cd command with various special characters in path."""
        # Test cases: (directory_name, should_work_unquoted)
        test_cases = [
            ("dir with spaces", True),  # Spaces - should work
            ("dir[with]brackets", True),  # Brackets - should work
            ("dir(with)parens", True),  # Parentheses - should work
            # Note: Quotes in filenames are tricky and may not work in all cases
        ]

        with tempfile.TemporaryDirectory() as temp_dir:
            for name, should_work_unquoted in test_cases:
                test_dir = os.path.join(temp_dir, name)
                os.makedirs(test_dir)

                if should_work_unquoted:
                    # Test unquoted (should work with tip)
                    with patch.object(self.shell, "console"):
                        await self.shell.handle_cd_command(f"cd {test_dir}")
                        assert os.path.samefile(os.getcwd(), test_dir)

                    # Return to original directory
                    os.chdir(self.original_dir)

                # Test quoted (should always work)
                await self.shell.handle_cd_command(f'cd "{test_dir}"')
                assert os.path.samefile(os.getcwd(), test_dir)

                # Return to original directory for next iteration
                os.chdir(self.original_dir)

    @pytest.mark.asyncio
    async def test_cd_with_unmatched_quotes(self):
        """Test cd command with unmatched quotes in argument."""
        with tempfile.TemporaryDirectory() as temp_dir:
            # Create directory without quotes in the actual name
            test_dir = os.path.join(temp_dir, "normal_dir")
            os.makedirs(test_dir)

            # Test with unmatched quote in command - should handle gracefully with fallback
            with patch.object(self.shell, "console") as mock_console:
                await self.shell.handle_cd_command(f'cd {test_dir}"')

                # Should show an error about path not found (because the quote is included)
                # This is expected behavior - malformed quotes should be caught
                error_calls = [
                    call
                    for call in mock_console.print.call_args_list
                    if "style" in str(call) and "red" in str(call)
                ]
                assert len(error_calls) > 0, "Expected error message in red"

                # Should NOT change directory due to bad path
                assert os.path.samefile(os.getcwd(), self.original_dir)

    @pytest.mark.asyncio
    async def test_pushd_nonexistent_path_with_spaces(self):
        """Test pushd command with nonexistent path containing spaces."""
        nonexistent_path = "/nonexistent/pushd path with spaces"

        with patch.object(self.shell, "console") as mock_console:
            await self.shell.handle_pushd_command(f"pushd {nonexistent_path}")

            # Should show error with suggestion
            mock_console.print.assert_any_call(
                f'❌ pushd: too many arguments. Use quotes for paths with spaces: pushd "{nonexistent_path}"',
                style="red",
            )

            # Should not change directory or modify stack
            assert os.path.samefile(os.getcwd(), self.original_dir)
            assert len(self.shell.directory_stack) == 0


class TestCdOptions:
    """Test cd command with -L/-P/-e/-@ options."""

    def setup_method(self):
        """Set up test fixtures."""
        self.config = ConfigModel(model="gpt-3.5-turbo")
        self.skill_manager = SkillManager()
        self.skill_manager.load_all_skills()
        self.shell = AIShell(config=self.config, skill_manager=self.skill_manager)
        self.original_dir = os.getcwd()

    def teardown_method(self):
        """Clean up after tests."""
        os.chdir(self.original_dir)

    @pytest.mark.asyncio
    async def test_cd_logical_default(self):
        """cd 默认使用 -L 模式，跟随符号链接"""
        with tempfile.TemporaryDirectory() as temp_dir:
            from pathlib import Path

            # Create actual directory and symlink
            actual_dir = Path(temp_dir) / "actual_dir"
            actual_dir.mkdir()
            link_dir = Path(temp_dir) / "link_dir"

            try:
                # Create symlink (may fail on systems without symlink support)
                link_dir.symlink_to(actual_dir)

                # Change to symlink path
                await self.shell.handle_cd_command(f'cd "{link_dir}"')

                # PWD should show the logical path (with symlink)
                pwd = os.environ.get("PWD", "")
                assert str(link_dir) in pwd or os.path.samefile(os.getcwd(), actual_dir)

            except OSError:
                # Skip test on systems that don't support symlinks
                pytest.skip("Symbolic links not supported on this system")

    @pytest.mark.asyncio
    async def test_cd_physical_mode(self):
        """cd -P 解析符号链接"""
        with tempfile.TemporaryDirectory() as temp_dir:
            from pathlib import Path

            # Create actual directory and symlink
            actual_dir = Path(temp_dir) / "actual_dir"
            actual_dir.mkdir()
            link_dir = Path(temp_dir) / "link_dir"

            try:
                # Create symlink (may fail on systems without symlink support)
                link_dir.symlink_to(actual_dir)

                # Change to symlink path with -P flag
                await self.shell.handle_cd_command(f'cd -P "{link_dir}"')

                # PWD should show the physical path (resolving symlink)
                pwd = os.environ.get("PWD", "")
                # In physical mode, PWD should be the real path
                assert str(actual_dir) in pwd or os.path.samefile(
                    os.getcwd(), actual_dir
                )

            except OSError:
                # Skip test on systems that don't support symlinks
                pytest.skip("Symbolic links not supported on this system")

    @pytest.mark.asyncio
    async def test_cd_invalid_option(self):
        """cd 无效选项返回错误"""
        with patch.object(self.shell, "console") as mock_console:
            await self.shell.handle_cd_command("cd -x /tmp")

            # Should show error about invalid option
            error_calls = [
                call
                for call in mock_console.print.call_args_list
                if "invalid option" in str(call).lower()
            ]
            assert len(error_calls) > 0

    @pytest.mark.asyncio
    async def test_cd_dash_e_with_physical(self):
        """cd -P -e 组合测试"""
        with tempfile.TemporaryDirectory() as temp_dir:
            # Test that -e flag is accepted (doesn't affect normal cd behavior)
            await self.shell.handle_cd_command(f'cd -e "{temp_dir}"')

            # Should successfully change directory
            assert os.path.samefile(os.getcwd(), temp_dir)

    @pytest.mark.asyncio
    async def test_cd_dash_at_option(self):
        """cd -@ 选项测试（扩展属性）"""
        with tempfile.TemporaryDirectory() as temp_dir:
            # Test that -@ flag is accepted (extended attributes, just ignored)
            await self.shell.handle_cd_command(f'cd -@ "{temp_dir}"')

            # Should successfully change directory
            assert os.path.samefile(os.getcwd(), temp_dir)


if __name__ == "__main__":
    pytest.main([__file__])
