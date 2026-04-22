"""Tests for UpdateManager."""

import httpx
import pytest
from unittest.mock import Mock, patch
from aish import __version__
from aish.cli.update_manager import UpdateCheckError, UpdateManager


@pytest.fixture
def update_manager():
    """Create UpdateManager instance for testing."""
    return UpdateManager()


@pytest.fixture
def mock_latest_version_text():
    """Mock stable latest-version response body."""
    return "0.3.0\n"


@pytest.mark.timeout(5)
def test_get_current_version(update_manager):
    """Test getting current version."""
    assert update_manager.get_current_version() == __version__


@pytest.mark.timeout(5)
def test_detect_platform(update_manager):
    """Test platform detection."""
    plat, arch = update_manager.detect_platform()
    assert plat in ("linux", "darwin")
    assert arch in ("amd64", "arm64")


@pytest.mark.timeout(5)
@patch("aish.cli.update_manager.httpx.Client")
def test_get_latest_release_success(
    mock_client_class, update_manager, mock_latest_version_text
):
    """Test successful fetching of latest release from CDN metadata."""
    mock_response = Mock()
    mock_response.raise_for_status = Mock()
    mock_response.text = mock_latest_version_text
    mock_client_instance = mock_client_class.return_value
    mock_client_instance.get = Mock(return_value=mock_response)

    with patch.object(update_manager, "client", mock_client_instance):
        result = update_manager.get_latest_release()

    assert result is not None
    assert result["tag_name"] == "v0.3.0"
    assert result["body"] == ""
    assert mock_client_instance.get.call_args[0][0].endswith("/latest")


@pytest.mark.timeout(5)
@patch("aish.cli.update_manager.httpx.Client")
def test_get_latest_release_http_error(mock_client_class, update_manager):
    """Test handling of HTTP error raises UpdateCheckError."""
    mock_client_instance = mock_client_class.return_value
    mock_client_instance.get = Mock(side_effect=httpx.HTTPError("Connection error"))

    with patch.object(update_manager, "client", mock_client_instance):
        with pytest.raises(UpdateCheckError, match="Network error"):
            update_manager.get_latest_release()


@pytest.mark.timeout(5)
@patch("aish.cli.update_manager.httpx.Client")
def test_check_for_updates_available(
    mock_client_class, update_manager, mock_latest_version_text
):
    """Test checking when update is available."""
    mock_response = Mock()
    mock_response.raise_for_status = Mock()
    mock_response.text = mock_latest_version_text
    mock_client_instance = mock_client_class.return_value
    mock_client_instance.get = Mock(return_value=mock_response)

    with patch.object(update_manager, "client", mock_client_instance):
        result = update_manager.check_for_updates()

    assert result is not None
    assert result["current_version"] == __version__
    assert result["latest_version"] == "0.3.0"


@pytest.mark.timeout(5)
@patch("aish.cli.update_manager.httpx.Client")
def test_check_for_updates_none_available(mock_client_class, update_manager):
    """Test checking when no update available."""
    mock_response = Mock()
    mock_response.raise_for_status = Mock()
    mock_response.text = "0.1.0\n"
    mock_client_instance = mock_client_class.return_value
    mock_client_instance.get = Mock(return_value=mock_response)

    with patch.object(update_manager, "client", mock_client_instance):
        result = update_manager.check_for_updates()

    assert result is None


@pytest.mark.timeout(5)
@patch("aish.cli.update_manager.httpx.Client")
def test_download_release_success(mock_client_class, update_manager, tmp_path):
    """Test successful download from CDN."""
    mock_response = Mock()
    mock_response.raise_for_status = Mock()
    mock_response.iter_bytes = Mock(return_value=[b"test data"])
    mock_response.headers = {"content-length": "9"}
    mock_cm = Mock()
    mock_cm.__enter__ = Mock(return_value=mock_response)
    mock_cm.__exit__ = Mock(return_value=False)
    mock_client_instance = mock_client_class.return_value
    mock_client_instance.stream = Mock(return_value=mock_cm)

    with patch.object(update_manager, "client", mock_client_instance):
        result = update_manager.download_release("v0.3.0", dest_dir=tmp_path)

    assert result is not None
    assert result.name == "aish-0.3.0-linux-amd64.tar.gz"
    stream_url = mock_client_instance.stream.call_args[0][1]
    assert (
        stream_url
        == "https://cdn.aishell.ai/download/releases/0.3.0/aish-0.3.0-linux-amd64.tar.gz"
    )


