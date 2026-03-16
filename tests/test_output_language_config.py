"""
Test cases for output language configuration
"""

import os
import tempfile
from pathlib import Path

import pytest
import yaml

from aish.config import Config, ConfigModel
from aish.i18n import reset_i18n_for_tests
from aish.utils import get_output_language_from_locale


def test_output_language_from_config():
    """Test output language is correctly read from config"""
    for language in ["Chinese", "English", "German", "French", "Spanish", "Japanese"]:
        config_data = {"model": "test-model", "output_language": language}
        config_model = ConfigModel.model_validate(config_data)
        assert config_model.output_language == language

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
        config.set_output_language("German")
        assert config.get_output_language() == "German"

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
    original_lang = os.environ.get("LANG")
    try:
        for locale, expected in [
            ("zh_CN.UTF-8", "Chinese"),
            ("en_US.UTF-8", "English"),
            ("de_DE.UTF-8", "German"),
            ("fr_FR.UTF-8", "French"),
            ("es_ES.UTF-8", "Spanish"),
            ("ja_JP.UTF-8", "Japanese"),
            ("it_IT.UTF-8", "English"),
        ]:
            os.environ["LANG"] = locale
            reset_i18n_for_tests()
            assert get_output_language_from_locale() == expected
    finally:
        if original_lang:
            os.environ["LANG"] = original_lang
        elif "LANG" in os.environ:
            del os.environ["LANG"]
        reset_i18n_for_tests()


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
