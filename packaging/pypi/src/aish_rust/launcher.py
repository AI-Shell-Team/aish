from __future__ import annotations

import os
import stat
import sys
from pathlib import Path


def bundled_binary_path() -> Path:
    return Path(__file__).resolve().parent / "bin" / "aish"


def ensure_executable(path: Path) -> None:
    current_mode = path.stat().st_mode
    if current_mode & stat.S_IXUSR:
        return
    path.chmod(current_mode | stat.S_IXUSR)


def main() -> None:
    binary_path = bundled_binary_path()
    if not binary_path.is_file():
        raise SystemExit(f"Bundled AI Shell binary is missing: {binary_path}")

    ensure_executable(binary_path)
    exec_env = os.environ.copy()
    exec_env["AISH_INSTALL_CHANNEL"] = "pip"
    exec_env["AISH_PIP_PACKAGE_NAME"] = "aish-rust"
    exec_env["AISH_PYTHON_EXECUTABLE"] = sys.executable
    os.execve(str(binary_path), [str(binary_path), *sys.argv[1:]], exec_env)