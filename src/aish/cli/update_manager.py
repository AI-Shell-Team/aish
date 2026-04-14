"""Update manager for aish self-update functionality."""

from __future__ import annotations

import hashlib
import platform
import shutil
import subprocess
import tarfile
import tempfile
from pathlib import Path
from typing import Optional

import httpx
from packaging import version
from rich.console import Console
from rich.progress import (
    BarColumn,
    DownloadColumn,
    Progress,
    TextColumn,
    TimeRemainingColumn,
    TransferSpeedColumn,
)

from aish import __version__


class UpdateCheckError(Exception):
    """Raised when update check fails due to network/API errors."""


# Constants
GITHUB_API_LATEST = "https://api.github.com/repos/AI-Shell-Team/aish/releases/latest"
GITHUB_API_LIST = "https://api.github.com/repos/AI-Shell-Team/aish/releases"
GITHUB_RELEASES_BASE = "https://github.com/AI-Shell-Team/aish/releases/download"
FALLBACK_MIRROR = "https://www.aishell.ai/repo"
CONNECTION_TIMEOUT = 10  # seconds

# Version from package
CURRENT_VERSION = __version__


class UpdateManager:
    """Manages aish self-update process."""

    def __init__(self, console: Optional[Console] = None):
        """Initialize UpdateManager.

        Args:
            console: Rich console instance for output. If None, creates new one.
        """
        self.console = console or Console()
        self.client = httpx.Client(timeout=CONNECTION_TIMEOUT)
        self.temp_dir = Path(tempfile.gettempdir()) / "aish_update"

    def get_current_version(self) -> str:
        """Get current installed version.

        Returns:
            Current version string.
        """
        return CURRENT_VERSION

    def detect_platform(self) -> tuple[str, str]:
        """Detect operating system and architecture.

        Returns:
            Tuple of (platform, architecture). Platform is 'linux' or 'darwin'.
            Architecture is 'amd64' or 'arm64'.
        """
        system = platform.system().lower()
        if system == "linux":
            plat = "linux"
        elif system == "darwin":
            plat = "darwin"
        else:
            raise ValueError(f"Unsupported platform: {system}")

        machine = platform.machine().lower()
        if machine in ("x86_64", "amd64"):
            arch = "amd64"
        elif machine in ("aarch64", "arm64"):
            arch = "arm64"
        else:
            raise ValueError(f"Unsupported architecture: {machine}")

        return plat, arch

    def get_latest_release(self, include_pre_release: bool = False) -> Optional[dict]:
        """Get latest release information from GitHub API.

        Args:
            include_pre_release: Whether to include pre-releases.

        Returns:
            Dictionary with release info or None if failed. Keys:
            - tag_name: Version tag (e.g., "v0.2.0")
            - name: Release name
            - body: Release notes (markdown)
            - html_url: URL to release page
            - assets: List of asset dictionaries
        """
        try:
            if include_pre_release:
                # /releases/latest excludes pre-releases, use list endpoint instead
                response = self.client.get(GITHUB_API_LIST)
                response.raise_for_status()
                releases = response.json()
                if not releases:
                    return None
                data = releases[0]
            else:
                response = self.client.get(GITHUB_API_LATEST)
                response.raise_for_status()
                data = response.json()

            tag_name = data.get("tag_name")
            if not tag_name:
                return None

            return {
                "tag_name": tag_name,
                "name": data.get("name"),
                "body": data.get("body"),
                "html_url": data.get("html_url"),
                "assets": data.get("assets", []),
            }
        except httpx.HTTPError as e:
            raise UpdateCheckError(f"Network error while checking for updates: {e}") from e
        except Exception as e:
            raise UpdateCheckError(f"Unexpected error while checking for updates: {e}") from e

    def check_for_updates(self, include_pre_release: bool = False) -> Optional[dict]:
        """Check if there's a newer version available.

        Args:
            include_pre_release: Whether to include pre-releases.

        Returns:
            Dictionary with update info if update available, None otherwise.
            Keys:
            - current_version: Current installed version
            - latest_version: Latest available version
            - tag_name: Version tag (e.g., "v0.3.0")
            - release_notes: Release notes (markdown)
            - html_url: URL to release page
        """
        current = self.get_current_version()
        # get_latest_release() raises UpdateCheckError on network/API failures;
        # None is only returned for legitimate "no data" cases (empty list, missing tag)
        release_info = self.get_latest_release(include_pre_release)

        if release_info is None:
            return None

        latest_tag = release_info["tag_name"]
        latest_version_str = latest_tag.lstrip("v")
        latest_ver = version.parse(latest_version_str)
        current_ver = version.parse(current)

        if latest_ver > current_ver:
            return {
                "current_version": current,
                "latest_version": latest_version_str,
                "tag_name": latest_tag,
                "release_notes": release_info.get("body", ""),
                "html_url": release_info.get("html_url", ""),
            }

        return None

    def _download_with_progress(self, url: str, dest_path: Path, label: str) -> bool:
        """Download file from url to dest_path with a progress bar.

        Args:
            url: Download URL.
            dest_path: Destination file path.
            label: Label shown on the progress bar.

        Returns:
            True if download succeeded, False otherwise.
        """
        with self.client.stream("GET", url) as response:
            response.raise_for_status()
            total = int(response.headers.get("content-length", 0))

            progress = Progress(
                TextColumn("[bold blue]{task.description}"),
                BarColumn(),
                DownloadColumn(),
                TransferSpeedColumn(),
                TimeRemainingColumn(),
                console=self.console,
            )
            with progress, open(dest_path, "wb") as f:
                task = progress.add_task(label, total=total or None)
                for chunk in response.iter_bytes(chunk_size=8192):
                    f.write(chunk)
                    progress.update(task, advance=len(chunk))

        return True

    def download_release(
        self, tag_name: str, dest_dir: Optional[Path] = None
    ) -> Optional[Path]:
        """Download release archive for current platform.

        Args:
            tag_name: Version tag (e.g., "v0.3.0")
            dest_dir: Destination directory. Uses temp dir if None.

        Returns:
            Path to downloaded archive or None if failed.
        """
        if dest_dir is None:
            dest_dir = self.temp_dir

        dest_dir.mkdir(parents=True, exist_ok=True)

        plat, arch = self.detect_platform()
        version_str = tag_name.lstrip("v")
        filename = f"aish-{version_str}-{plat}-{arch}.tar.gz"
        dest_path = dest_dir / filename

        # Try GitHub first
        github_url = f"{GITHUB_RELEASES_BASE}/{tag_name}/{filename}"

        try:
            self._download_with_progress(github_url, dest_path, filename)
            self.console.print(f"[green]Downloaded: {dest_path}[/green]")
            return dest_path

        except httpx.HTTPError:
            self.console.print(
                "[yellow]GitHub download failed, trying mirror...[/yellow]"
            )
            mirror_url = f"{FALLBACK_MIRROR}/{tag_name}/{filename}"

            try:
                self._download_with_progress(
                    mirror_url, dest_path, f"{filename} (mirror)"
                )
                self.console.print(f"[green]Downloaded: {dest_path}[/green]")
                return dest_path

            except httpx.HTTPError as e:
                self.console.print(f"[red]Download failed from mirror: {e}[/red]")
                return None

        except Exception as e:
            self.console.print(f"[red]Unexpected error during download: {e}[/red]")
            return None

    def install_release(self, archive_path: Path) -> bool:
        """Install release from downloaded archive.

        Args:
            archive_path: Path to downloaded tar.gz archive.

        Returns:
            True if installation successful, False otherwise.
        """
        extract_dir = self.temp_dir / "extract"
        extract_dir.mkdir(parents=True, exist_ok=True)

        try:
            # Extract archive with path traversal protection
            self.console.print("[bold cyan]Extracting archive...[/bold cyan]")
            with tarfile.open(archive_path, "r:gz") as tar:
                # Python 3.12+ supports filter="data" for safe extraction
                # (blocks path traversal, absolute paths, unsafe symlinks)
                try:
                    tar.extractall(extract_dir, filter="data")
                except TypeError:
                    # Fallback for Python < 3.12: validate members manually
                    safe_members = []
                    for member in tar.getmembers():
                        member_path = (extract_dir / member.name).resolve()
                        if not member_path.is_relative_to(extract_dir.resolve()):
                            self.console.print(
                                f"[red]Security: path traversal detected: {member.name}[/red]"
                            )
                            return False
                        # Also validate symlink targets
                        if member.issym():
                            link_target = (
                                extract_dir / member.name
                            ).parent / member.linkname
                            if not link_target.resolve().is_relative_to(
                                extract_dir.resolve()
                            ):
                                self.console.print(
                                    f"[red]Security: symlink target escapes extract dir: {member.name} -> {member.linkname}[/red]"
                                )
                                return False
                        safe_members.append(member)
                    tar.extractall(extract_dir, members=safe_members)

            # Find install.sh
            install_scripts = list(extract_dir.rglob("install.sh"))
            if not install_scripts:
                self.console.print("[red]install.sh not found in archive[/red]")
                return False

            install_script = install_scripts[0]

            # Verify script hash and show content before executing with sudo
            script_hash = hashlib.sha256(install_script.read_bytes()).hexdigest()
            self.console.print(f"[dim]install.sh SHA256: {script_hash}[/dim]")

            # Run install script
            self.console.print("[bold cyan]Running install script...[/bold cyan]")
            result = subprocess.run(
                ["sudo", str(install_script)], capture_output=True, text=True
            )

            if result.returncode != 0:
                self.console.print("[red]Installation failed:[/red]")
                self.console.print(result.stderr)
                return False

            self.console.print("[green]Installation successful[/green]")
            return True

        except Exception as e:
            self.console.print(f"[red]Installation error: {e}[/red]")
            return False
        finally:
            # Cleanup extracted files
            if extract_dir.exists():
                shutil.rmtree(extract_dir)
            # Cleanup downloaded archive
            try:
                archive_path.unlink(missing_ok=True)
            except OSError:
                pass

    def close(self) -> None:
        """Release resources."""
        self.client.close()

    def __enter__(self) -> "UpdateManager":
        return self

    def __exit__(self, exc_type, exc_val, exc_tb) -> None:
        self.close()
