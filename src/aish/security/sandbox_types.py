# src/aish/security/sandbox_types.py
from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Dict, List, Optional


@dataclass
class FsChange:
    """单个文件系统变更记录。

    Attributes:
        path: 逻辑路径，推荐使用相对于“工程根”的相对路径；如无法
            归一化，则可以使用绝对路径。
        kind: 变更类型，目前约定为 "created" / "modified" / "deleted"，
            未来可以扩展为 "chmod" / "chown" 等。
        detail: 可选的额外信息，例如旧/新权限等，暂时留作占位。
    """

    path: str
    kind: str
    detail: Optional[Dict[str, str]] = None


@dataclass
class SandboxResult:
    """沙箱执行结果摘要。

    由 SandboxExecutor 产生。风险评估只消费 exit_code 与文件系统变更。
    预跑的 stdout/stderr 不进入该结果，避免特权上下文的输出经 IPC 回传。
    """

    exit_code: int
    changes: List[FsChange]
    changes_truncated: bool = False


@dataclass
class SandboxSecurityResult:
    """Shared sandbox execution result for main-process callers."""

    command: str
    cwd: Path
    sandbox: SandboxResult
