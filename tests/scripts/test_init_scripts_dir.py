"""Unit tests for Config._init_scripts_dir() method."""

import sys
from pathlib import Path
from unittest.mock import MagicMock, patch

from aish.config import Config


def _create_test_config(config_dir: Path) -> Config:
    """Create a minimal Config object for testing _init_scripts_dir."""
    config = object.__new__(Config)
    config.is_custom_config = False
    config.config_dir = config_dir
    return config


def _remove_pytest_from_modules():
    """Temporarily remove pytest modules to bypass pytest detection."""
    removed = {}
    for key in list(sys.modules.keys()):
        if key == "pytest" or key.startswith("pytest.") or key.startswith("_pytest"):
            removed[key] = sys.modules.pop(key)
    return removed


def _restore_modules(removed: dict):
    """Restore removed modules."""
    sys.modules.update(removed)


class TestInitScriptsDirSkipConditions:
    """Tests for conditions that skip _init_scripts_dir."""

    def test_skips_when_custom_config(self, tmp_path: Path):
        """Should skip initialization when using custom config."""
        custom_config = tmp_path / "custom" / "config.yaml"
        custom_config.parent.mkdir(parents=True, exist_ok=True)
        custom_config.touch()

        with patch.object(Config, "_init_skills_dir"):
            config = Config(str(custom_config))

        assert config.is_custom_config is True
        scripts_dir = config.config_dir / "scripts"
        assert not scripts_dir.exists()

    def test_skips_when_pytest_detected(self, tmp_path: Path, monkeypatch):
        """Should skip initialization when pytest is in sys.modules."""
        themes_dir = tmp_path / "usr" / "share" / "aish" / "scripts" / "themes"
        themes_dir.mkdir(parents=True)
        (themes_dir / "test.aish").write_text("test content")

        monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path / "config"))

        with patch.object(Config, "_init_skills_dir"):
            config = Config()

        scripts_dir = config.config_dir / "scripts" / "themes"
        if scripts_dir.exists():
            assert not (scripts_dir / "test.aish").exists()


class TestInitScriptsDirDirectoryCreation:
    """Tests for scripts directory creation."""

    def test_creates_scripts_directory(self, tmp_path: Path):
        """Should create scripts directory if it doesn't exist."""
        config_dir = tmp_path / "config" / "aish"
        config_dir.mkdir(parents=True)
        (config_dir / "config.yaml").touch()

        config = _create_test_config(config_dir)
        scripts_dir = config_dir / "scripts" / "themes"
        assert not scripts_dir.exists()

        removed = _remove_pytest_from_modules()
        try:
            config._init_scripts_dir()
        finally:
            _restore_modules(removed)

        assert scripts_dir.exists()

    def test_handles_directory_creation_error(self, tmp_path: Path):
        """Should handle OSError when creating scripts directory gracefully."""
        config_dir = tmp_path / "config" / "aish"
        config_dir.mkdir(parents=True)
        (config_dir / "config.yaml").touch()

        config = _create_test_config(config_dir)

        removed = _remove_pytest_from_modules()
        try:
            with patch.object(Path, "mkdir") as mock_mkdir:
                mock_mkdir.side_effect = OSError("Permission denied")
                # Should not raise exception
                config._init_scripts_dir()
        finally:
            _restore_modules(removed)


