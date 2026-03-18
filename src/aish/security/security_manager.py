from __future__ import annotations

import os
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Dict, Optional, Tuple, Union

from rich.console import Console
from rich.panel import Panel

from aish.i18n import t

from .fallback_rule_engine import FallbackRuleEngine
from .sandbox import (
    DEFAULT_SANDBOX_SOCKET_PATH,
    SandboxConfig,
    SandboxSecurity,
    SandboxUnavailableError,
)
from .sandbox_ipc import SandboxSecurityIpc
from .security_policy import (AiRiskAssessment, AiRiskEngine, RiskLevel,
                              SandboxOffAction, SecurityPolicy, load_policy)

_FAIL_OPEN_PANEL_SHOWN = False


@dataclass
class SecurityDecision:
    """最终执行决策。

    Attributes:
        level:      评估得到的风险等级。
        allow:      是否允许执行命令。
        require_confirmation: 是否在执行前需要用户确认。
        analysis:   详细分析数据，供 UI / 日志使用。
    """

    level: RiskLevel
    allow: bool
    require_confirmation: bool
    analysis: Dict[str, Any]


class SimpleSecurityManager:
    """基于沙箱 + SecurityPolicy + AiRiskEngine 的统一安全管理器。

    相比旧版 simple_security_manager：

    - 去掉 heuristic_engine / context_analyzer 等旧规则体系，只保留沙箱 + 路径策略；
    - 风险等级统一使用 RiskLevel(LOW/MEDIUM/HIGH)；
    - 固定使用 balanced 的确认策略；
    - 提供 analyze_command_risk / decide 两个核心 API。
    """

    def __init__(
        self,
        *,
        console: Optional[Console] = None,
        repo_root: Optional[Path] = None,
        policy: Optional[SecurityPolicy] = None,
        use_privileged_sandbox: bool = True,
        privileged_sandbox_socket: Optional[Path] = None,
    ) -> None:
        self.console = console or Console()

        self._policy = policy or load_policy()

        self._repo_root = (repo_root or Path("/")).resolve()
        self._sandbox_security: Optional[Union[SandboxSecurity, SandboxSecurityIpc]] = (
            None
        )
        self._sandbox_disabled_reason: Optional[str] = None
        if not self._policy.enable_sandbox:
            self._sandbox_disabled_reason = "sandbox_disabled_by_policy"

        sandbox_enabled = bool(self._policy.enable_sandbox)
        if sandbox_enabled:
            # 当前不再基于策略推导 bind 白名单：直接使用最小配置。
            # 只读/读写 bind 的缺省行为由 SandboxExecutor 内部处理。
            sandbox_config = SandboxConfig(repo_root=self._repo_root)
            # 统一优先使用特权沙箱服务（IPC），避免按调用者 uid 分叉出
            # 本地直连与服务端两套行为路径。若 IPC 不可用，会在
            # analyze_command_risk() 里捕获并按原逻辑降级。
            socket_path = privileged_sandbox_socket or DEFAULT_SANDBOX_SOCKET_PATH
            if use_privileged_sandbox:
                self._sandbox_security = SandboxSecurityIpc(
                    repo_root=self._repo_root,
                    enabled=True,
                    socket_path=socket_path,
                )
            else:
                self._sandbox_security = SandboxSecurity(
                    repo_root=self._repo_root,
                    enabled=True,
                    config=sandbox_config,
                )

        self._ai_engine = AiRiskEngine(self._policy)
        self._fallback_rule_engine = FallbackRuleEngine(self._policy)

        # 固定为 balanced 的确认策略
        self._config: Dict[str, Any] = {
            "confirm_for_low": False,
            "confirm_for_medium": True,
            "confirm_for_high": True,
            "show_low_warnings": True,
        }

    def _show_fail_open_panel_once(self, analysis: Dict[str, Any]) -> None:
        global _FAIL_OPEN_PANEL_SHOWN
        if _FAIL_OPEN_PANEL_SHOWN:
            return

        # Only show for interactive/common user sessions.
        try:
            if os.geteuid() == 0:
                return
        except Exception:
            return

        sandbox_info = analysis.get("sandbox") if isinstance(analysis, dict) else None
        action_raw = (
            analysis.get("sandbox_off_action") if isinstance(analysis, dict) else None
        )
        try:
            action = SandboxOffAction(str(action_raw).upper())
        except Exception:
            action = SandboxOffAction.CONFIRM
        action_display = t(f"security.sandbox_off_action.{action.value.lower()}")
        reason = "unknown"
        error = None
        if isinstance(sandbox_info, dict):
            reason = str(sandbox_info.get("reason") or "unknown")
            error = sandbox_info.get("error")

        # 普通用户场景下：
        # - 如果是 IPC 不可用，通常是 aish-sandbox.socket 未安装/未启用/未运行。
        # - 如果是本地 overlay/bwrap 失败，则多半确实需要 root/mount 权限。
        show_error = True
        display_reason = reason
        if os.geteuid() != 0:
            if reason == "sandbox_ipc_unavailable":
                display_reason = t("security.sandbox_unavailable.ipc_unavailable")
            elif reason == "sandbox_ipc_failed":
                display_reason = t("security.sandbox_unavailable.ipc_failed")
            elif reason == "sandbox_execute_failed":
                display_reason = t(
                    "security.sandbox_unavailable.sandbox_execute_failed"
                )
            elif reason in {
                "overlay_mount_failed",
                "overlay_perm_failed",
                "bubblewrap_failed",
                "sandbox_requires_root",
                "sandbox_unavailable",
            }:
                display_reason = t("security.sandbox_unavailable.root_required")
                # 这类错误往往是系统权限/内核限制，原始错误信息可读性一般。
                show_error = False

        details = (
            "\n[dim]"
            + t("security.sandbox_unavailable.reason", reason=display_reason)
            + "[/dim]"
            if display_reason
            else ""
        )
        if error and show_error:
            details += (
                "\n[dim]"
                + t("security.sandbox_unavailable.error", error=str(error))
                + "[/dim]"
            )

        message_lines = [
            t("security.sandbox_unavailable.line1"),
            t("security.sandbox_unavailable.line2", action=action_display),
        ]
        if reason in {"sandbox_disabled_by_policy", "sandbox_disabled"}:
            message_lines = [
                t("security.sandbox_unavailable.policy_line1"),
                t("security.sandbox_unavailable.policy_line2", action=action_display),
            ]

        self.console.print(
            Panel(
                "\n".join(message_lines) + details,
                title=t("security.sandbox_unavailable.title"),
                border_style="yellow",
            )
        )
        _FAIL_OPEN_PANEL_SHOWN = True

    # ------------------------------------------------------------------
    # 核心评估 API
    # ------------------------------------------------------------------

    def analyze_command_risk(
        self,
        command: str,
        *,
        is_ai_command: bool = False,
        cwd: Optional[Path] = None,
    ) -> Tuple[RiskLevel, Dict[str, Any]]:
        """评估命令风险。

        - AI 命令：使用沙箱 + AiRiskEngine + SecurityPolicy；
        - 非 AI 命令：目前视为 LOW，并给出简要提示（未来可扩展）。
        """

        analysis: Dict[str, Any] = {
            "is_ai_command": is_ai_command,
            "risk_level": RiskLevel.LOW.value,
            "reasons": [],
            "changes": [],
            "sandbox": {"enabled": False},
            "fail_open": False,
        }

        if not is_ai_command:
            return RiskLevel.LOW, analysis

        effective_cwd = (cwd or self._repo_root).resolve()

        sandbox_off_action = getattr(
            self._policy, "sandbox_off_action", SandboxOffAction.CONFIRM
        )
        action_display = t(
            f"security.sandbox_off_action.{sandbox_off_action.value.lower()}"
        )
        fallback_risk: RiskLevel
        if sandbox_off_action == SandboxOffAction.BLOCK:
            fallback_risk = RiskLevel.HIGH
        elif sandbox_off_action == SandboxOffAction.ALLOW:
            fallback_risk = RiskLevel.LOW
        else:
            fallback_risk = RiskLevel.MEDIUM

        forced_confirm_risk = RiskLevel.MEDIUM

        # 以下为 AI 命令路径：
        # 如果沙箱关闭、不可用或执行失败，则无法获取变更信息做风险评估。
        # 在此情况下，系统将使用策略中定义的 sandbox_off_action 作为最终处理动作，
        # 并将其映射为对应的风险等级用于内部展示。
        if not self._sandbox_security or not self._sandbox_security.enabled:
            reason = self._sandbox_disabled_reason or "sandbox_disabled"
            fallback_assessment = None
            if reason == "sandbox_disabled_by_policy":
                fallback_assessment = self._fallback_rule_engine.assess_disabled_command(command)

            if fallback_assessment is not None:
                primary_rule = fallback_assessment.primary_rule
                reasons = [primary_rule.reason] if primary_rule.reason else list(fallback_assessment.reasons[:1])

                alternatives: list[str] = []
                if primary_rule.suggestion:
                    alternatives = [line.strip() for line in primary_rule.suggestion.splitlines() if line.strip()]

                analysis["risk_level"] = fallback_assessment.level.value
                analysis["reasons"] = reasons
                analysis["changes"] = [
                    {"path": path, "kind": "fallback_deleted"} for path in fallback_assessment.matched_paths
                ]
                analysis["sandbox"] = {"enabled": False, "reason": reason}
                analysis["sandbox_off_action"] = sandbox_off_action.value
                analysis["fallback_rule_matched"] = True
                analysis["matched_rule"] = {
                    "id": primary_rule.rule_id,
                    "name": primary_rule.name,
                    "pattern": primary_rule.pattern,
                }
                analysis["matched_paths"] = list(fallback_assessment.matched_paths)
                analysis["impact_description"] = ""
                analysis["suggested_alternatives"] = alternatives
                if primary_rule.confirm_message:
                    analysis["confirm_message"] = primary_rule.confirm_message
                analysis["fail_open"] = False
                return fallback_assessment.level, analysis

            analysis["risk_level"] = fallback_risk.value
            if reason == "sandbox_disabled_by_policy":
                analysis["reasons"].append(
                    t(
                        "security.risk_reason.sandbox_disabled_by_policy",
                        action=action_display,
                    )
                )
            else:
                analysis["reasons"].append(
                    t("security.risk_reason.sandbox_disabled", action=action_display)
                )
            analysis["sandbox"] = {"enabled": False, "reason": reason}
            analysis["sandbox_off_action"] = sandbox_off_action.value
            analysis["fail_open"] = sandbox_off_action == SandboxOffAction.ALLOW
            return fallback_risk, analysis

        # 当前沙箱实现要求 cwd 必须在 repo_root 下（否则内部会报错/退出）。
        # 为避免相对路径误判，这里遇到不满足条件时安全降级为“需要人工确认”。
        if not effective_cwd.is_relative_to(self._repo_root):
            analysis["risk_level"] = fallback_risk.value
            analysis["reasons"].append(
                t(
                    "security.risk_reason.cwd_outside_repo_root",
                    cwd=str(effective_cwd),
                    root=str(self._repo_root),
                    action=action_display,
                )
            )
            analysis["sandbox"] = {
                "enabled": False,
                "reason": "cwd_outside_repo_root",
                "repo_root": str(self._repo_root),
                "cwd": str(effective_cwd),
            }
            analysis["sandbox_off_action"] = sandbox_off_action.value
            analysis["fail_open"] = sandbox_off_action == SandboxOffAction.ALLOW
            if os.geteuid() != 0:
                self._show_fail_open_panel_once(analysis)
            return fallback_risk, analysis

        # 在 repo_root 视图下执行 AI 命令
        try:
            sandbox_result = self._sandbox_security.run(command, cwd=effective_cwd)
        except SandboxUnavailableError as exc:
            # Sandbox is unavailable (commonly due to missing mount/unshare privileges).
            # Disable it for subsequent commands to avoid repeated noisy failures.
            # IPC 不可用往往是服务未启动/短暂不可用：不要永久禁用，允许用户启动服务后自动恢复。
            should_disable = True
            if isinstance(
                self._sandbox_security, SandboxSecurityIpc
            ) and exc.reason in {
                "sandbox_ipc_unavailable",
                "sandbox_ipc_failed",
                "sandbox_execute_failed",
                "sandbox_ipc_timeout",
                "sandbox_timeout",
            }:
                should_disable = False

            if self._sandbox_security is not None and should_disable:
                self._sandbox_security.set_enabled(False)
                self._sandbox_disabled_reason = exc.reason
            forced_confirm = exc.reason in {
                "sandbox_ipc_failed",
                "sandbox_execute_failed",
                "sandbox_ipc_timeout",
                "sandbox_timeout",
            }
            action_display_effective = (
                t("security.sandbox_off_action.confirm")
                if forced_confirm
                else action_display
            )
            analysis["risk_level"] = (
                forced_confirm_risk.value if forced_confirm else fallback_risk.value
            )
            analysis["reasons"].append(
                t(
                    "security.risk_reason.sandbox_unavailable",
                    action=action_display_effective,
                )
            )
            analysis["sandbox"] = {
                "enabled": False,
                "reason": exc.reason,
                "error": exc.details or str(exc),
            }
            analysis["sandbox_off_action"] = sandbox_off_action.value
            if forced_confirm:
                analysis["sandbox_off_action"] = SandboxOffAction.CONFIRM.value
                analysis["fail_open"] = False
            else:
                analysis["fail_open"] = sandbox_off_action == SandboxOffAction.ALLOW
            if os.geteuid() != 0 and exc.reason not in {
                "sandbox_execute_failed",
                "sandbox_ipc_timeout",
                "sandbox_timeout",
            }:
                self._show_fail_open_panel_once(analysis)
            return (forced_confirm_risk if forced_confirm else fallback_risk), analysis
        except Exception as exc:
            action_display_effective = t("security.sandbox_off_action.confirm")
            analysis["risk_level"] = fallback_risk.value
            analysis["reasons"].append(
                t(
                    "security.risk_reason.sandbox_exception",
                    action=action_display_effective,
                )
            )
            analysis["sandbox"] = {
                "enabled": False,
                "reason": "sandbox_exception",
                "error": f"{type(exc).__name__}: {exc}",
            }
            analysis["sandbox_off_action"] = SandboxOffAction.CONFIRM.value
            analysis["fail_open"] = False
            if os.geteuid() != 0:
                self._show_fail_open_panel_once(analysis)
            return fallback_risk, analysis
        if sandbox_result is None:
            action_display_effective = t("security.sandbox_off_action.confirm")
            analysis["risk_level"] = fallback_risk.value
            analysis["reasons"].append(
                t(
                    "security.risk_reason.sandbox_failed",
                    action=action_display_effective,
                )
            )
            analysis["sandbox"] = {"enabled": False, "reason": "sandbox_failed"}
            analysis["sandbox_off_action"] = SandboxOffAction.CONFIRM.value
            analysis["fail_open"] = False
            if os.geteuid() != 0:
                self._show_fail_open_panel_once(analysis)
            return fallback_risk, analysis

        # Sandbox returned a result but the command itself failed.
        # In this case, we cannot reliably assess the real side effects.
        # Force a confirmation fallback regardless of sandbox_off_action.
        if int(getattr(sandbox_result.sandbox, "exit_code", 1) or 0) != 0:
            action_display_effective = t("security.sandbox_off_action.confirm")
            analysis["risk_level"] = forced_confirm_risk.value
            analysis["reasons"].append(
                t(
                    "security.risk_reason.sandbox_unavailable",
                    action=action_display_effective,
                )
            )
            analysis["sandbox"] = {
                "enabled": False,
                "reason": "sandbox_execute_failed",
                "exit_code": int(sandbox_result.sandbox.exit_code),
            }
            analysis["sandbox_off_action"] = SandboxOffAction.CONFIRM.value
            analysis["fail_open"] = False
            return forced_confirm_risk, analysis

        ai_assessment: AiRiskAssessment = self._ai_engine.assess(
            command, sandbox_result.sandbox
        )

        analysis["risk_level"] = ai_assessment.level.value
        analysis["reasons"] = list(ai_assessment.reasons)
        analysis["changes"] = [
            {"path": ch.path, "kind": ch.kind} for ch in ai_assessment.changes
        ]
        analysis["sandbox"] = {
            "enabled": True,
            "exit_code": sandbox_result.sandbox.exit_code,
        }

        return ai_assessment.level, analysis

    def decide(
        self,
        command: str,
        *,
        is_ai_command: bool = False,
        cwd: Optional[Path] = None,
    ) -> SecurityDecision:
        """综合评估并给出最终执行决策。"""

        level, analysis = self.analyze_command_risk(
            command,
            is_ai_command=is_ai_command,
            cwd=cwd,
        )

        # AI 命令处理逻辑：当沙箱关闭、不可用或执行失败时，直接依据 sandbox_off_action 执行对应动作。
        if is_ai_command and isinstance(analysis.get("sandbox"), dict):
            if analysis["sandbox"].get("enabled") is False:
                if analysis.get("fallback_rule_matched"):
                    if level == RiskLevel.HIGH:
                        return SecurityDecision(
                            level=level,
                            allow=False,
                            require_confirmation=False,
                            analysis=analysis,
                        )
                    if level == RiskLevel.MEDIUM:
                        return SecurityDecision(
                            level=level,
                            allow=True,
                            require_confirmation=True,
                            analysis=analysis,
                        )
                    return SecurityDecision(
                        level=level,
                        allow=True,
                        require_confirmation=False,
                        analysis=analysis,
                    )

                sandbox_reason = str(analysis["sandbox"].get("reason") or "")
                sandbox_is_disabled = sandbox_reason in {
                    "sandbox_disabled",
                    "sandbox_disabled_by_policy",
                }
                action_raw = analysis.get("sandbox_off_action")
                try:
                    action = SandboxOffAction(str(action_raw).upper())
                except Exception:
                    action = SandboxOffAction.CONFIRM

                if action == SandboxOffAction.BLOCK:
                    return SecurityDecision(
                        level=level,
                        allow=False,
                        require_confirmation=False,
                        analysis=analysis,
                    )
                if action == SandboxOffAction.CONFIRM:
                    return SecurityDecision(
                        level=level,
                        allow=True,
                        require_confirmation=False if sandbox_is_disabled else True,
                        analysis=analysis,
                    )
                # ALLOW
                return SecurityDecision(
                    level=level,
                    allow=True,
                    require_confirmation=False,
                    analysis=analysis,
                )

        if level == RiskLevel.HIGH:
            allow = False
            require_confirmation = True
        elif level == RiskLevel.MEDIUM:
            allow = True
            require_confirmation = self._config["confirm_for_medium"]
        else:  # LOW
            allow = True
            require_confirmation = self._config["confirm_for_low"]

        return SecurityDecision(
            level=level,
            allow=allow,
            require_confirmation=require_confirmation,
            analysis=analysis,
        )


__all__ = ["SimpleSecurityManager", "SecurityDecision"]
