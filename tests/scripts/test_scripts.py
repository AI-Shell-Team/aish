"""Tests for the script system."""

from __future__ import annotations

import tempfile
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock

import pytest

from aish.scripts import (
    Script,
    ScriptArgument,
    ScriptExecutor,
    ScriptHotReloadService,
    ScriptLoader,
    ScriptMetadata,
    ScriptRegistry,
)
from aish.scripts.hooks import HookManager


class TestScriptModels:
    """Tests for script Pydantic models."""

    def test_script_metadata_minimal(self) -> None:
        """Test creating metadata with minimal fields."""
        metadata = ScriptMetadata(name="test")
        assert metadata.name == "test"
        assert metadata.description == ""
        assert metadata.version == "1.0.0"
        assert metadata.arguments == []
        assert metadata.type == "command"

    def test_script_metadata_full(self) -> None:
        """Test creating metadata with all fields."""
        metadata = ScriptMetadata(
            name="deploy",
            description="Deploy to server",
            version="2.0.0",
            arguments=[
                ScriptArgument(name="env", description="Environment", default="staging"),
                ScriptArgument(name="version", required=True),
            ],
            type="hook",
            hook_event="precmd",
        )
        assert metadata.name == "deploy"
        assert metadata.description == "Deploy to server"
        assert metadata.version == "2.0.0"
        assert len(metadata.arguments) == 2
        assert metadata.arguments[0].name == "env"
        assert metadata.arguments[0].default == "staging"
        assert metadata.arguments[1].required is True
        assert metadata.type == "hook"
        assert metadata.hook_event == "precmd"

    def test_script_metadata_name_validation(self) -> None:
        """Test script name validation."""
        # Valid names
        ScriptMetadata(name="test")
        ScriptMetadata(name="test-script")
        ScriptMetadata(name="test_script")
        ScriptMetadata(name="test123")
        ScriptMetadata(name="a")

        # Note: uppercase is converted to lowercase automatically
        metadata = ScriptMetadata(name="Test")
        assert metadata.name == "test"

        # Invalid names
        with pytest.raises(ValueError):
            ScriptMetadata(name="-test")  # Starts with hyphen
        with pytest.raises(ValueError):
            ScriptMetadata(name="test script")  # Space
        with pytest.raises(ValueError):
            ScriptMetadata(name="x" * 65)  # Too long

    def test_script_model(self) -> None:
        """Test Script model."""
        script = Script(
            metadata=ScriptMetadata(name="test"),
            content="echo hello",
            file_path="/tmp/test.aish",
            base_dir="/tmp",
        )
        assert script.name == "test"
        assert script.is_hook is False
        assert script.hook_event is None

    def test_script_model_hook(self) -> None:
        """Test Script model with hook type."""
        script = Script(
            metadata=ScriptMetadata(name="aish_prompt", type="hook", hook_event="prompt"),
            content="echo 'prompt > '",
            file_path="/tmp/aish_prompt.aish",
            base_dir="/tmp",
        )
        assert script.is_hook is True
        assert script.hook_event == "prompt"


