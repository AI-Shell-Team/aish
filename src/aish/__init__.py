from __future__ import annotations

__version__ = "0.1.3"

# Avoid importing heavy modules (and any side-effects) at package import time.
# This matters for system services like aish-sandbox, which only need aish.sandboxd.
__all__ = ["AIShell", "main"]


def __getattr__(name: str):
    if name == "AIShell":
        from .shell import AIShell as _AIShell

        return _AIShell
    if name == "main":
        from .cli import main as _main

        return _main
    raise AttributeError(name)
