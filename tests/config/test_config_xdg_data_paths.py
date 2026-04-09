from pathlib import Path

from aish.config import (ConfigModel, get_default_aish_data_dir,
                         get_default_session_db_path)
from aish.memory.config import MemoryConfig


def test_default_data_dir_uses_xdg_data_home(monkeypatch, tmp_path: Path):
    xdg_data_home = tmp_path / "xdg-data-home"
    monkeypatch.setenv("XDG_DATA_HOME", str(xdg_data_home))

    assert get_default_aish_data_dir() == xdg_data_home / "aish"
    assert get_default_session_db_path() == str(xdg_data_home / "aish" / "sessions.db")


def test_config_model_session_db_path_defaults_to_xdg(monkeypatch, tmp_path: Path):
    xdg_data_home = tmp_path / "xdg-data-home"
    monkeypatch.setenv("XDG_DATA_HOME", str(xdg_data_home))

    model = ConfigModel(model="test-model")
    assert model.session_db_path == str(xdg_data_home / "aish" / "sessions.db")
    assert model.memory.data_dir == str(xdg_data_home / "aish" / "memory")


def test_config_model_respects_explicit_session_db_path(monkeypatch, tmp_path: Path):
    xdg_data_home = tmp_path / "xdg-data-home"
    monkeypatch.setenv("XDG_DATA_HOME", str(xdg_data_home))

    explicit_db_path = tmp_path / "custom" / "sessions.db"
    model = ConfigModel(model="test-model", session_db_path=str(explicit_db_path))
    assert model.session_db_path == str(explicit_db_path)


def test_memory_config_data_dir_defaults_to_xdg(monkeypatch, tmp_path: Path):
    xdg_data_home = tmp_path / "xdg-data-home"
    monkeypatch.setenv("XDG_DATA_HOME", str(xdg_data_home))

    config = MemoryConfig()
    assert config.data_dir == str(xdg_data_home / "aish" / "memory")


def test_config_model_pty_output_keep_bytes_default():
    model = ConfigModel(model="test-model")
    assert model.pty_output_keep_bytes == 4096


def test_config_model_pty_output_keep_bytes_custom():
    model = ConfigModel(model="test-model", pty_output_keep_bytes=2048)
    assert model.pty_output_keep_bytes == 2048


def test_config_model_terminal_resize_mode_default():
    model = ConfigModel(model="test-model")
    assert model.terminal_resize_mode == "full"


def test_config_model_terminal_resize_mode_supported_values():
    assert (
        ConfigModel(
            model="test-model", terminal_resize_mode="pty_only"
        ).terminal_resize_mode
        == "pty_only"
    )
    assert (
        ConfigModel(model="test-model", terminal_resize_mode="off").terminal_resize_mode
        == "off"
    )


def test_config_model_terminal_resize_mode_invalid_falls_back_to_full():
    model = ConfigModel(model="test-model", terminal_resize_mode="unexpected")
    assert model.terminal_resize_mode == "full"