class TestScriptLoader:
    """Tests for script loader."""

    def test_loader_default_scripts_dir(self) -> None:
        """Test default scripts directory."""
        loader = ScriptLoader()
        # Should use default location
        assert "scripts" in str(loader.get_scripts_dir())

    def test_loader_custom_scripts_dir(self) -> None:
        """Test custom scripts directory."""
        with tempfile.TemporaryDirectory() as tmpdir:
            loader = ScriptLoader(scripts_dir=Path(tmpdir))
            assert loader.get_scripts_dir() == Path(tmpdir)

    def test_parse_script_file_with_frontmatter(self) -> None:
        """Test parsing script file with YAML frontmatter."""
        with tempfile.TemporaryDirectory() as tmpdir:
            script_path = Path(tmpdir) / "test.aish"
            script_path.write_text(
                """---
name: myscript
description: My test script
arguments:
  - name: input
    description: Input value
    required: true
---

echo "Input: $AISH_ARG_INPUT"
""",
                encoding="utf-8",
            )

            loader = ScriptLoader(scripts_dir=Path(tmpdir))
            script = loader.parse_script_file(script_path)

            assert script is not None
            assert script.name == "myscript"
            assert script.metadata.description == "My test script"
            assert len(script.metadata.arguments) == 1
            assert script.metadata.arguments[0].name == "input"
            assert 'echo "Input: $AISH_ARG_INPUT"' in script.content

    def test_parse_script_file_without_frontmatter(self) -> None:
        """Test parsing script file without frontmatter (uses filename)."""
        with tempfile.TemporaryDirectory() as tmpdir:
            script_path = Path(tmpdir) / "simple-script.aish"
            script_path.write_text(
                'echo "Hello World"',
                encoding="utf-8",
            )

            loader = ScriptLoader(scripts_dir=Path(tmpdir))
            script = loader.parse_script_file(script_path)

            assert script is not None
            assert script.name == "simple-script"
            assert script.content == 'echo "Hello World"'

    def test_scan_scripts(self) -> None:
        """Test scanning multiple scripts."""
        with tempfile.TemporaryDirectory() as tmpdir:
            # Create multiple scripts
            (Path(tmpdir) / "script1.aish").write_text(
                "---\nname: script1\n---\necho 1", encoding="utf-8"
            )
            (Path(tmpdir) / "script2.aish").write_text(
                "---\nname: script2\n---\necho 2", encoding="utf-8"
            )

            loader = ScriptLoader(scripts_dir=Path(tmpdir))
            scripts = loader.scan_scripts()

            assert len(scripts) == 2
            assert "script1" in scripts
            assert "script2" in scripts


class TestScriptRegistry:
    """Tests for script registry."""

    def test_registry_initial_state(self) -> None:
        """Test registry initial state."""
        with tempfile.TemporaryDirectory() as tmpdir:
            registry = ScriptRegistry(scripts_dir=Path(tmpdir))
            assert registry.has_script("test") is False
            assert registry.get_script("test") is None
            assert registry.list_scripts() == []

    def test_registry_load_scripts(self) -> None:
        """Test loading scripts into registry."""
        with tempfile.TemporaryDirectory() as tmpdir:
            # Create a script
            (Path(tmpdir) / "test.aish").write_text(
                "---\nname: test\n---\necho test", encoding="utf-8"
            )

            registry = ScriptRegistry(scripts_dir=Path(tmpdir))
            registry.load_all_scripts()

            assert registry.has_script("test")
            script = registry.get_script("test")
            assert script is not None
            assert script.name == "test"

    def test_registry_invalidate_reload(self) -> None:
        """Test invalidate and reload mechanism."""
        with tempfile.TemporaryDirectory() as tmpdir:
            registry = ScriptRegistry(scripts_dir=Path(tmpdir))
            registry.load_all_scripts()

            # Add a new script
            (Path(tmpdir) / "new.aish").write_text(
                "---\nname: new\n---\necho new", encoding="utf-8"
            )

            # Invalidate
            registry.invalidate()
            assert registry.is_dirty

            # Reload
            reloaded = registry.reload_if_dirty()
            assert reloaded
            assert registry.has_script("new")


