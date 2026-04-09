import json
import os
import subprocess
from pathlib import Path

from .config import ConfigModel

# 环境信息缓存文件路径
ENV_CACHE_FILE = Path.home() / ".config" / "aish" / "env_cache.json"


def _is_wildcard_pattern(pattern: str) -> bool:
    """
    Check if a string is a wildcard pattern that should be expanded by shell.

    This function identifies patterns that contain shell wildcards (*, ?, [], {})
    but don't contain other shell special characters that would require quoting.

    Args:
        pattern: The string to check

    Returns:
        True if the string should be treated as a wildcard pattern (not quoted)
        False if the string should be quoted
    """
    import re

    def has_unescaped_wildcard(s: str) -> bool:
        """Check if string contains unescaped wildcard characters."""
        i = 0
        while i < len(s):
            if s[i] == "\\":
                i += 2
            elif s[i] in "*?[":
                return True
            else:
                i += 1
        return False

    has_wildcards = has_unescaped_wildcard(pattern)
    has_brace_expansion = bool(re.search(r"(?<!\\\\)\{[^{}]*(?<!\\\\)\}", pattern))

    if not has_wildcards and not has_brace_expansion:
        return False

    dangerous_chars = set("\"'$`\\()&|;<>")
    has_dangerous_chars = any(c in pattern for c in dangerous_chars)

    return (has_wildcards or has_brace_expansion) and not has_dangerous_chars


def get_output_language(config: ConfigModel) -> str:
    """Get the output language from config or locale"""
    if config.output_language:
        return config.output_language

    return get_output_language_from_locale()


def get_output_language_from_locale() -> str:
    """Get the output language from the locale"""
    locale = os.getenv("LANG", "zh_CN.UTF-8")
    lang = locale.split(".")[0]
    if lang.startswith("zh"):
        return "Chinese"
    else:
        return "English"


def get_system_info(command: str) -> str:
    """Execute a command and return its output, handling errors."""
    try:
        result = subprocess.run(
            command,
            shell=True,
            check=True,
            capture_output=True,
            text=True,
            timeout=2,
        )
        return result.stdout.strip()
    except (
        subprocess.CalledProcessError,
        subprocess.TimeoutExpired,
        FileNotFoundError,
    ) as e:
        if "cat /etc/issue" in command:
            return ""
        print(f"Failed to get system info with '{command}': {e}")
        return "N/A"


def get_basic_env_info() -> str:
    """Get basic environment information including package manager, user identity, and dependencies."""
    info_parts = []
    package_info = []

    apt_version = get_system_info("apt --version 2>/dev/null | head -1")
    if apt_version:
        package_info.append(f"APT: {apt_version}")

    dnf_version = get_system_info("dnf --version 2>/dev/null | head -1")
    if dnf_version:
        package_info.append(f"DNF: {dnf_version}")

    yum_version = get_system_info("yum --version 2>/dev/null | head -1")
    if yum_version and not dnf_version:
        package_info.append(f"YUM: {yum_version}")

    pacman_version = get_system_info("pacman --version 2>/dev/null | head -1")
    if pacman_version:
        package_info.append(f"Pacman: {pacman_version}")

    zypper_version = get_system_info("zypper --version 2>/dev/null | head -1")
    if zypper_version:
        package_info.append(f"Zypper: {zypper_version}")

    if package_info:
        info_parts.append("Package Managers:")
        for pkg in package_info:
            info_parts.append(f"  {pkg}")

    user = os.getenv("USER", "unknown")
    uid = os.getenv("UID", str(os.getuid()))
    groups = "unknown"
    try:
        groups_result = get_system_info("groups")
        if groups_result and groups_result != "N/A":
            groups = groups_result
    except Exception:
        pass

    info_parts.append(f"User Identity: USER={user}, UID={uid}, GROUPS={groups}")

    sudo_user = os.getenv("SUDO_USER")
    if sudo_user:
        sudo_uid = os.getenv("SUDO_UID")
        sudo_gid = os.getenv("SUDO_GID")
        info_parts.append(
            f"Sudo Origin: SUDO_USER={sudo_user}, SUDO_UID={sudo_uid}, SUDO_GID={sudo_gid}"
        )

    ld_library_path = os.getenv("LD_LIBRARY_PATH", "")
    if ld_library_path:
        info_parts.append(f"Library Path: LD_LIBRARY_PATH={ld_library_path}")
    else:
        info_parts.append(
            "Library Path: LD_LIBRARY_PATH=(not set, using system defaults)"
        )

    return "\n".join(info_parts)


def get_current_env_info() -> str:
    """Get current environment information including locale and working directory."""
    info_parts = []

    lang = os.getenv("LANG", "not set")
    lc_all = os.getenv("LC_ALL", "not set")

    info_parts.append(f"System Language: LANG={lang}, LC_ALL={lc_all}")

    pwd = os.getenv("PWD", os.getcwd())
    info_parts.append(f"Current Directory (PWD): {pwd}")

    return "\n".join(info_parts)


def load_static_env_cache() -> dict | None:
    """加载静态环境信息缓存."""
    if not ENV_CACHE_FILE.exists():
        return None

    try:
        with open(ENV_CACHE_FILE, "r", encoding="utf-8") as f:
            cache = json.load(f)
        if all(k in cache for k in ("uname_info", "os_info", "basic_env_info")):
            return cache
    except (json.JSONDecodeError, IOError):
        pass
    return None


def save_static_env_cache(uname_info: str, os_info: str, basic_env_info: str) -> None:
    """保存静态环境信息到缓存文件."""
    ENV_CACHE_FILE.parent.mkdir(parents=True, exist_ok=True)

    cache = {
        "uname_info": uname_info,
        "os_info": os_info,
        "basic_env_info": basic_env_info,
    }

    with open(ENV_CACHE_FILE, "w", encoding="utf-8") as f:
        json.dump(cache, f, ensure_ascii=False, indent=2)


def get_or_fetch_static_env_info() -> tuple[str, str, str]:
    """获取静态环境信息，优先从缓存读取."""
    cache = load_static_env_cache()
    if cache:
        return (
            cache["uname_info"],
            cache["os_info"],
            cache["basic_env_info"],
        )

    uname_info = get_system_info("uname -a")
    os_info = get_system_info("cat /etc/issue 2>/dev/null") or "N/A"
    basic_env_info = get_basic_env_info()

    save_static_env_cache(uname_info, os_info, basic_env_info)

    return uname_info, os_info, basic_env_info


def _check_if_part_was_quoted(original_cmd: str, part: str) -> bool:
    """
    Check if a part was originally quoted in the command.

    This checks if the part appeared in quotes in the original command,
    which means the user wanted literal interpretation.

    Args:
        original_cmd: The original command string
        part: The parsed part to check

    Returns:
        True if the part was quoted in the original command
    """
    import re

    for quote in ['"', "'"]:
        escaped_part = re.escape(part)
        pattern = f"{quote}{escaped_part}{quote}"
        if re.search(pattern, original_cmd):
            return True

    return False


def escape_command_with_paths(command: str) -> str:
    """
    Return the command as-is since bash handles all escaping correctly.
    """
    return command