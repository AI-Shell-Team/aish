"""Tests for CLI functionality."""

from unittest.mock import Mock, patch

from typer.testing import CliRunner

from aish.cli import app, run
from aish.config import ConfigModel
from aish.i18n import t


class TestCLI:
    """Tests for CLI commands"""

    def setup_method(self):
        """Set up test fixtures"""
        self.runner = CliRunner()

    def test_info_command(self):
        """Test info command"""
        result = self.runner.invoke(app, ["info"])

        assert result.exit_code == 0
        assert "AI Shell" in result.output
        assert "Features:" in result.output
        assert "Supported Models:" in result.output

    @patch("aish.cli.AIShell")
    @patch("aish.cli.anyio.run")
    def test_run_command_default(self, mock_anyio_run, mock_shell_class):
        """Test run command with default parameters"""
        mock_shell = Mock()
        mock_shell_class.return_value = mock_shell

        with patch("aish.cli.needs_interactive_setup", return_value=False):
            result = self.runner.invoke(app, ["run"])

        assert result.exit_code == 0
        mock_shell_class.assert_called_once()
        mock_anyio_run.assert_called_once()

    @patch("aish.cli.AIShell")
    @patch("aish.cli.anyio.run")
    def test_default_invokes_run(self, mock_anyio_run, mock_shell_class):
        """Running `aish` with no args should default to `run`."""

        mock_shell = Mock()
        mock_shell_class.return_value = mock_shell

        with patch("aish.cli.needs_interactive_setup", return_value=False):
            result = self.runner.invoke(app, [])

        assert result.exit_code == 0
        mock_shell_class.assert_called_once()
        mock_anyio_run.assert_called_once()

    @patch("aish.cli.AIShell")
    @patch("aish.cli.anyio.run")
    def test_run_command_custom_model(self, mock_anyio_run, mock_shell_class):
        """Test run command with custom model"""
        mock_shell = Mock()
        mock_shell_class.return_value = mock_shell

        with patch("aish.cli.needs_interactive_setup", return_value=False):
            result = self.runner.invoke(app, ["run", "--model", "gpt-4"])

        assert result.exit_code == 0
        mock_shell_class.assert_called_once()
        mock_anyio_run.assert_called_once()

    @patch("aish.cli.AIShell")
    @patch("aish.cli.anyio.run")
    @patch("aish.cli.os.environ", {})
    def test_run_command_with_api_key(self, mock_anyio_run, mock_shell_class):
        """Test run command with API key"""
        mock_shell = Mock()
        mock_shell_class.return_value = mock_shell

        result = self.runner.invoke(
            app, ["run", "--model", "gpt-4", "--api-key", "test-key"]
        )

        assert result.exit_code == 0
        mock_shell_class.assert_called_once()
        mock_anyio_run.assert_called_once()

    @patch("aish.cli.AIShell")
    @patch("aish.cli.anyio.run")
    @patch("aish.cli.os.getenv")
    def test_run_command_no_api_key_minimal_output(
        self, mock_getenv, mock_anyio_run, mock_shell_class
    ):
        """Run command keeps startup output minimal when API key is absent."""
        mock_shell = Mock()
        mock_shell_class.return_value = mock_shell
        mock_getenv.return_value = None  # No API key set

        with patch("aish.cli.needs_interactive_setup", return_value=False):
            result = self.runner.invoke(app, ["run"])

        assert result.exit_code == 0
        assert t("cli.startup.no_api_key_warning") not in result.output
        mock_shell_class.assert_called_once()
        mock_anyio_run.assert_called_once()

    @patch("aish.cli.AIShell")
    @patch("aish.cli.anyio.run")
    def test_run_command_keyboard_interrupt(self, mock_anyio_run, mock_shell_class):
        """Test run command handles keyboard interrupt"""
        mock_shell = Mock()
        mock_shell_class.return_value = mock_shell
        mock_anyio_run.side_effect = KeyboardInterrupt()

        with patch("aish.cli.needs_interactive_setup", return_value=False):
            result = self.runner.invoke(app, ["run"])

        assert result.exit_code == 0
        assert t("cli.startup.goodbye") in result.output
        mock_shell_class.assert_called_once()
        mock_anyio_run.assert_called_once()

    @patch("aish.cli.run_interactive_setup")
    @patch("aish.cli.AIShell")
    @patch("aish.cli.anyio.run")
    def test_run_skips_interactive_setup_when_config_present(
        self, mock_anyio_run, mock_shell_class, mock_run_interactive_setup
    ):
        """Run should not invoke setup when config already has model + api_key."""

        mock_shell = Mock()
        mock_shell_class.return_value = mock_shell

        mock_config = Mock()
        mock_config.config_file = "/tmp/aish-config.yaml"
        mock_config.model_config = ConfigModel(
            model="openai/deepseek-chat",
            api_key="k",
            api_base="https://example.com",
        )

        with (
            patch("aish.cli.Config", return_value=mock_config),
            patch(
                "aish.cli._load_raw_yaml_config",
                return_value={
                    "model": "openai/deepseek-chat",
                    "api_key": "k",
                    "api_base": "https://example.com",
                },
            ),
        ):
            run(model=None, api_key=None, api_base=None, config_file=None)

        mock_run_interactive_setup.assert_not_called()

    @patch("aish.cli.run_interactive_setup")
    @patch("aish.cli.AIShell")
    @patch("aish.cli.anyio.run")
    def test_run_invokes_interactive_setup_when_config_missing(
        self, mock_anyio_run, mock_shell_class, mock_run_interactive_setup
    ):
        """Run should invoke setup when model or api_key is missing."""

        mock_shell = Mock()
        mock_shell_class.return_value = mock_shell

        mock_config = Mock()
        mock_config.config_file = "/tmp/aish-config.yaml"
        mock_config.model_config = ConfigModel()
        mock_run_interactive_setup.return_value = mock_config.model_config

        with (
            patch("aish.cli.Config", return_value=mock_config),
            patch("aish.cli._load_raw_yaml_config", return_value={}),
        ):
            run(model=None, api_key=None, api_base=None, config_file=None)

        mock_run_interactive_setup.assert_called_once_with(mock_config)

    @patch("aish.cli.Config")
    @patch("aish.cli.run_interactive_setup")
    def test_setup_command_success(self, mock_run_interactive_setup, mock_config_class):
        """Setup command exits successfully when interactive setup returns config."""
        mock_config = Mock()
        mock_config_class.return_value = mock_config
        mock_run_interactive_setup.return_value = ConfigModel(
            model="openai/gpt-4o", api_key="k"
        )

        result = self.runner.invoke(app, ["setup"])

        assert result.exit_code == 0
        mock_config_class.assert_called_once()
        mock_run_interactive_setup.assert_called_once_with(mock_config)

    @patch("aish.cli.Config")
    @patch("aish.cli.run_interactive_setup", return_value=None)
    def test_setup_command_cancelled(
        self, mock_run_interactive_setup, mock_config_class
    ):
        """Setup command returns non-zero when interactive setup is cancelled."""
        mock_config = Mock()
        mock_config_class.return_value = mock_config

        result = self.runner.invoke(app, ["setup"])

        assert result.exit_code == 1
        assert t("cli.setup.cancelled") in result.output
        mock_config_class.assert_called_once()
        mock_run_interactive_setup.assert_called_once_with(mock_config)