class TestScriptExecutor:
    """Tests for script executor."""

    @pytest.mark.asyncio
    async def test_execute_simple_script(self) -> None:
        """Test executing a simple script."""
        script = Script(
            metadata=ScriptMetadata(name="test"),
            content='echo "Hello World"',
            file_path="/tmp/test.aish",
            base_dir="/tmp",
        )

        executor = ScriptExecutor()
        result = await executor.execute(script)

        assert result.success
        assert "Hello World" in result.output

    @pytest.mark.asyncio
    async def test_execute_script_with_arguments(self) -> None:
        """Test executing script with arguments."""
        script = Script(
            metadata=ScriptMetadata(
                name="greet",
                arguments=[
                    ScriptArgument(name="NAME", default="World"),
                ],
            ),
            content='echo "Hello, $AISH_ARG_NAME!"',
            file_path="/tmp/greet.aish",
            base_dir="/tmp",
        )

        executor = ScriptExecutor()
        result = await executor.execute(script, args=["Alice"])

        assert result.success
        assert "Hello, Alice!" in result.output

    @pytest.mark.asyncio
    async def test_execute_script_with_required_argument_missing(self) -> None:
        """Test executing script with missing required argument."""
        script = Script(
            metadata=ScriptMetadata(
                name="needs_arg",
                arguments=[
                    ScriptArgument(name="value", required=True),
                ],
            ),
            content="echo $AISH_ARG_VALUE",
            file_path="/tmp/needs_arg.aish",
            base_dir="/tmp",
        )

        executor = ScriptExecutor()
        result = await executor.execute(script, args=[])

        assert result.success is False
        assert "required" in result.error.lower()

    @pytest.mark.asyncio
    async def test_execute_script_cd_command(self) -> None:
        """Test executing script with cd command."""
        with tempfile.TemporaryDirectory() as tmpdir:
            script = Script(
                metadata=ScriptMetadata(name="cd_test"),
                content=f"cd {tmpdir}",
                file_path="/tmp/cd_test.aish",
                base_dir="/tmp",
            )

            executor = ScriptExecutor()
            result = await executor.execute(script)

            assert result.success
            assert result.new_cwd == tmpdir

    @pytest.mark.asyncio
    async def test_execute_script_export_command(self) -> None:
        """Test executing script with export command."""
        script = Script(
            metadata=ScriptMetadata(name="export_test"),
            content='export MY_VAR="test_value"',
            file_path="/tmp/export_test.aish",
            base_dir="/tmp",
        )

        executor = ScriptExecutor()
        result = await executor.execute(script)

        assert result.success
        assert "MY_VAR" in result.env_changes
        assert result.env_changes["MY_VAR"] == "test_value"

    @pytest.mark.asyncio
    async def test_execute_script_ai_call(self) -> None:
        """Test executing script with AI call."""
        script = Script(
            metadata=ScriptMetadata(name="ai_test"),
            content='ai "What is 2+2?"',
            file_path="/tmp/ai_test.aish",
            base_dir="/tmp",
        )

        # Mock LLM session
        mock_llm = MagicMock()
        mock_llm.completion = AsyncMock(return_value="2+2 equals 4")

        executor = ScriptExecutor(llm_session=mock_llm)
        result = await executor.execute(script)

        assert result.success
        assert "4" in result.output
        mock_llm.completion.assert_called_once()

    def test_execute_sync(self) -> None:
        """Test synchronous script execution."""
        script = Script(
            metadata=ScriptMetadata(name="sync_test"),
            content='echo "sync output"',
            file_path="/tmp/sync_test.aish",
            base_dir="/tmp",
        )

        executor = ScriptExecutor()
        result = executor.execute_sync(script)

        assert result.success
        assert "sync output" in result.output


