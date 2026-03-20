import json
import logging
import os
import sys
from pathlib import Path
from typing import ClassVar, Optional

from aish.builtin import BuiltinRegistry
from aish.config import BashOutputOffloadSettings
from aish.i18n import t
from aish.interruption import ShellState
from aish.offload import render_bash_output
from aish.security.security_manager import (SecurityDecision,
                                            SimpleSecurityManager)
from aish.tools.base import ToolBase
from aish.tools.bash_executor import UnifiedBashExecutor
from aish.tools.result import ToolResult

DISPLAY_MAX_LINES = 2
DISPLAY_ELLIPSIS = " ..."
logger = logging.getLogger("aish.tools.code_exec")


def _collapse_output_lines(text: str, max_lines: int = DISPLAY_MAX_LINES) -> str:
    lines = text.splitlines()
    if len(lines) <= max_lines:
        return text
    collapsed = lines[:max_lines]
    collapsed[-1] = f"{collapsed[-1]}{DISPLAY_ELLIPSIS}"
    return "\n".join(collapsed)


class PythonTool(ToolBase):
    def __init__(self) -> None:
        super().__init__(
            name="python_exec",
            description="Execute arbitrary Python code and return the result. Use print() for output.",
            parameters={
                "type": "object",
                "properties": {
                    "code": {
                        "type": "string",
                        "description": "The Python code to execute.",
                    }
                },
                "required": ["code"],
            },
        )

    def __call__(self, code: str) -> ToolResult:
        import io

        # Save and set current working directory for Python execution
        original_cwd = os.getcwd()

        output = io.StringIO()
        old_stdout = sys.stdout
        sys.stdout = output

        local_namespace = {"os": os}  # Provide os module for directory operations
        try:
            # Ensure Python code runs in the shell's current directory
            # Use repr() to properly escape the path string for Python
            safe_cwd = repr(original_cwd)
            exec(f"os.chdir({safe_cwd})\n{code}", local_namespace)

            # 直接显示 Python 输出到终端，类似于 BashTool 的做法
            python_output = output.getvalue().strip()
            if python_output:
                display_output = _collapse_output_lines(python_output)
                print(display_output, end="")

            # 返回实际的Python代码输出，而不是状态信息
            # 这样LLM就能看到真实的执行结果
            if python_output:
                # 如果输出太长，截断但保留关键信息
                if len(python_output) > 1000:
                    python_output = (
                        python_output[:1000] + "...\n[Output truncated due to length]"
                    )
                return ToolResult(ok=True, output=python_output)
            else:
                return ToolResult(
                    ok=True, output="Python code executed successfully with no output"
                )
        except Exception as e:
            # 显示错误信息
            error_msg = f"Error: {str(e)}"
            print(f"❌ {error_msg}")
            return ToolResult(
                ok=False,
                output=error_msg,
                meta={"exception_type": type(e).__name__},
            )
        finally:
            sys.stdout = old_stdout
            # Ensure we're still in the correct directory after Python execution
            try:
                os.chdir(original_cwd)
            except Exception:
                pass  # Ignore errors restoring directory


def _build_bash_tagged_result(
    *,
    stdout: str,
    stderr: str,
    return_code: int,
    offload_payload: dict,
) -> str:
    offload_json = json.dumps(
        offload_payload, ensure_ascii=False, separators=(",", ":")
    )
    return "\n".join(
        [
            "<stdout>",
            stdout,
            "</stdout>",
            "<stderr>",
            stderr,
            "</stderr>",
            "<return_code>",
            str(return_code),
            "</return_code>",
            "<offload>",
            offload_json,
            "</offload>",
        ]
    )


