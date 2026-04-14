"""Environment Manager for AI Shell - Manages environment variables."""

import os
import subprocess
from typing import Any, Dict, Optional


class EnvironmentManager:
    """Manage AI Shell internal environment variables"""

    def __init__(self) -> None:
        # Internal environment variable storage
        self._env_vars = {}
        # Exported variable set (used to mark which variables need to be exported to child processes)
        self._exported_vars = set()
        # Load system environment variables on init
        self._load_system_env()

        # Directory stack for pushd/popd commands
        # This is shared between the shell core and BashTool
        self.directory_stack: Any | None = None  # Will be set by the shell core

    def _load_system_env(self) -> None:
        """Load system environment variables into internal storage"""
        self._env_vars = os.environ.copy()

        # Add color support environment variables (if missing)
        if "TERM" not in self._env_vars:
            self._env_vars["TERM"] = "xterm-256color"

        # Force enable ls color output
        self._env_vars["CLICOLOR"] = "1"
        self._env_vars["CLICOLOR_FORCE"] = "1"

        # If system has LS_COLORS, keep it; otherwise use default value
        if "LS_COLORS" not in self._env_vars:
            # Default LS_COLORS settings to distinguish files and directories
            self._env_vars["LS_COLORS"] = (
                "di=1;34:fi=0:ln=1;36:pi=1;33:so=1;35:do=1;35:bd=1;33:cd=1;33:or=1;31:mi=1;31:ex=1;32:*.tar=1;31:*.tgz=1;31:*.arc=1;31:*.arj=1;31:*.taz=1;31:*.lha=1;31:*.lz4=1;31:*.lzh=1;31:*.lzma=1;31:*.tlz=1;31:*.txz=1;31:*.tzo=1;31:*.t7z=1;31:*.zip=1;31:*.z=1;31:*.Z=1;31:*.dz=1;31:*.gz=1;31:*.lrz=1;31:*.lz=1;31:*.lzo=1;31:*.xz=1;31:*.zst=1;31:*.tzst=1;31:*.bz2=1;31:*.bz=1;31:*.tbz=1;31:*.tbz2=1;31:*.tz=1;31:*.deb=1;31:*.rpm=1;31:*.jar=1;31:*.war=1;31:*.ear=1;31:*.sar=1;31:*.rar=1;31:*.alz=1;31:*.ace=1;31:*.zoo=1;31:*.cpio=1;31:*.7z=1;31:*.rz=1;31:*.cab=1;31:*.wim=1;31:*.swm=1;31:*.dwm=1;31:*.esd=1;31"
            )

        # Load environment from bashrc and bash_profile
        self._load_bash_env()

        # Mark all system environment variables as exported
        self._exported_vars = set(self._env_vars.keys())

    def _load_bash_env(self):
        """Load environment variables from ~/.bashrc and ~/.bash_profile"""
        home = os.path.expanduser("~")
        bash_files = []

        # Check for bash_profile (for login shells)
        bash_profile = os.path.join(home, ".bash_profile")
        if os.path.exists(bash_profile):
            bash_files.append(bash_profile)

        # Check for bashrc (for interactive shells)
        bashrc = os.path.join(home, ".bashrc")
        if os.path.exists(bashrc):
            bash_files.append(bashrc)

        if not bash_files:
            return

        # Use bash to source the files and export environment
        for bash_file in bash_files:
            try:
                # Run bash in login mode, source the file, and print all exported variables
                cmd = f'bash -lc "source {bash_file} && env -0"'
                result = subprocess.run(
                    cmd,
                    shell=True,
                    capture_output=True,
                    text=False,
                    timeout=5,
                )

                if result.returncode == 0:
                    # Parse the null-delimited output
                    env_output = result.stdout.decode("utf-8", errors="replace")
                    for line in env_output.split("\0"):
                        if "=" in line:
                            key, value = line.split("=", 1)
                            # Only add new variables, don't overwrite existing ones
                            # This preserves parent shell's environment (e.g., PATH)
                            if key not in self._env_vars:
                                self._env_vars[key] = value
                                self._exported_vars.add(key)
            except (subprocess.TimeoutExpired, FileNotFoundError, OSError):
                # Silently skip if bash is not available or times out
                pass
            except Exception:
                # Ignore any other errors during bashrc loading
                pass

    def set_var(self, key: str, value: str, export: bool = True) -> bool:
        """Set environment variable"""
        self._env_vars[key] = value
        if export:
            self._exported_vars.add(key)
        # Also update os.environ so subprocesses can see the change
        os.environ[key] = value
        return True

    def unset_var(self, key: str) -> bool:
        """Delete environment variable"""
        if key in self._env_vars:
            del self._env_vars[key]
            self._exported_vars.discard(key)
            # Also remove from os.environ
            os.environ.pop(key, None)
            return True
        return False

    def get_var(self, key: str) -> Optional[str]:
        """Get environment variable value"""
        return self._env_vars.get(key)

    def get_all_vars(self) -> Dict[str, str]:
        """Get all environment variables"""
        return self._env_vars.copy()

    def get_exported_vars(self) -> Dict[str, str]:
        """Get exported environment variables only"""
        return {k: v for k, v in self._env_vars.items() if k in self._exported_vars}

    def remove_export(self, key: str) -> bool:
        """Remove export attribute from variable"""
        if key in self._env_vars:
            self._exported_vars.discard(key)
            return True
        return False

    def is_exported(self, key: str) -> bool:
        """Check if variable is exported"""
        return key in self._exported_vars