class TestHookManager:
    """Tests for hook manager."""

    def test_has_hook_false_when_no_hook(self) -> None:
        """Test has_hook returns False when hook doesn't exist."""
        with tempfile.TemporaryDirectory() as tmpdir:
            registry = ScriptRegistry(scripts_dir=Path(tmpdir))
            executor = ScriptExecutor()
            manager = HookManager(registry, executor)

            assert manager.has_hook("prompt") is False

    def test_has_hook_true_when_hook_exists(self) -> None:
        """Test has_hook returns True when hook exists."""
        with tempfile.TemporaryDirectory() as tmpdir:
            # Create a prompt hook script
            (Path(tmpdir) / "aish_prompt.aish").write_text(
                'echo "custom > "', encoding="utf-8"
            )

            registry = ScriptRegistry(scripts_dir=Path(tmpdir))
            registry.load_all_scripts()

            executor = ScriptExecutor()
            manager = HookManager(registry, executor)

            assert manager.has_hook("prompt") is True

    def test_run_prompt_hook(self) -> None:
        """Test running prompt hook."""
        with tempfile.TemporaryDirectory() as tmpdir:
            # Create a prompt hook script
            (Path(tmpdir) / "aish_prompt.aish").write_text(
                'echo "🚀 custom > "', encoding="utf-8"
            )

            registry = ScriptRegistry(scripts_dir=Path(tmpdir))
            registry.load_all_scripts()

            executor = ScriptExecutor()
            manager = HookManager(registry, executor)

            prompt = manager.run_prompt_hook()
            assert "custom" in prompt

    def test_run_prompt_hook_preserves_trailing_space(self) -> None:
        """Prompt hook should preserve trailing space for cursor separation."""
        with tempfile.TemporaryDirectory() as tmpdir:
            (Path(tmpdir) / "aish_prompt.aish").write_text(
                'printf "prompt > "', encoding="utf-8"
            )

            registry = ScriptRegistry(scripts_dir=Path(tmpdir))
            registry.load_all_scripts()

            executor = ScriptExecutor()
            manager = HookManager(registry, executor)

            prompt = manager.run_prompt_hook()
            assert prompt == "prompt > "


class TestScriptHotReloadService:
    """Tests for script hot reload service."""

    def test_service_initialization(self) -> None:
        """Test service initialization."""
        with tempfile.TemporaryDirectory() as tmpdir:
            registry = ScriptRegistry(scripts_dir=Path(tmpdir))
            service = ScriptHotReloadService(registry)
            assert service.script_registry is registry
            assert service._running is False

    def test_service_start_stop(self) -> None:
        """Test starting and stopping service."""
        with tempfile.TemporaryDirectory() as tmpdir:
            registry = ScriptRegistry(scripts_dir=Path(tmpdir))
            service = ScriptHotReloadService(registry)

            service.start()
            assert service._running is True

            service.stop()
            assert service._running is False


