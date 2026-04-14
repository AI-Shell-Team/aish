"""Uninstall manager for aish."""

import os
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Literal

from rich.console import Console

# Paths used by the archive/script installer (install.sh)
_ARCHIVE_BIN_DIR = Path("/usr/local/bin")
_ARCHIVE_BINARY_NAMES = ("aish", "aish-sandbox", "aish-uninstall")
_ARCHIVE_SHARE_DIR = Path("/usr/local/share/aish")


class UninstallManager:
    """Manages aish uninstallation process."""

    def __init__(self, console: Console | None = None):
        """Initialize UninstallManager.

        Args:
            console: Rich console instance for output.
        """
        self.console = console or Console()

    def detect_installation_method(
        self,
    ) -> Literal["archive", "pip", "system", "unknown"]:
        """Detect how aish was installed.

        Returns:
            Installation method: "archive", "pip", "system", or "unknown".
        """
        # Check archive/script installation first (highest priority)
        # Archive installs place ELF binaries in /usr/local/bin/
        aish_bin = _ARCHIVE_BIN_DIR / "aish"
        if aish_bin.exists() and self._is_elf_binary(aish_bin):
            return "archive"

        # Check if installed via pip (skip if running inside a venv)
        if sys.prefix == sys.base_prefix:
            try:
                result = subprocess.run(
                    ["pip", "show", "aish"], capture_output=True, text=True
                )
                if result.returncode == 0:
                    return "pip"
            except FileNotFoundError:
                pass

        # Check if installed via system package manager
        try:
            result = subprocess.run(
                ["dpkg", "-s", "aish"], capture_output=True, text=True
            )
            if result.returncode == 0:
                return "system"
        except FileNotFoundError:
            pass

        try:
            result = subprocess.run(
                ["rpm", "-q", "aish"], capture_output=True, text=True
            )
            if result.returncode == 0:
                return "system"
        except FileNotFoundError:
            pass

        return "unknown"

    @staticmethod
    def _is_elf_binary(path: Path) -> bool:
        """Check if a file is an ELF binary (not a Python script or wrapper)."""
        try:
            magic = path.read_bytes()[:4]
            return magic == b"\x7fELF"
        except OSError:
            return False

    def uninstall_package(
        self, method: str | None = None, purge: bool = False
    ) -> bool:
        """Uninstall aish package.

        Args:
            method: Pre-detected installation method. If None, auto-detect.
            purge: If True, also remove system-level config files.

        Returns:
            True if successful, False otherwise.
        """
        if method is None:
            method = self.detect_installation_method()

        if method == "archive":
            return self._uninstall_archive(purge=purge)
        elif method == "pip":
            return self._uninstall_pip()
        elif method == "system":
            return self._uninstall_system(purge=purge)
        else:
            self.console.print("[yellow]Could not detect installation method[/yellow]")
            self.console.print("[dim]Please uninstall manually[/dim]")
            return False

    def _uninstall_archive(self, purge: bool = False) -> bool:
        """Uninstall archive/script installation.

        Uses the bundled aish-uninstall script if available,
        otherwise removes files manually.

        Args:
            purge: If True, pass --purge-config to remove system-level config.
        """
        uninstall_script = _ARCHIVE_BIN_DIR / "aish-uninstall"
        if uninstall_script.exists():
            try:
                cmd = ["sudo", str(uninstall_script)]
                if purge:
                    cmd.append("--purge-config")
                result = subprocess.run(
                    cmd,
                    capture_output=True,
                    text=True,
                )
                return result.returncode == 0
            except Exception as e:
                self.console.print(f"[red]Uninstall failed: {e}[/red]")
                return False

        # Fallback: remove files manually
        try:
            success = True
            for name in _ARCHIVE_BINARY_NAMES:
                binary = _ARCHIVE_BIN_DIR / name
                if binary.exists():
                    r = subprocess.run(
                        ["sudo", "rm", "-f", str(binary)],
                        capture_output=True,
                        text=True,
                    )
                    if r.returncode != 0:
                        self.console.print(
                            f"[red]Failed to remove {binary}: {r.stderr.strip()}[/red]"
                        )
                        success = False
            if _ARCHIVE_SHARE_DIR.exists():
                r = subprocess.run(
                    ["sudo", "rm", "-rf", str(_ARCHIVE_SHARE_DIR)],
                    capture_output=True,
                    text=True,
                )
                if r.returncode != 0:
                    self.console.print(
                        f"[red]Failed to remove {_ARCHIVE_SHARE_DIR}: {r.stderr.strip()}[/red]"
                    )
                    success = False

            # Remove system-level config when purge is requested,
            # matching the behaviour of aish-uninstall --purge-config.
            if purge:
                etc_aish = Path("/etc/aish")
                policy_file = etc_aish / "security_policy.yaml"
                if policy_file.exists():
                    r = subprocess.run(
                        ["sudo", "rm", "-f", str(policy_file)],
                        capture_output=True,
                        text=True,
                    )
                    if r.returncode != 0:
                        self.console.print(
                            f"[red]Failed to remove {policy_file}: {r.stderr.strip()}[/red]"
                        )
                        success = False
                if etc_aish.exists():
                    subprocess.run(
                        [
                            "sudo",
                            "rmdir",
                            "--ignore-fail-on-non-empty",
                            str(etc_aish),
                        ],
                        capture_output=True,
                        text=True,
                    )

            return success
        except Exception as e:
            self.console.print(f"[red]Uninstall failed: {e}[/red]")
            return False

    def _uninstall_pip(self) -> bool:
        """Uninstall via pip."""
        try:
            result = subprocess.run(
                ["pip", "uninstall", "-y", "aish"],
                capture_output=True,
                text=True,
            )
            if result.returncode == 0:
                return True
            # Retry with --break-system-packages if externally-managed
            if "externally-managed-environment" in result.stderr:
                result = subprocess.run(
                    ["pip", "uninstall", "-y", "--break-system-packages", "aish"],
                    capture_output=True,
                    text=True,
                )
                return result.returncode == 0
            return False
        except Exception as e:
            self.console.print(f"[red]Uninstall failed: {e}[/red]")
            return False

    def _uninstall_system(self, purge: bool = False) -> bool:
        """Uninstall via system package manager.

        Args:
            purge: If True, also remove system config files after removal.
        """
        has_dpkg = shutil.which("dpkg") is not None
        has_rpm = shutil.which("rpm") is not None

        if has_dpkg:
            try:
                result = subprocess.run(
                    ["sudo", "apt-get", "remove", "-y", "aish"],
                    capture_output=True,
                    text=True,
                )
                if result.returncode == 0 and purge:
                    self._purge_system_config()
                return result.returncode == 0
            except FileNotFoundError:
                pass

        if has_rpm:
            try:
                result = subprocess.run(
                    ["sudo", "dnf", "remove", "-y", "aish"],
                    capture_output=True,
                    text=True,
                )
                if result.returncode == 0 and purge:
                    self._purge_system_config()
                return result.returncode == 0
            except FileNotFoundError:
                pass

        return False

    def _purge_system_config(self) -> None:
        """Remove system-level config files (e.g. /etc/aish/security_policy.yaml)."""
        etc_aish = Path("/etc/aish")
        if etc_aish.exists():
            try:
                subprocess.run(
                    ["sudo", "rm", "-rf", str(etc_aish)],
                    capture_output=True,
                    text=True,
                    check=True,
                )
            except subprocess.CalledProcessError:
                pass

    def get_data_directories(self) -> dict[str, Path]:
        """Get paths to aish data directories.

        Returns:
            Dictionary with directory paths.
        """
        # Use XDG standard paths (consistent with config.py)
        xdg_config_home = Path(
            os.environ.get("XDG_CONFIG_HOME", "~/.config")
        ).expanduser()
        xdg_data_home = Path(
            os.environ.get("XDG_DATA_HOME", "~/.local/share")
        ).expanduser()
        xdg_cache_home = Path(os.environ.get("XDG_CACHE_HOME", "~/.cache")).expanduser()

        return {
            "config": xdg_config_home / "aish",
            "data": xdg_data_home / "aish",
            "cache": xdg_cache_home / "aish",
        }

    @staticmethod
    def _is_safe_purge_path(path: Path) -> bool:
        """Check if a path is safe to recursively delete.

        Rejects paths that resolve to system-critical directories or their
        descendants. Allows user-scoped directories outside $HOME, such as
        XDG paths under /tmp.
        """
        critical_prefixes = (
            Path("/etc"),
            Path("/usr"),
            Path("/boot"),
            Path("/dev"),
            Path("/proc"),
            Path("/sys"),
            Path("/lib"),
            Path("/lib64"),
            Path("/bin"),
            Path("/sbin"),
            Path("/opt"),
        )
        try:
            lexical = path.expanduser()
            if not lexical.is_absolute():
                return False

            for candidate in (lexical, lexical.resolve()):
                if candidate == Path("/"):
                    return False
                if candidate.name != "aish":
                    return False
                for prefix in critical_prefixes:
                    if candidate == prefix or prefix in candidate.parents:
                        return False

            # Require at least 2 path components (e.g. /tmp/aish, /home/user/.config)
            parts = [p for p in lexical.parts if p != "/"]
            if len(parts) < 2:
                return False
            return True
        except (OSError, ValueError):
            return False

    def purge_data(self) -> bool:
        """Remove all aish configuration and data.

        Returns:
            True if successful, False otherwise.
        """
        dirs = self.get_data_directories()
        success = True

        for name, path in dirs.items():
            if path.exists():
                if not self._is_safe_purge_path(path):
                    self.console.print(
                        f"[red]Refusing to delete {path}: unsafe path[/red]"
                    )
                    success = False
                    continue
                try:
                    shutil.rmtree(path)
                    self.console.print(f"[green]Removed {name}: {path}[/green]")
                except Exception as e:
                    self.console.print(f"[red]Failed to remove {path}: {e}[/red]")
                    success = False

        return success
