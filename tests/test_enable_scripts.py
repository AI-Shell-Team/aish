"""Tests for enable_scripts configuration."""
from aish.config import ConfigModel


def test_enable_scripts_default_true():
    """Test that enable_scripts defaults to True."""
    config = ConfigModel()
    assert config.enable_scripts is True


def test_enable_scripts_can_be_false():
    """Test that enable_scripts can be set to False."""
    config = ConfigModel(enable_scripts=False)
    assert config.enable_scripts is False


def test_enable_scripts_yaml_parsing():
    """Test that enable_scripts can be parsed from YAML."""
    import yaml
    yaml_content = "enable_scripts: false\n"
    data = yaml.safe_load(yaml_content)
    config = ConfigModel.model_validate(data)
    assert config.enable_scripts is False


def test_shell_legacy_prompt_when_scripts_disabled():
    """Test shell shows legacy prompt when enable_scripts=False."""
    from unittest.mock import MagicMock, patch

    with patch("aish.shell.Config") as mock_config_class:
        mock_config = MagicMock()
        mock_config.enable_scripts = False
        mock_config.prompt_style = "🚀"
        mock_config_class.return_value = mock_config

        with patch("aish.shell.LLMSession"):
            with patch("aish.shell.EnvironmentManager"):
                from aish.shell import AIShell

                # Create shell with mocked dependencies
                shell = AIShell.__new__(AIShell)
                shell.config = mock_config
                shell._tui_app = None

                # Test get_prompt returns legacy format
                # get_prompt uses os.getcwd(), so we need to patch it
                with patch("os.getcwd", return_value="/home/user/myproject"):
                    prompt = shell.get_prompt()
                    assert prompt == "🚀 myproject > "


def test_shell_no_script_attrs_when_disabled():
    """Test that script attributes are not created when enable_scripts=False."""
    from unittest.mock import MagicMock, patch

    with patch("aish.shell.Config") as mock_config_class:
        mock_config = MagicMock()
        mock_config.enable_scripts = False
        mock_config_class.return_value = mock_config

        with patch("aish.shell.LLMSession"):
            with patch("aish.shell.EnvironmentManager"):
                from aish.shell import AIShell

                # Create shell with mocked dependencies
                shell = AIShell.__new__(AIShell)
                shell.config = mock_config
                shell._tui_app = None

                # Verify script attributes are not present
                assert not hasattr(shell, "script_registry")
                assert not hasattr(shell, "script_executor")
                assert not hasattr(shell, "hook_manager")
                assert not hasattr(shell, "_script_hotreload_service")