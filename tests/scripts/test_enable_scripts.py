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