class BashTool(ToolBase):
    # 控制命令执行阶段是否允许立即中断
    # False = 等待命令执行完成再中断
    # True = 允许立即中断（可能导致部分结果丢失）
    ALLOW_INTERRUPT_COMMAND: ClassVar[bool] = False

    def __init__(
        self,
        env_manager=None,
        interruption_manager=None,
        cancellation_token_ref=None,
        history_manager=None,
        offload_settings: Optional[BashOutputOffloadSettings | dict] = None,
    ):
        """
        Args:
            cancellation_token_ref: A callable that returns the current cancellation token
            history_manager: HistoryManager instance for recording command history
        """
        super().__init__(
            name="bash_exec",
            description=(
                "\n".join(
                    [
                        "Execute shell commands or bash scripts and return output.",
                        "Notice that the same command may have different options and may have different output on different platforms.",
                        "IMPORTANT: Large stdout/stderr may be offloaded to disk; check the <offload> field for full output paths.",
                    ]
                )
                + "\n"
            ),
            parameters={
                "type": "object",
                "properties": {
                    "code": {
                        "type": "string",
                        "description": "The shell command to execute.",
                    }
                },
                "required": ["code"],
            },
        )

        # 对 AI 生成的 bash_exec 命令启用基于沙箱的安全评估：
        # repo_root 使用 "/"，在沙箱内 overlay 整个系统视图。
        self.security_manager = SimpleSecurityManager(repo_root=Path("/"))
        self._last_decision: Optional[SecurityDecision] = None
        self.history_manager = history_manager
        self.env_manager = env_manager
        self.interruption_manager = interruption_manager
        if isinstance(offload_settings, BashOutputOffloadSettings):
            self.offload_settings = offload_settings
        elif isinstance(offload_settings, dict):
            self.offload_settings = BashOutputOffloadSettings.model_validate(
                offload_settings
            )
        else:
            self.offload_settings = BashOutputOffloadSettings()

        # 创建统一执行器（不传递 history_manager，由调用方手动添加历史记录）
        self.executor = UnifiedBashExecutor(
            env_manager=env_manager,
            history_manager=None,
        )

        # 使用可调用对象来获取当前的 token，而不是直接保存引用
        # 这样当 LLMSession 重置 token 时，BashTool 能看到新的 token
        if callable(cancellation_token_ref):
            self._get_cancellation_token = cancellation_token_ref
        elif cancellation_token_ref is not None:
            # 如果传入的是直接的 token 对象（向后兼容），包装成函数
            self._get_cancellation_token = lambda: cancellation_token_ref
        else:
            self._get_cancellation_token = lambda: None

    def need_confirm_before_exec(self, code: Optional[str] = None) -> bool:
        """Override ToolBase method to integrate security check"""
        command = code
        if command is None:
            return False

        # 设置沙箱评估状态
        if self.interruption_manager:
            self.interruption_manager.set_state(ShellState.SANDBOX_EVAL)

        # 统一走 decide：
        # - allow=False（例如命中 HIGH 风险策略）：直接阻断，不进入确认流程
        # - allow=True 且 require_confirmation=True：进入确认
        # - allow=True 且无需确认：直接执行
        self._last_decision = self.security_manager.decide(
            command,
            is_ai_command=True,
            cwd=Path(os.getcwd()).resolve(),
        )

        if not self._last_decision.allow:
            return False
        return bool(self._last_decision.require_confirmation)

    def get_confirmation_info(self, code: Optional[str] = None) -> dict:
        """Get security information for confirmation dialog"""
        command = code
        if command is None:
            return {}

        decision = self._last_decision
        if decision is None or decision.analysis.get("is_ai_command") is not True:
            decision = self.security_manager.decide(
                command,
                is_ai_command=True,
                cwd=Path(os.getcwd()).resolve(),
            )
            self._last_decision = decision
        analysis_data = decision.analysis

        # 新安全体系的 analysis_data 已经包含 risk_level/reasons/changes/sandbox 等字段。
        return {
            "command": command,
            "security_analysis": analysis_data,
            "security_decision": {
                "allow": bool(decision.allow),
                "require_confirmation": bool(decision.require_confirmation),
            },
        }

    def get_session_output(self, result: ToolResult) -> str | None:
        if result.meta.get("kind") == "security_blocked":
            return ""
        return None

    async def __call__(self, code: str) -> ToolResult:
        """Execute bash command with unified state detection"""
        # 在真正执行命令前，检查是否不允许执行（HIGH 风险）
        decision = self._last_decision
        if decision is None or decision.analysis.get("is_ai_command") is not True:
            decision = self.security_manager.decide(
                code,
                is_ai_command=True,
                cwd=Path(os.getcwd()).resolve(),
            )
            self._last_decision = decision

        if not decision.allow:
            # 高风险命令被拦截，返回错误信息给AI
            analysis = decision.analysis or {}
            reasons = analysis.get("reasons") or []
            reason_text = ", ".join(reasons[:5])
            if reason_text:
                blocked_msg = t(
                    "security.command_blocked_with_reason", reason=reason_text
                )
            else:
                blocked_msg = t("security.command_blocked")
            blocked_output = _build_bash_tagged_result(
                stdout="",
                stderr=blocked_msg,
                return_code=126,
                offload_payload={"status": "inline", "reason": "below_threshold"},
            )
            return ToolResult(
                ok=False,
                output=blocked_output,
                code=126,
                meta={"kind": "security_blocked", "reasons": reasons},
                stop_tool_chain=True,
            )

        # Check for rejected commands first (e.g., exit, logout)
        if BuiltinRegistry.is_rejected_command(code):
            rejected_msg = BuiltinRegistry.get_rejected_command_message(code)
            if rejected_msg:
                rejected_output = _build_bash_tagged_result(
                    stdout="",
                    stderr=rejected_msg,
                    return_code=1,
                    offload_payload={"status": "inline", "reason": "below_threshold"},
                )
                return ToolResult(ok=False, output=rejected_output, code=1)

        # 设置命令执行状态
        if self.interruption_manager:
            self.interruption_manager.set_state(ShellState.COMMAND_EXEC)

        # 使用统一执行器执行命令（自动检测状态变化）
        success, stdout, stderr, returncode, changes = self.executor.execute(
            code, source="ai"
        )

        # 恢复状态
        if self.interruption_manager:
            self.interruption_manager.set_state(ShellState.NORMAL)

        # 手动记录到历史
        if self.history_manager:
            try:
                import asyncio

                loop = asyncio.get_event_loop()
                if loop.is_running():
                    asyncio.create_task(
                        self.history_manager.add_entry(
                            command=code,
                            source="ai",
                            returncode=returncode,
                            stdout=stdout,
                            stderr=stderr,
                        )
                    )
            except Exception:
                pass

        # 展示执行结果到终端
        if stdout:
            display_output = _collapse_output_lines(stdout)
            print(display_output)
        if stderr:
            display_stderr = _collapse_output_lines(stderr)
            print(f"\033[91m{display_stderr}\033[0m")  # 红色输出

        session_uuid = "unknown-session"
        if self.history_manager and hasattr(self.history_manager, "get_session_uuid"):
            try:
                session_uuid = self.history_manager.get_session_uuid()
            except Exception:
                session_uuid = "unknown-session"

        offload_render = render_bash_output(
            stdout=stdout,
            stderr=stderr,
            command=code,
            return_code=returncode,
            session_uuid=session_uuid,
            cwd=os.getcwd(),
            settings=self.offload_settings,
        )
        offload_status = str(offload_render.offload_payload.get("status", "inline"))
        if offload_status == "offloaded":
            logger.info(
                "bash_exec output offloaded: session=%s return_code=%s stdout_path=%s stderr_path=%s meta_path=%s",
                session_uuid,
                returncode,
                offload_render.offload_payload.get("stdout_path", ""),
                offload_render.offload_payload.get("stderr_path", ""),
                offload_render.offload_payload.get("meta_path", ""),
            )
        elif offload_status == "failed":
            logger.warning(
                "bash_exec output offload failed: session=%s return_code=%s error=%s",
                session_uuid,
                returncode,
                offload_render.offload_payload.get("error", "unknown"),
            )
        output = _build_bash_tagged_result(
            stdout=offload_render.stdout_text,
            stderr=offload_render.stderr_text,
            return_code=returncode,
            offload_payload=offload_render.offload_payload,
        )

        # 构造返回结果
        if success:
            return ToolResult(ok=True, output=output)

        return ToolResult(
            ok=False,
            output=output,
            code=returncode,
        )
