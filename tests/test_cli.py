"""Tests for CLI functionality."""

from dataclasses import dataclass
from pathlib import Path
from unittest.mock import Mock, patch

import pytest
from typer.testing import CliRunner

from aish.cli import app, run
from aish.config import ConfigModel
from aish.i18n import t
from aish.providers.interface import ProviderAuthConfig
from aish.providers.openai_codex import OpenAICodexAuthError


@dataclass
class _FakeAuthState:
    auth_path: Path


def _has_free_key_module() -> bool:
    """Check if free key functionality is available (binary or Python package)."""
    try:
        from aish.wizard.setup_wizard import HAS_FREE_KEY_MODULE
        return HAS_FREE_KEY_MODULE
    except ImportError:
        return False


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

    @patch("aish.cli.Config")
    @patch("aish.providers.openai_codex.load_openai_codex_auth")
    def test_models_auth_login_sets_default_openai_codex_model(
        self, mock_load_openai_codex_auth, mock_config_class
    ):
        """Codex auth login should persist auth path and default model."""
        mock_config = Mock()
        mock_config.model_config = ConfigModel(model="openai/gpt-4o", api_key="k")
        mock_config_class.return_value = mock_config
        mock_load_openai_codex_auth.return_value = Mock(
            auth_path=Path("/tmp/codex-auth.json")
        )

        result = self.runner.invoke(
            app,
            ["models", "auth", "login", "--provider", "openai-codex"],
        )

        assert result.exit_code == 0
        assert mock_config.config_model.model == "openai-codex/gpt-5.4"
        assert mock_config.config_model.api_key is None
        assert mock_config.config_model.codex_auth_path == "/tmp/codex-auth.json"
        mock_config.save_config.assert_called_once()

    @patch("aish.cli.Config")
    @patch("aish.providers.openai_codex.load_openai_codex_auth")
    def test_models_auth_login_without_default_keeps_existing_api_key(
        self, mock_load_openai_codex_auth, mock_config_class
    ):
        """Codex auth login should not clear the current provider config when not switching defaults."""
        mock_config = Mock()
        mock_config.model_config = ConfigModel(model="openai/gpt-4o", api_key="k")
        mock_config_class.return_value = mock_config
        mock_load_openai_codex_auth.return_value = Mock(
            auth_path=Path("/tmp/codex-auth.json")
        )

        result = self.runner.invoke(
            app,
            [
                "models",
                "auth",
                "login",
                "--provider",
                "openai-codex",
                "--no-set-default",
            ],
        )

        assert result.exit_code == 0
        assert mock_config.config_model.model == "openai/gpt-4o"
        assert mock_config.config_model.api_key == "k"
        assert mock_config.config_model.codex_auth_path == "/tmp/codex-auth.json"
        mock_config.save_config.assert_called_once()

    @patch("aish.cli.Config")
    @patch("aish.providers.openai_codex.login_openai_codex_with_browser")
    @patch("aish.providers.openai_codex.load_openai_codex_auth")
    def test_models_auth_login_uses_builtin_browser_flow_by_default(
        self,
        mock_load_openai_codex_auth,
        mock_login_browser,
        mock_config_class,
    ):
        """Codex auth login should use the built-in browser flow by default."""
        mock_config = Mock()
        mock_config.model_config = ConfigModel(model="openai/gpt-4o", api_key="k")
        mock_config_class.return_value = mock_config
        mock_load_openai_codex_auth.side_effect = OpenAICodexAuthError("missing")
        mock_login_browser.return_value = Mock(auth_path=Path("/tmp/codex-auth.json"))

        result = self.runner.invoke(
            app,
            [
                "models",
                "auth",
                "login",
                "--provider",
                "openai-codex",
                "--no-open-browser",
            ],
        )

        assert result.exit_code == 0
        mock_login_browser.assert_called_once()
        assert mock_login_browser.call_args.kwargs["open_browser"] is False
        assert mock_login_browser.call_args.kwargs["auth_path"] is None

    @patch("aish.cli.Config")
    @patch("aish.providers.openai_codex.login_openai_codex_with_device_code")
    @patch("aish.providers.openai_codex.load_openai_codex_auth")
    def test_models_auth_login_supports_device_code_flow(
        self,
        mock_load_openai_codex_auth,
        mock_login_device_code,
        mock_config_class,
    ):
        """Codex auth login should support the built-in device-code flow."""
        mock_config = Mock()
        mock_config.model_config = ConfigModel(model="openai/gpt-4o", api_key="k")
        mock_config_class.return_value = mock_config
        mock_load_openai_codex_auth.side_effect = OpenAICodexAuthError("missing")
        mock_login_device_code.return_value = Mock(
            auth_path=Path("/tmp/codex-auth.json")
        )

        result = self.runner.invoke(
            app,
            [
                "models",
                "auth",
                "login",
                "--provider",
                "openai-codex",
                "--auth-flow",
                "device-code",
            ],
        )

        assert result.exit_code == 0
        mock_login_device_code.assert_called_once()

    def test_models_auth_login_rejects_unknown_provider(self):
        result = self.runner.invoke(
            app,
            ["models", "auth", "login", "--provider", "github-copilot"],
        )

        assert result.exit_code == 1
        assert "Unsupported provider `github-copilot`" in result.output

    @patch("aish.cli.Config")
    @patch("aish.cli.get_provider_by_id")
    def test_models_auth_login_dispatches_through_provider_contract(
        self,
        mock_get_provider_by_id,
        mock_config_class,
    ):
        mock_config = Mock()
        mock_config.model_config = ConfigModel(model="openai/gpt-4o", api_key="k")
        mock_config_class.return_value = mock_config

        load_auth_state = Mock(side_effect=RuntimeError("missing"))
        login_with_browser = Mock(return_value=_FakeAuthState(Path("/tmp/fake-auth.json")))
        fake_provider = Mock(
            provider_id="fake-provider",
            model_prefix="fake-provider",
            display_name="Fake Provider",
            auth_config=ProviderAuthConfig(
                auth_path_config_key="codex_auth_path",
                default_model="model-x",
                load_auth_state=load_auth_state,
                login_handlers={"browser": login_with_browser},
            ),
        )
        mock_get_provider_by_id.return_value = fake_provider

        result = self.runner.invoke(
            app,
            ["models", "auth", "login", "--provider", "fake-provider"],
        )

        assert result.exit_code == 0
        load_auth_state.assert_called_once_with(None)
        login_with_browser.assert_called_once()
        assert mock_config.config_model.model == "fake-provider/model-x"
        assert mock_config.config_model.codex_auth_path == "/tmp/fake-auth.json"