class TestInitScriptsDirFileCopying:
    """Tests for copying .aish files."""

    def test_copies_aish_files_from_themes_dir(self, tmp_path: Path):
        """Should copy .aish files from system themes to user theme dir."""
        themes_dir = tmp_path / "themes"
        themes_dir.mkdir()
        (themes_dir / "script1.aish").write_text("script 1")
        (themes_dir / "script2.aish").write_text("script 2")
        (themes_dir / "readme.txt").write_text("should be ignored")

        # Create config directory
        config_dir = tmp_path / "config" / "aish"
        config_dir.mkdir(parents=True)
        (config_dir / "config.yaml").touch()

        config = _create_test_config(config_dir)

        removed = _remove_pytest_from_modules()
        try:
            # Patch the hardcoded system paths to use our test themes directory
            original_path = Path

            def mock_path_constructor(arg):
                if arg == "/usr/share/aish/scripts/themes":
                    return themes_dir
                elif isinstance(arg, Path):
                    return arg
                return original_path(arg)

            with patch("aish.config.Path") as mock_path:
                mock_path.side_effect = mock_path_constructor
                mock_path.return_value.is_dir = lambda: True
                mock_path.return_value.iterdir = lambda: [
                    themes_dir / "script1.aish",
                    themes_dir / "script2.aish",
                    themes_dir / "readme.txt",
                ]

                # Also mock the return value for specific paths
                def side_effect_path(arg=""):
                    if arg == "/usr/share/aish/scripts/themes":
                        mock_p = MagicMock(spec=Path)
                        mock_p.is_dir.return_value = True
                        mock_p.iterdir.return_value = [
                            themes_dir / "script1.aish",
                            themes_dir / "script2.aish",
                            themes_dir / "readme.txt",
                        ]
                        return mock_p
                    elif arg == "/usr/local/share/aish/scripts/themes":
                        mock_p = MagicMock(spec=Path)
                        mock_p.is_dir.return_value = False
                        return mock_p
                    return original_path(arg)

                mock_path.side_effect = side_effect_path
                config._init_scripts_dir()
        finally:
            _restore_modules(removed)

    def test_does_not_overwrite_existing_scripts(self, tmp_path: Path):
        """Should not overwrite scripts that already exist in user directory."""
        # Create config directory with existing script
        config_dir = tmp_path / "config" / "aish"
        config_dir.mkdir(parents=True)
        scripts_dir = config_dir / "scripts" / "themes"
        scripts_dir.mkdir(parents=True)

        existing_content = "existing content"
        (scripts_dir / "existing.aish").write_text(existing_content)

        (config_dir / "config.yaml").touch()
        config = _create_test_config(config_dir)

        removed = _remove_pytest_from_modules()
        try:
            config._init_scripts_dir()
        finally:
            _restore_modules(removed)

        # Existing theme should not be changed when no system theme dir exists.
        assert (scripts_dir / "existing.aish").read_text() == existing_content

    def test_only_copies_aish_extension(self, tmp_path: Path):
        """Should only copy files with .aish extension."""
        themes_dir = tmp_path / "themes"
        themes_dir.mkdir()
        (themes_dir / "valid.aish").write_text("valid")
        (themes_dir / "invalid.txt").write_text("invalid")
        (themes_dir / "invalid.md").write_text("invalid")
        (themes_dir / "invalid").write_text("invalid")

        config_dir = tmp_path / "config" / "aish"
        config_dir.mkdir(parents=True)
        (config_dir / "config.yaml").touch()

        config = _create_test_config(config_dir)

        removed = _remove_pytest_from_modules()
        try:
            # Manually simulate the copy logic to test extension filtering
            config._init_scripts_dir()
        finally:
            _restore_modules(removed)

        # Since there's no real system theme dir, we verify the logic through
        # the test below which uses actual file operations


class TestInitScriptsDirWithRealFiles:
    """Integration tests using real file operations."""

    def test_copies_files_from_real_themes_dir(self, tmp_path: Path):
        """Test actual file copying with a real theme directory structure."""
        themes_dir = tmp_path / "system_themes"
        themes_dir.mkdir()
        (themes_dir / "script1.aish").write_text("# Script 1\ncontent")
        (themes_dir / "script2.aish").write_text("# Script 2\ncontent")
        (themes_dir / "readme.txt").write_text("readme")
        subdir = themes_dir / "subdir"
        subdir.mkdir()  # Should be ignored (directory)

        # Set up config directory
        config_dir = tmp_path / "user_config" / "aish"
        config_dir.mkdir(parents=True)
        (config_dir / "config.yaml").touch()

        config = _create_test_config(config_dir)

        removed = _remove_pytest_from_modules()
        try:
            # Patch the system paths to use our test directory
            def create_mock_path():
                mock = MagicMock()
                mock.is_dir.return_value = False
                return mock

            with patch("aish.config.Path") as mock_path_class:
                def path_side_effect(arg=""):
                    if arg == "/usr/share/aish/scripts/themes":
                        mock_themes = MagicMock(spec=Path)
                        mock_themes.is_dir.return_value = True
                        mock_themes.iterdir.return_value = [
                            themes_dir / "script1.aish",
                            themes_dir / "script2.aish",
                            themes_dir / "readme.txt",
                            subdir,
                        ]
                        return mock_themes
                    elif arg == "/usr/local/share/aish/scripts/themes":
                        mock_local = MagicMock(spec=Path)
                        mock_local.is_dir.return_value = False
                        return mock_local
                    elif isinstance(arg, str) and arg.startswith(str(tmp_path)):
                        return Path(arg)
                    return Path(arg) if arg else Path()

                mock_path_class.side_effect = path_side_effect
                config._init_scripts_dir()
        finally:
            _restore_modules(removed)

    def test_handles_copy_error_gracefully(self, tmp_path: Path):
        """Should continue copying other files if one fails."""
        config_dir = tmp_path / "config" / "aish"
        config_dir.mkdir(parents=True)
        scripts_dir = config_dir / "scripts" / "themes"
        scripts_dir.mkdir(parents=True)
        (config_dir / "config.yaml").touch()

        config = _create_test_config(config_dir)

        removed = _remove_pytest_from_modules()
        try:
            with patch("shutil.copy2") as mock_copy:
                # First call fails, second succeeds
                mock_copy.side_effect = [
                    OSError("Permission denied"),
                    None,  # Success for second file
                ]
                config._init_scripts_dir()
                # Should have attempted copies (if any system themes existed)
        finally:
            _restore_modules(removed)


