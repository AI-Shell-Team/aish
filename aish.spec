# -*- mode: python ; coding: utf-8 -*-
"""
PyInstaller spec file for  AI Shell
Build command: pyinstaller aish.spec
"""

import os
from PyInstaller.utils.hooks import collect_data_files


def _filter_linux_toolchain_libs(binaries):
    """Filter bundled shared libraries on Linux.

    Why:
    - PyInstaller's bootloader prepends the extracted/bundle directory to
      LD_LIBRARY_PATH.
    - If we ship an older toolchain library (libstdc++.so.6 / libgcc_s.so.1),
      child processes (e.g. apt) may accidentally load it and fail with
      GLIBCXX_* symbol version errors.

    Default policy:
    - Exclude toolchain libs (always).
    - Exclude common system libs by default to prefer target system libraries.

    Build-time override:
    - Set AISH_PYI_EXCLUDE_SYSTEM_SOS=0 to keep bundling common system libs.
    """

    if os.name != "posix":
        return binaries

    # Default to excluding system libs ("1").
    exclude_system_sos = os.environ.get("AISH_PYI_EXCLUDE_SYSTEM_SOS", "1") == "1"

    # Always exclude toolchain libs to avoid polluting child processes.
    exclude_prefixes = {
        "libstdc++.so.6",
        "libgcc_s.so.1",
    }

    # Optional: rely on target system for common system libs.
    if exclude_system_sos:
        exclude_prefixes.update(
            {
                "libncursesw.so.",
                "libtinfo.so.",
                "libsqlite3.so.",
                "libuuid.so.",
                "libz.so.",
                "libbz2.so.",
                "liblzma.so.",
            }
        )

    filtered = []
    for entry in binaries:
        # Expected format: (dest_name, src_name, typecode)
        try:
            dest_name, src_name, typecode = entry
        except ValueError:
            filtered.append(entry)
            continue

        base = os.path.basename(dest_name)
        if any(base.startswith(prefix) for prefix in exclude_prefixes):
            continue

        filtered.append((dest_name, src_name, typecode))

    return filtered

# Collect tiktoken data files from the build-time cache.
tiktoken_cache_root = os.environ.get(
    "AISH_BUILD_TIKTOKEN_CACHE_DIR",
    os.path.join("build", "tiktoken_cache"),
)
if not os.path.isdir(tiktoken_cache_root):
    raise SystemExit(
        "Missing build-time tiktoken cache. Run packaging/prefetch_tiktoken_cache.py before PyInstaller."
    )

tiktoken_datas = [(os.path.join(tiktoken_cache_root, "*"), "tiktoken_cache")]

# Collect litellm tokenizers (for offline token counting)
litellm_datas = collect_data_files('litellm', includes=['litellm_core_utils/tokenizers/*'])

# Collect root-level JSON price/context data files
litellm_json_datas = collect_data_files('litellm', includes=['*.json'])

block_cipher = None

# Collect default skills directory
skills_datas = []
if os.path.exists('debian/skills'):
    for skill_name in os.listdir('debian/skills'):
        skill_path = os.path.join('debian/skills', skill_name)
        if os.path.isdir(skill_path):
            skills_datas.append((skill_path, os.path.join('aish', 'skills', skill_name)))

a = Analysis(
    ['main.py'],
    pathex=['src'],
    binaries=[],
    datas=[
        ('src/aish/prompts', 'aish/prompts'),
        ('src/aish/i18n', 'aish/i18n'),
        ('src/aish/scripts/themes', 'aish/scripts/themes'),
        ('src/aish/scripts/templates', 'aish/scripts/templates'),
        ('src/aish/terminal/pty/bash_rc_wrapper.sh', 'aish/terminal/pty'),
    ] + tiktoken_datas + litellm_datas + litellm_json_datas + skills_datas,
    hiddenimports=[
        'aish.prompts',
        'aish.shell',
        'aish.config',
        'aish.llm',
        'aish.command',
        'dotenv',
        'litellm',
        'yaml',
        'pydantic',
        'prompt_toolkit',
        'prompt_toolkit.history',
        'prompt_toolkit.auto_suggest',
        'prompt_toolkit.completion',
        'prompt_toolkit.shortcuts',
        'rich',
        'rich.console',
        'rich.panel',
        'rich.markdown',
        'typer',
        'click',
        'asyncio',
        'concurrent.futures',
        'threading',
        'pty',
        'select',
        'signal',
        'subprocess',
        'tempfile',
        'termios',
        'tty',
        'shlex',
        'tiktoken',
        'tiktoken_ext',
        'tiktoken_ext.openai_public',
        'tiktoken.load',
        'tiktoken.core',
        'regex',
    ],
    hookspath=[],
    hooksconfig={},
    runtime_hooks=[],
    excludes=[
        'tkinter',
        'matplotlib',
        'numpy',
        'scipy',
        'pandas',
        'wx',
        'PyQt5',
        'PyQt6',
        'PySide2', 
        'PySide6',
    ],
    win_no_prefer_redirects=False,
    win_private_assemblies=False,
    cipher=block_cipher,
    noarchive=False,
)

# Prevent bundling toolchain libs that may pollute child processes.
a.binaries = _filter_linux_toolchain_libs(a.binaries)

pyz = PYZ(a.pure, a.zipped_data, cipher=block_cipher)

exe = EXE(
    pyz,
    a.scripts,
    a.binaries,
    a.zipfiles,
    a.datas,
    [],
    name='aish',
    debug=False,
    bootloader_ignore_signals=False,
    strip=False,
    upx=True,
    upx_exclude=[],
    runtime_tmpdir=None,
    console=True,
    disable_windowed_traceback=False,
    target_arch=None,
    codesign_identity=None,
    entitlements_file=None,
) 

# Build the privileged sandbox daemon as a separate binary.
# This lets standalone deb packaging ship /usr/bin/aish-sandbox without relying
# on system Python or vendored site-packages.
a_sandbox = Analysis(
    ['src/aish/sandboxd.py'],
    pathex=['src'],
    binaries=[],
    datas=[],
    hiddenimports=[
        'aish.security.sandbox_daemon',
        'aish.security.sandbox_worker',
    ],
    hookspath=[],
    hooksconfig={},
    runtime_hooks=[],
    excludes=[
        'tkinter',
        'matplotlib',
        'numpy',
        'scipy',
        'pandas',
        'wx',
        'PyQt5',
        'PyQt6',
        'PySide2',
        'PySide6',
    ],
    win_no_prefer_redirects=False,
    win_private_assemblies=False,
    cipher=block_cipher,
    noarchive=False,
)

# Same filtering for the sandbox daemon binary.
a_sandbox.binaries = _filter_linux_toolchain_libs(a_sandbox.binaries)

pyz_sandbox = PYZ(a_sandbox.pure, a_sandbox.zipped_data, cipher=block_cipher)

exe_sandbox = EXE(
    pyz_sandbox,
    a_sandbox.scripts,
    a_sandbox.binaries,
    a_sandbox.zipfiles,
    a_sandbox.datas,
    [],
    name='aish-sandbox',
    debug=False,
    bootloader_ignore_signals=False,
    strip=False,
    upx=True,
    upx_exclude=[],
    runtime_tmpdir=None,
    console=True,
    disable_windowed_traceback=False,
    target_arch=None,
    codesign_identity=None,
    entitlements_file=None,
)