@pytest.mark.timeout(5)
@patch("aish.cli.update_manager.httpx.Client")
def test_download_release_respects_download_base_override(
    mock_client_class, update_manager, tmp_path, monkeypatch
):
    """Test download uses the configured CDN base URL override."""
    monkeypatch.setenv("AISH_DOWNLOAD_BASE_URL", "https://cdn.example.com/releases")

    mock_response = Mock()
    mock_response.raise_for_status = Mock()
    mock_response.iter_bytes = Mock(return_value=[b"test data"])
    mock_response.headers = {"content-length": "9"}
    mock_cm = Mock()
    mock_cm.__enter__ = Mock(return_value=mock_response)
    mock_cm.__exit__ = Mock(return_value=False)
    mock_client_instance = mock_client_class.return_value
    mock_client_instance.stream = Mock(return_value=mock_cm)

    with patch.object(update_manager, "client", mock_client_instance):
        result = update_manager.download_release("v0.3.0", dest_dir=tmp_path)

    assert result is not None
    stream_url = mock_client_instance.stream.call_args[0][1]
    assert (
        stream_url
        == "https://cdn.example.com/releases/releases/0.3.0/aish-0.3.0-linux-amd64.tar.gz"
    )


@pytest.mark.timeout(5)
@patch("aish.cli.update_manager.subprocess.run")
@patch("aish.cli.update_manager.tarfile.open")
def test_install_release_success(mock_tarfile, mock_run, update_manager, tmp_path):
    """Test successful installation."""
    extract_dir = update_manager.temp_dir / "extract"
    extract_dir.mkdir(parents=True, exist_ok=True)
    install_script_path = extract_dir / "install.sh"
    install_script_path.write_text("#!/bin/bash\necho 'install'\n")

    # Mock tarfile with no path traversal members
    mock_member = Mock()
    mock_member.name = "install.sh"
    mock_tar = Mock()
    mock_tar.getmembers = Mock(return_value=[mock_member])
    mock_tar.extractall = Mock()
    mock_tarfile.return_value.__enter__.return_value = mock_tar

    mock_result = Mock()
    mock_result.returncode = 0
    mock_run.return_value = mock_result

    archive_path = tmp_path / "test.tar.gz"
    archive_path.write_text("fake content")

    result = update_manager.install_release(archive_path)

    assert result is True


@pytest.mark.timeout(5)
@patch("aish.cli.update_manager.httpx.Client")
def test_get_latest_release_with_pre_release(mock_client_class, update_manager):
    """Test fetching pre-release uses list endpoint."""
    pre_release_response = [
        {
            "tag_name": "v0.4.0-beta",
            "name": "v0.4.0-beta",
            "body": "Beta release",
            "html_url": "https://example.com",
            "assets": [],
            "prerelease": True,
        }
    ]
    mock_response = Mock()
    mock_response.raise_for_status = Mock()
    mock_response.json = Mock(return_value=pre_release_response)
    mock_client_instance = mock_client_class.return_value
    mock_client_instance.get = Mock(return_value=mock_response)

    with patch.object(update_manager, "client", mock_client_instance):
        result = update_manager.get_latest_release(include_pre_release=True)

    assert result is not None
    assert result["tag_name"] == "v0.4.0-beta"
    # Should have called the list endpoint, not the latest endpoint
    call_args = mock_client_instance.get.call_args[0][0]
    assert "releases" in call_args
    assert "latest" not in call_args