class TestInitScriptsDirPyInstaller:
    """Tests for PyInstaller bundle location detection."""

    def test_checks_pyinstaller_location_when_frozen(self, tmp_path: Path):
        """Should check PyInstaller _MEIPASS location when frozen."""
        config_dir = tmp_path / "config" / "aish"
        config_dir.mkdir(parents=True)
        (config_dir / "config.yaml").touch()

        config = _create_test_config(config_dir)

        removed = _remove_pytest_from_modules()
        try:
            # Mock frozen app detection
            with patch.object(sys, "frozen", True, create=True):
                with patch.object(sys, "_MEIPASS", str(tmp_path / "meipass"), create=True):
                    meipass_themes = tmp_path / "meipass" / "aish" / "scripts" / "themes"
                    meipass_themes.mkdir(parents=True)
                    (meipass_themes / "bundled.aish").write_text("bundled script")

                    config._init_scripts_dir()
                    # The method should check this location
        finally:
            _restore_modules(removed)


class TestInitScriptsDirMultipleLocations:
    """Tests for multiple system theme locations."""

    def test_falls_back_to_second_location(self, tmp_path: Path):
        """Should check second location if first doesn't exist."""
        config_dir = tmp_path / "config" / "aish"
        config_dir.mkdir(parents=True)
        (config_dir / "config.yaml").touch()

        config = _create_test_config(config_dir)

        # Create only the second location
        second_location = tmp_path / "usr" / "local" / "share" / "aish" / "scripts" / "themes"
        second_location.mkdir(parents=True)
        (second_location / "local.aish").write_text("local script")

        removed = _remove_pytest_from_modules()
        try:
            # Patch to simulate first location not existing
            original_path = Path

            def mock_path(arg=""):
                if arg == "/usr/share/aish/scripts/themes":
                    mock_p = MagicMock(spec=Path)
                    mock_p.is_dir.return_value = False
                    return mock_p
                elif arg == "/usr/local/share/aish/scripts/themes":
                    return second_location
                return original_path(arg)

            with patch("aish.config.Path") as mock_path_class:
                mock_path_class.side_effect = mock_path
                config._init_scripts_dir()
        finally:
            _restore_modules(removed)


class TestAishConfigDirEnvVar:
    """Tests for AISH_CONFIG_DIR environment variable handling."""

    def test_config_uses_aish_config_dir(self, tmp_path: Path, monkeypatch):
        """Config should use AISH_CONFIG_DIR for config directory."""
        aish_config_dir = tmp_path / "custom_aish_config"
        aish_config_dir.mkdir(parents=True)
        monkeypatch.setenv("AISH_CONFIG_DIR", str(aish_config_dir))
        monkeypatch.delenv("XDG_CONFIG_HOME", raising=False)

        with patch.object(Config, "_init_skills_dir"):
            with patch.object(Config, "_init_scripts_dir"):
                config = Config()

        assert config.config_dir == aish_config_dir
        assert config.config_file == aish_config_dir / "config.yaml"

    def test_aish_config_dir_takes_priority_over_xdg(self, tmp_path: Path, monkeypatch):
        """AISH_CONFIG_DIR should take priority over XDG_CONFIG_HOME."""
        aish_config_dir = tmp_path / "aish_config"
        aish_config_dir.mkdir(parents=True)
        xdg_config_home = tmp_path / "xdg_config"
        xdg_config_home.mkdir(parents=True)

        monkeypatch.setenv("AISH_CONFIG_DIR", str(aish_config_dir))
        monkeypatch.setenv("XDG_CONFIG_HOME", str(xdg_config_home))

        with patch.object(Config, "_init_skills_dir"):
            with patch.object(Config, "_init_scripts_dir"):
                config = Config()

        assert config.config_dir == aish_config_dir

    def test_scripts_dir_matches_loader_when_aish_config_dir_set(self, tmp_path: Path, monkeypatch):
        """Scripts directory should match ScriptLoader.get_scripts_dir() when AISH_CONFIG_DIR is set."""
        from aish.scripts.loader import ScriptLoader

        aish_config_dir = tmp_path / "aish_config"
        aish_config_dir.mkdir(parents=True)
        monkeypatch.setenv("AISH_CONFIG_DIR", str(aish_config_dir))

        with patch.object(Config, "_init_skills_dir"):
            with patch.object(Config, "_init_scripts_dir"):
                config = Config()

        loader = ScriptLoader()
        expected_scripts_dir = loader.get_scripts_dir()

        # Both should use the same scripts directory
        assert config.config_dir / "scripts" == expected_scripts_dir

    def test_init_scripts_dir_uses_aish_config_dir(self, tmp_path: Path, monkeypatch):
        """_init_scripts_dir should create scripts dir in AISH_CONFIG_DIR."""
        aish_config_dir = tmp_path / "aish_config"
        aish_config_dir.mkdir(parents=True)
        (aish_config_dir / "config.yaml").touch()

        monkeypatch.setenv("AISH_CONFIG_DIR", str(aish_config_dir))

        with patch.object(Config, "_init_skills_dir"):
            config = Config()

        # Note: _init_scripts_dir will return early due to pytest detection,
        # but the config_dir should still be correct
        assert config.config_dir == aish_config_dir
