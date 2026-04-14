"""Tests for UninstallManager."""

from pathlib import Path

import pytest
from unittest.mock import Mock, patch
from aish.cli.uninstall_manager import UninstallManager


@pytest.fixture
def uninstall_manager():
    """Create UninstallManager instance for testing."""
    return UninstallManager()


@pytest.mark.timeout(5)
def test_get_data_directories(uninstall_manager):
    """Test getting data directories."""
    dirs = uninstall_manager.get_data_directories()
    assert "config" in dirs
    assert "data" in dirs
    assert "cache" in dirs


@pytest.mark.timeout(5)
def test_is_elf_binary(tmp_path):
    """Test ELF binary detection."""
    elf_file = tmp_path / "elf_bin"
    elf_file.write_bytes(b"\x7fELF" + b"\x00" * 100)
    assert UninstallManager._is_elf_binary(elf_file) is True

    script_file = tmp_path / "script"
    script_file.write_text("#!/bin/bash\necho hi\n")
    assert UninstallManager._is_elf_binary(script_file) is False

    nonexistent = tmp_path / "nope"
    assert UninstallManager._is_elf_binary(nonexistent) is False


@pytest.mark.timeout(5)
@patch("aish.cli.uninstall_manager._ARCHIVE_BIN_DIR")
def test_detect_installation_method_archive(mock_bin_dir, uninstall_manager):
    """Test detecting archive installation when ELF binary exists."""
    mock_aish = Mock()
    mock_aish.exists.return_value = True
    mock_bin_dir.__truediv__ = Mock(return_value=mock_aish)

    with patch.object(UninstallManager, "_is_elf_binary", return_value=True):
        method = uninstall_manager.detect_installation_method()
    assert method == "archive"


@pytest.mark.timeout(5)
@patch.object(UninstallManager, "_is_elf_binary", return_value=False)
@patch("aish.cli.uninstall_manager.sys")
@patch("aish.cli.uninstall_manager.subprocess.run")
def test_detect_installation_method_pip(
    mock_run, mock_sys, mock_elf, uninstall_manager
):
    """Test detecting pip installation when no archive binary."""
    # Simulate not in a virtual environment
    mock_sys.prefix = "/usr"
    mock_sys.base_prefix = "/usr"
    mock_result = Mock()
    mock_result.returncode = 0
    mock_run.return_value = mock_result

    method = uninstall_manager.detect_installation_method()
    assert method == "pip"


@pytest.mark.timeout(5)
@patch.object(UninstallManager, "_is_elf_binary", return_value=False)
@patch("aish.cli.uninstall_manager.sys")
@patch("aish.cli.uninstall_manager.subprocess.run")
def test_detect_installation_method_system_dpkg(
    mock_run, mock_sys, mock_elf, uninstall_manager
):
    """Test detecting system installation via dpkg."""
    mock_sys.prefix = "/usr"
    mock_sys.base_prefix = "/usr"
    pip_result = Mock()
    pip_result.returncode = 1
    dpkg_result = Mock()
    dpkg_result.returncode = 0
    mock_run.side_effect = [pip_result, dpkg_result]

    method = uninstall_manager.detect_installation_method()
    assert method == "system"


@pytest.mark.timeout(5)
@patch.object(UninstallManager, "_is_elf_binary", return_value=False)
@patch("aish.cli.uninstall_manager.sys")
@patch("aish.cli.uninstall_manager.subprocess.run")
def test_detect_installation_method_unknown(
    mock_run, mock_sys, mock_elf, uninstall_manager
):
    """Test when installation method cannot be detected."""
    mock_sys.prefix = "/usr"
    mock_sys.base_prefix = "/usr"
    mock_run.side_effect = FileNotFoundError()

    method = uninstall_manager.detect_installation_method()
    assert method == "unknown"


@pytest.mark.timeout(5)
@patch.object(UninstallManager, "_is_elf_binary", return_value=False)
@patch("aish.cli.uninstall_manager.sys")
@patch("aish.cli.uninstall_manager.subprocess.run")
def test_detect_installation_method_venv_skips_pip(
    mock_run, mock_sys, mock_elf, uninstall_manager
):
    """Test that pip detection is skipped inside a virtual environment."""
    # Simulate running inside a venv
    mock_sys.prefix = "/home/user/.venv"
    mock_sys.base_prefix = "/usr"
    # dpkg/rpm checks fail → unknown
    mock_run.side_effect = FileNotFoundError()

    method = uninstall_manager.detect_installation_method()
    # Should skip pip check (no "pip show" call), fall through to system checks
    assert method == "unknown"
    # Verify "pip show" was never called
    for call_args in mock_run.call_args_list:
        args = call_args[0][0]
        assert args[0] != "pip" or "show" not in args


@pytest.mark.timeout(5)
@patch("aish.cli.uninstall_manager.subprocess.run")
def test_uninstall_archive_with_script(mock_run, uninstall_manager):
    """Test archive uninstall using aish-uninstall script."""
    with patch("aish.cli.uninstall_manager._ARCHIVE_BIN_DIR") as mock_bin_dir:
        mock_bin_dir.__truediv__ = Mock(
            return_value=Mock(exists=Mock(return_value=True))
        )
        mock_result = Mock()
        mock_result.returncode = 0
        mock_run.return_value = mock_result

        result = uninstall_manager._uninstall_archive()
        assert result is True