class TestPromptEnvBuilder:
    """Tests for prompt environment variable building."""

    def test_virtual_env_detection_venv(self) -> None:
        """Test VIRTUAL_ENV detection."""
        with tempfile.TemporaryDirectory() as tmpdir:
            registry = ScriptRegistry(scripts_dir=Path(tmpdir))
            executor = ScriptExecutor()
            manager = HookManager(registry, executor)

            # Mock VIRTUAL_ENV
            import os
            old_venv = os.environ.get("VIRTUAL_ENV")
            old_conda = os.environ.get("CONDA_DEFAULT_ENV")

            try:
                os.environ["VIRTUAL_ENV"] = "/path/to/my-project/.venv"
                if "CONDA_DEFAULT_ENV" in os.environ:
                    del os.environ["CONDA_DEFAULT_ENV"]

                env = manager._build_prompt_env()
                assert env.get("AISH_VIRTUAL_ENV") == ".venv"
            finally:
                if old_venv:
                    os.environ["VIRTUAL_ENV"] = old_venv
                elif "VIRTUAL_ENV" in os.environ:
                    del os.environ["VIRTUAL_ENV"]
                if old_conda:
                    os.environ["CONDA_DEFAULT_ENV"] = old_conda

    def test_virtual_env_detection_conda(self) -> None:
        """Test CONDA_DEFAULT_ENV detection."""
        with tempfile.TemporaryDirectory() as tmpdir:
            registry = ScriptRegistry(scripts_dir=Path(tmpdir))
            executor = ScriptExecutor()
            manager = HookManager(registry, executor)

            import os
            old_venv = os.environ.get("VIRTUAL_ENV")
            old_conda = os.environ.get("CONDA_DEFAULT_ENV")

            try:
                if "VIRTUAL_ENV" in os.environ:
                    del os.environ["VIRTUAL_ENV"]
                os.environ["CONDA_DEFAULT_ENV"] = "my-conda-env"

                env = manager._build_prompt_env()
                assert env.get("AISH_VIRTUAL_ENV") == "my-conda-env"
            finally:
                if old_venv:
                    os.environ["VIRTUAL_ENV"] = old_venv
                if old_conda:
                    os.environ["CONDA_DEFAULT_ENV"] = old_conda
                elif "CONDA_DEFAULT_ENV" in os.environ:
                    del os.environ["CONDA_DEFAULT_ENV"]

    def test_virtual_env_priority_venv_over_conda(self) -> None:
        """Test VIRTUAL_ENV takes priority over CONDA_DEFAULT_ENV."""
        with tempfile.TemporaryDirectory() as tmpdir:
            registry = ScriptRegistry(scripts_dir=Path(tmpdir))
            executor = ScriptExecutor()
            manager = HookManager(registry, executor)

            import os
            old_venv = os.environ.get("VIRTUAL_ENV")
            old_conda = os.environ.get("CONDA_DEFAULT_ENV")

            try:
                os.environ["VIRTUAL_ENV"] = "/path/to/venv"
                os.environ["CONDA_DEFAULT_ENV"] = "conda-env"

                env = manager._build_prompt_env()
                # VIRTUAL_ENV should take priority
                assert env.get("AISH_VIRTUAL_ENV") == "venv"
            finally:
                if old_venv:
                    os.environ["VIRTUAL_ENV"] = old_venv
                elif "VIRTUAL_ENV" in os.environ:
                    del os.environ["VIRTUAL_ENV"]
                if old_conda:
                    os.environ["CONDA_DEFAULT_ENV"] = old_conda
                elif "CONDA_DEFAULT_ENV" in os.environ:
                    del os.environ["CONDA_DEFAULT_ENV"]

    def test_no_virtual_env(self) -> None:
        """Test when no virtual environment is active."""
        with tempfile.TemporaryDirectory() as tmpdir:
            registry = ScriptRegistry(scripts_dir=Path(tmpdir))
            executor = ScriptExecutor()
            manager = HookManager(registry, executor)

            import os
            old_venv = os.environ.get("VIRTUAL_ENV")
            old_conda = os.environ.get("CONDA_DEFAULT_ENV")

            try:
                if "VIRTUAL_ENV" in os.environ:
                    del os.environ["VIRTUAL_ENV"]
                if "CONDA_DEFAULT_ENV" in os.environ:
                    del os.environ["CONDA_DEFAULT_ENV"]

                env = manager._build_prompt_env()
                assert "AISH_VIRTUAL_ENV" not in env or env.get("AISH_VIRTUAL_ENV") is None
            finally:
                if old_venv:
                    os.environ["VIRTUAL_ENV"] = old_venv
                if old_conda:
                    os.environ["CONDA_DEFAULT_ENV"] = old_conda

    def test_exclude_aish_own_venv(self) -> None:
        """Test that aish's own .venv is excluded from detection."""
        with tempfile.TemporaryDirectory() as tmpdir:
            registry = ScriptRegistry(scripts_dir=Path(tmpdir))
            executor = ScriptExecutor()
            manager = HookManager(registry, executor)

            import os
            old_venv = os.environ.get("VIRTUAL_ENV")

            try:
                # Simulate aish's own venv (should be excluded)
                os.environ["VIRTUAL_ENV"] = "/home/user/projects/aish/.venv"
                env = manager._build_prompt_env()
                assert "AISH_VIRTUAL_ENV" not in env or env.get("AISH_VIRTUAL_ENV") is None

                # Simulate user's venv (should be included)
                os.environ["VIRTUAL_ENV"] = "/home/user/projects/myproject/.venv"
                env = manager._build_prompt_env()
                assert env.get("AISH_VIRTUAL_ENV") == ".venv"
            finally:
                if old_venv:
                    os.environ["VIRTUAL_ENV"] = old_venv
                elif "VIRTUAL_ENV" in os.environ:
                    del os.environ["VIRTUAL_ENV"]

