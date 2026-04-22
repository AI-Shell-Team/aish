from __future__ import annotations

__version__ = "0.2.3"

# Avoid importing heavy modules (and any side-effects) at package import time.
# This matters for system services like aish-sandbox, which only need aish.sandboxd.
def __getattr__(name: str):
    if name == "PTYAIShell":
        from .shell import PTYAIShell as _PTYAIShell

        return _PTYAIShell
    if name == "run_shell":
        from .shell import run_shell as _run_shell

        return _run_shell
    if name == "main":
        from .cli import main as _main

        return _main
    raise AttributeError(name)