@pytest.mark.skipif(
    not _has_free_key_module(),
    reason="Free key module not available - these tests require the binary or Python package",
)
class TestSetupWizardFreeKeyHelpers:
    """Tests for free API key registration helper functions.

    Note: These tests require the aish_freekey binary or Python package.
    They will be skipped if neither is available.
    """

    def test_extract_free_key_info_from_data_payload(self):
        """Test extracting API key and base from a successful response."""
        from aish.wizard.setup_wizard import extract_free_key_info

        payload = {
            "status": "success",
            "apikey": "  test-key  ",
            "api_base": " https://example.com/v1 ",
        }

        api_key, api_base, model = extract_free_key_info(payload)

        assert api_key == "test-key"
        assert api_base == "https://example.com/v1"

    def test_extract_free_key_info_from_fixed_payload(self):
        """Test extracting API key when only apikey is present."""
        from aish.wizard.setup_wizard import extract_free_key_info

        payload = {
            "status": "success",
            "apikey": "k-123",
        }

        api_key, api_base, model = extract_free_key_info(payload)

        assert api_key == "k-123"
        assert api_base is None

    def test_extract_free_key_info_from_non_fixed_payload(self):
        """Test that api_key field (different from apikey) is also accepted."""
        from aish.wizard.setup_wizard import extract_free_key_info

        payload = {
            "status": "success",
            "api_key": "legacy-field",
        }

        api_key, api_base, model = extract_free_key_info(payload)

        # The implementation supports both 'apikey' and 'api_key'
        assert api_key == "legacy-field"
        assert api_base is None

    def test_extract_free_key_info_empty_apikey(self):
        """Test that empty apikey returns None."""
        from aish.wizard.setup_wizard import extract_free_key_info

        payload = {
            "status": "success",
            "apikey": "   ",
        }

        api_key, api_base, model = extract_free_key_info(payload)

        assert api_key is None
        assert api_base is None

    def test_request_free_api_key_returns_stub(self):
        """Test request_free_api_key returns stub message (Go binary handles actual requests)."""
        import aish.wizard.setup_wizard as setup_module

        # Force binary mode
        setup_module._HAS_FREEKEY_PYTHON_PACKAGE = False
        setup_module._FREEKEY_BINARY_PATH = "/fake/path"

        try:
            result = setup_module.request_free_api_key("fingerprint")
            assert result["status"] == "error"
            assert "Use register_free_key_with_retry" in result["message"]
        finally:
            # Restore - try to import package again
            try:
                from aish_freekey import request_free_api_key as _pkg_func  # noqa: F401
                setup_module._HAS_FREEKEY_PYTHON_PACKAGE = True
            except ImportError:
                pass

    def test_register_free_key_with_retry_success(self, monkeypatch):
        """Test successful free key registration in binary mode."""
        import aish.wizard.setup_wizard as setup_module

        # Force binary mode
        setup_module._HAS_FREEKEY_PYTHON_PACKAGE = False
        setup_module._FREEKEY_BINARY_PATH = "/fake/path/aish_freekey_bin"

        # Mock the binary JSON response
        def mock_run_binary_json(binary_path, cmd, *args):
            return {
                "success": True,
                "api_key": "free-key",
                "api_base": "https://free.example.com/v1",
                "model": "test-model",
            }

        monkeypatch.setattr(setup_module, "_run_binary_json", mock_run_binary_json)

        try:
            result = setup_module.register_free_key_with_retry()
            assert result.success is True
            assert result.api_key == "free-key"
            assert result.api_base == "https://free.example.com/v1"
        finally:
            # Restore
            try:
                from aish_freekey import register_free_key_with_retry as _pkg_func  # noqa: F401
                setup_module._HAS_FREEKEY_PYTHON_PACKAGE = True
            except ImportError:
                pass

    def test_register_free_key_with_retry_default_api_base(self, monkeypatch):
        """Test free key registration uses default API base when not returned."""
        import aish.wizard.setup_wizard as setup_module

        # Force binary mode
        setup_module._HAS_FREEKEY_PYTHON_PACKAGE = False
        setup_module._FREEKEY_BINARY_PATH = "/fake/path/aish_freekey_bin"

        # Mock the binary JSON response (no api_base returned)
        def mock_run_binary_json(binary_path, cmd, *args):
            return {
                "success": True,
                "api_key": "free-key",
                "api_base": "",
                "model": "",
            }

        monkeypatch.setattr(setup_module, "_run_binary_json", mock_run_binary_json)

        try:
            result = setup_module.register_free_key_with_retry(location="cn")
            assert result.success is True
            assert result.api_key == "free-key"
        finally:
            # Restore
            try:
                from aish_freekey import register_free_key_with_retry as _pkg_func  # noqa: F401
                setup_module._HAS_FREEKEY_PYTHON_PACKAGE = True
            except ImportError:
                pass

    def test_register_free_key_with_retry_failure(self, monkeypatch):
        """Test registration failure in binary mode."""
        import aish.wizard.setup_wizard as setup_module

        # Force binary mode
        setup_module._HAS_FREEKEY_PYTHON_PACKAGE = False
        setup_module._FREEKEY_BINARY_PATH = "/fake/path/aish_freekey_bin"

        # Mock the binary JSON response for failure
        def mock_run_binary_json(binary_path, cmd, *args):
            return {
                "success": False,
                "error_message": "Registration failed",
            }

        monkeypatch.setattr(setup_module, "_run_binary_json", mock_run_binary_json)

        try:
            result = setup_module.register_free_key_with_retry()
            assert result.success is False
            assert result.error_message == "Registration failed"
        finally:
            # Restore
            try:
                from aish_freekey import register_free_key_with_retry as _pkg_func  # noqa: F401
                setup_module._HAS_FREEKEY_PYTHON_PACKAGE = True
            except ImportError:
                pass

    def test_register_free_key_with_retry_empty_response(self, monkeypatch):
        """Test registration with empty response in binary mode."""
        import aish.wizard.setup_wizard as setup_module

        # Force binary mode
        setup_module._HAS_FREEKEY_PYTHON_PACKAGE = False
        setup_module._FREEKEY_BINARY_PATH = "/fake/path/aish_freekey_bin"

        # Mock empty response from binary
        def mock_run_binary_json(binary_path, cmd, *args):
            return {}

        monkeypatch.setattr(setup_module, "_run_binary_json", mock_run_binary_json)

        try:
            result = setup_module.register_free_key_with_retry()
            assert result.success is False
            assert "Failed to communicate" in result.error_message
        finally:
            # Restore
            try:
                from aish_freekey import register_free_key_with_retry as _pkg_func  # noqa: F401
                setup_module._HAS_FREEKEY_PYTHON_PACKAGE = True
            except ImportError:
                pass