@pytest.mark.timeout(5)
@patch("aish.cli.uninstall_manager.subprocess.run")
def test_uninstall_pip_success(mock_run, uninstall_manager):
    """Test successful pip uninstall."""
    mock_result = Mock()
    mock_result.returncode = 0
    mock_run.return_value = mock_result

    result = uninstall_manager.uninstall_package(method="pip")
    assert result is True


@pytest.mark.timeout(5)
@patch("aish.cli.uninstall_manager.subprocess.run")
def test_uninstall_pip_failure(mock_run, uninstall_manager):
    """Test failed pip uninstall."""
    mock_result = Mock()
    mock_result.returncode = 1
    mock_result.stderr = "some other error"
    mock_run.return_value = mock_result

    result = uninstall_manager.uninstall_package(method="pip")
    assert result is False
    # Should NOT retry --break-system-packages for non-externally-managed errors
    assert mock_run.call_count == 1


@pytest.mark.timeout(5)
@patch("aish.cli.uninstall_manager.subprocess.run")
def test_uninstall_pip_externally_managed(mock_run, uninstall_manager):
    """Test pip uninstall retries with --break-system-packages."""
    # First call: fails with externally-managed-environment
    first_result = Mock()
    first_result.returncode = 1
    first_result.stderr = "error: externally-managed-environment"
    # Second call: succeeds
    second_result = Mock()
    second_result.returncode = 0
    mock_run.side_effect = [first_result, second_result]

    result = uninstall_manager.uninstall_package(method="pip")
    assert result is True
    assert mock_run.call_count == 2


@pytest.mark.timeout(5)
@patch("aish.cli.uninstall_manager.shutil.which")
@patch("aish.cli.uninstall_manager.subprocess.run")
def test_uninstall_system_dpkg(mock_run, mock_which, uninstall_manager):
    """Test system uninstall via dpkg/apt."""
    mock_which.side_effect = lambda cmd: "/usr/bin/" + cmd if cmd == "dpkg" else None
    mock_result = Mock()
    mock_result.returncode = 0
    mock_run.return_value = mock_result

    result = uninstall_manager.uninstall_package(method="system")
    assert result is True
    mock_run.assert_called_with(
        ["sudo", "apt-get", "remove", "-y", "aish"],
        capture_output=True,
        text=True,
    )


@pytest.mark.timeout(5)
@patch("aish.cli.uninstall_manager.shutil.which")
@patch("aish.cli.uninstall_manager.subprocess.run")
def test_uninstall_system_rpm(mock_run, mock_which, uninstall_manager):
    """Test system uninstall via rpm/dnf."""
    mock_which.side_effect = lambda cmd: "/usr/bin/" + cmd if cmd == "rpm" else None
    mock_result = Mock()
    mock_result.returncode = 0
    mock_run.return_value = mock_result

    result = uninstall_manager.uninstall_package(method="system")
    assert result is True
    mock_run.assert_called_with(
        ["sudo", "dnf", "remove", "-y", "aish"],
        capture_output=True,
        text=True,
    )


@pytest.mark.timeout(5)
def test_uninstall_unknown_method(uninstall_manager):
    """Test uninstall with unknown method."""
    result = uninstall_manager.uninstall_package(method="unknown")
    assert result is False


@pytest.mark.timeout(5)
@patch("aish.cli.uninstall_manager.shutil.rmtree")
def test_purge_data_success(mock_rmtree, uninstall_manager):
    """Test successful data purge."""
    dirs = {
        "config": Path("/tmp/test-aish-config/aish"),
        "data": Path("/tmp/test-aish-data/aish"),
        "cache": Path("/tmp/test-aish-cache/aish"),
    }
    with patch.object(uninstall_manager, "get_data_directories", return_value=dirs):
        with patch.object(Path, "exists", return_value=True):
            result = uninstall_manager.purge_data()
    assert result is True
    assert mock_rmtree.call_count == 3


@pytest.mark.timeout(5)
def test_is_safe_purge_path_rejects_system_prefix_descendants():
    """System directories and their descendants must never be purged."""
    assert UninstallManager._is_safe_purge_path(Path("/etc/aish")) is False
    assert UninstallManager._is_safe_purge_path(Path("/usr/local/aish")) is False


@pytest.mark.timeout(5)
def test_is_safe_purge_path_allows_non_home_xdg_paths(tmp_path):
    """Legitimate XDG locations outside HOME (for example under /tmp) remain allowed."""
    assert UninstallManager._is_safe_purge_path(tmp_path / "xdg" / "aish") is True


@pytest.mark.timeout(5)
def test_is_safe_purge_path_rejects_symlink_to_system_path(tmp_path):
    """Symlinked XDG paths must not bypass the system path safety checks."""
    link_path = tmp_path / "xdg" / "aish"
    link_path.parent.mkdir(parents=True)
    link_path.symlink_to(Path("/etc/aish"))

    assert UninstallManager._is_safe_purge_path(link_path) is False


@pytest.mark.timeout(5)
@patch.object(UninstallManager, "_is_elf_binary", return_value=False)
@patch("aish.cli.uninstall_manager.sys")
@patch("aish.cli.uninstall_manager.subprocess.run")
def test_uninstall_package_auto_detect(mock_run, mock_sys, mock_elf, uninstall_manager):
    """Test uninstall_package auto-detects when method is None."""
    mock_sys.prefix = "/usr"
    mock_sys.base_prefix = "/usr"
    mock_result = Mock()
    mock_result.returncode = 0
    mock_run.return_value = mock_result

    result = uninstall_manager.uninstall_package()
    assert result is True
