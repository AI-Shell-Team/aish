"""
Test cases for output language configuration
"""

import os
import tempfile
from pathlib import Path

import pytest
import yaml

from aish.config import Config, ConfigModel


def test_output_language_from_config():
    """Test output language is correctly read from config"""
    # Test with Chinese
    config_data = {"model": "test-model", "output_language": "Chinese"}

    config_model = ConfigModel.model_validate(config_data)
    assert config_model.output_language == "Chinese"

    # Test with English
    config_data = {"model": "test-model", "output_language": "English"}

    config_model = ConfigModel.model_validate(config_data)
    assert config_model.output_language == "English"

    # Test with None (should use auto-detection)
    config_data = {"model": "test-model", "output_language": None}

    config_model = ConfigModel.model_validate(config_data)
    assert config_model.output_language is None


def test_config_methods():
    """Test Config class methods for output_language"""
    with tempfile.TemporaryDirectory() as temp_dir:
        config_file = Path(temp_dir) / "test_config.yaml"

        # Create a config file
        config_data = {"model": "test-model", "output_language": "Chinese"}

        with open(config_file, "w") as f:
            yaml.safe_dump(config_data, f)

        config = Config(str(config_file))

        # Test get method
        assert config.get_output_language() == "Chinese"

        # Test set method
        config.set_output_language("English")
        assert config.get_output_language() == "English"

        # Test setting to None
        config.set_output_language(None)
        assert config.get_output_language() is None


def test_shell_output_language_logic():
    """Test AIShell output language selection logic"""

    # Mock shell class to test get_output_language method
    class MockShell:
        def get_output_language_from_locale(self) -> str:
            return "Chinese"  # Mock locale detection

        def get_output_language(self, config: ConfigModel) -> str:
            # Replicate the logic from AIShell
            if config.output_language:
                return config.output_language
            return self.get_output_language_from_locale()

    mock_shell = MockShell()

    # Test with config setting
    config_with_language = ConfigModel(model="test-model", output_language="English")
    result = mock_shell.get_output_language(config_with_language)
    assert result == "English"

    # Test with None (should use locale)
    config_without_language = ConfigModel(model="test-model", output_language=None)
    result = mock_shell.get_output_language(config_without_language)
    assert result == "Chinese"  # From mock locale detection


def test_locale_detection():
    """Test locale-based language detection"""

    class MockShell:
        def get_output_language_from_locale(self) -> str:
            locale = os.getenv("LANG", "zh_CN.UTF-8")
            lang = locale.split(".")[0]
            if lang.startswith("zh"):
                return "Chinese"
            else:
                return "English"

    mock_shell = MockShell()

    # Test with Chinese locale
    original_lang = os.environ.get("LANG")
    try:
        os.environ["LANG"] = "zh_CN.UTF-8"
        assert mock_shell.get_output_language_from_locale() == "Chinese"

        os.environ["LANG"] = "en_US.UTF-8"
        assert mock_shell.get_output_language_from_locale() == "English"

    finally:
        # Restore original locale
        if original_lang:
            os.environ["LANG"] = original_lang
        elif "LANG" in os.environ:
            del os.environ["LANG"]


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
