import json
import os
import shlex
import subprocess
from pathlib import Path

from aish.config import ConfigModel

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

    # Check for shell wildcards, but exclude escaped wildcards (backslash before them)
    # We need to check if wildcards are preceded by backslash (escaped)
    def has_unescaped_wildcard(s: str) -> bool:
        """Check if string contains unescaped wildcard characters."""
        # Remove escaped characters (backslash + char) to check remaining wildcards
        # But careful: \\ should become \, then \[ should become [ (no longer escaped)
        i = 0
        while i < len(s):
            if s[i] == "\\":
                # Skip the escaped character
                i += 2
            elif s[i] in "*?[":
                return True
            else:
                i += 1
        return False

    has_wildcards = has_unescaped_wildcard(pattern)

    # Check for brace expansion patterns: {a,b,c} or {start..end}
    # Examples: file.{txt,log}, test{1..5}, image{,.png}
    has_brace_expansion = bool(re.search(r"(?<!\\\\)\{[^{}]*(?<!\\\\)\}", pattern))

    if not has_wildcards and not has_brace_expansion:
        return False

    # Check for other shell special characters that would require quoting
    # Note: {} is excluded here since brace expansion is allowed
    dangerous_chars = set("\"'$`\\()&|;<>")
    has_dangerous_chars = any(c in pattern for c in dangerous_chars)

    # If it has wildcards/brace expansion but no dangerous characters, treat as wildcard pattern
    return (has_wildcards or has_brace_expansion) and not has_dangerous_chars


def get_output_language(config: ConfigModel) -> str:
    """Get the output language from config or locale"""
    # First, check if output_language is set in config
    if config.output_language:
        return config.output_language

    # If not set in config, auto-detect from locale
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
        # For /etc/issue, it's common for it to not exist.
        if "cat /etc/issue" in command:
            return ""
        print(f"Failed to get system info with '{command}': {e}")
        return "N/A"


def get_basic_env_info() -> str:
    """Get basic environment information including package manager, user identity, and dependencies.

    Returns:
        Formatted string with basic environment information
    """
    info_parts = []

    # 1. 包管理器版本（根据OS信息自动推断）
    package_info = []

    # Check for apt (Debian/Ubuntu)
    apt_version = get_system_info("apt --version 2>/dev/null | head -1")
    if apt_version:
        package_info.append(f"APT: {apt_version}")

    # Check for dnf (Fedora/RHEL/Server)
    dnf_version = get_system_info("dnf --version 2>/dev/null | head -1")
    if dnf_version:
        package_info.append(f"DNF: {dnf_version}")

    # Check for yum (older RHEL/CentOS)
    yum_version = get_system_info("yum --version 2>/dev/null | head -1")
    if yum_version and not dnf_version:
        package_info.append(f"YUM: {yum_version}")

    # Check for pacman (Arch Linux)
    pacman_version = get_system_info("pacman --version 2>/dev/null | head -1")
    if pacman_version:
        package_info.append(f"Pacman: {pacman_version}")

    # Check for zypper (openSUSE)
    zypper_version = get_system_info("zypper --version 2>/dev/null | head -1")
    if zypper_version:
        package_info.append(f"Zypper: {zypper_version}")

    if package_info:
        info_parts.append("Package Managers:")
        for pkg in package_info:
            info_parts.append(f"  {pkg}")

    # 2. 身份感知（USER，UID，GROUPS）
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

    # 3. 身份溯源（SUDO_USER）
    sudo_user = os.getenv("SUDO_USER")
    if sudo_user:
        sudo_uid = os.getenv("SUDO_UID")
        sudo_gid = os.getenv("SUDO_GID")
        info_parts.append(
            f"Sudo Origin: SUDO_USER={sudo_user}, SUDO_UID={sudo_uid}, SUDO_GID={sudo_gid}"
        )

    # 4. 依赖完整性检查（LD_LIBRARY_PATH）
    ld_library_path = os.getenv("LD_LIBRARY_PATH", "")
    if ld_library_path:
        info_parts.append(f"Library Path: LD_LIBRARY_PATH={ld_library_path}")
    else:
        info_parts.append(
            "Library Path: LD_LIBRARY_PATH=(not set, using system defaults)"
        )

    return "\n".join(info_parts)


def get_current_env_info() -> str:
    """Get current environment information including locale and working directory.

    Returns:
        Formatted string with current environment information
    """
    info_parts = []

    # 1. LANG/LC_ALL系统语言信息
    lang = os.getenv("LANG", "not set")
    lc_all = os.getenv("LC_ALL", "not set")

    info_parts.append(f"System Language: LANG={lang}, LC_ALL={lc_all}")

    # 2. 路径感知（PWD）
    pwd = os.getenv("PWD", os.getcwd())
    info_parts.append(f"Current Directory (PWD): {pwd}")

    return "\n".join(info_parts)


def load_static_env_cache() -> dict | None:
    """加载静态环境信息缓存.

    Returns:
        包含静态环境信息的字典，如果缓存不存在或无效则返回 None
    """
    if not ENV_CACHE_FILE.exists():
        return None

    try:
        with open(ENV_CACHE_FILE, "r", encoding="utf-8") as f:
            cache = json.load(f)
        # 验证缓存数据结构
        if all(k in cache for k in ("uname_info", "os_info", "basic_env_info")):
            return cache
    except (json.JSONDecodeError, IOError):
        pass
    return None


def save_static_env_cache(uname_info: str, os_info: str, basic_env_info: str) -> None:
    """保存静态环境信息到缓存文件.

    Args:
        uname_info: 系统信息 (uname -a)
        os_info: 操作系统信息 (/etc/issue)
        basic_env_info: 基本环境信息
    """
    # 确保目录存在
    ENV_CACHE_FILE.parent.mkdir(parents=True, exist_ok=True)

    cache = {
        "uname_info": uname_info,
        "os_info": os_info,
        "basic_env_info": basic_env_info,
    }

    with open(ENV_CACHE_FILE, "w", encoding="utf-8") as f:
        json.dump(cache, f, ensure_ascii=False, indent=2)


def get_or_fetch_static_env_info() -> tuple[str, str, str]:
    """获取静态环境信息，优先从缓存读取.

    Returns:
        (uname_info, os_info, basic_env_info) 三元组
    """
    cache = load_static_env_cache()
    if cache:
        return (
            cache["uname_info"],
            cache["os_info"],
            cache["basic_env_info"],
        )

    # 缓存不存在，获取并保存
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

    # Check for double-quoted or single-quoted occurrence
    # We look for the part surrounded by quotes
    for quote in ['"', "'"]:
        # Pattern: quote + part + quote
        # Need to escape special regex characters in part
        escaped_part = re.escape(part)
        pattern = f"{quote}{escaped_part}{quote}"
        if re.search(pattern, original_cmd):
            return True

    return False


def escape_command_with_paths(command: str) -> str:
    """
    Return the command as-is since bash handles all escaping correctly.

    This function is kept for backward compatibility but no longer processes
    the command. All escaping, wildcards, and special characters are now
    handled directly by bash.

    Args:
        command: The shell command

    Returns:
        The original command unchanged
    """
    return command
