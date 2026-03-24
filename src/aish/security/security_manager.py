from __future__ import annotations

import os
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Dict, Optional, Tuple

from rich.console import Console
from rich.panel import Panel

from aish.i18n import t

from .fallback_rule_engine import FallbackRuleEngine
from .sandbox import (
    DEFAULT_SANDBOX_SOCKET_PATH,
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
        privileged_sandbox_socket: Optional[Path] = None,
    ) -> None:
        self.console = console or Console()

        self._policy = policy or load_policy()

        self._repo_root = (repo_root or Path("/")).resolve()
        self._sandbox_security: Optional[SandboxSecurityIpc] = None
        self._sandbox_disabled_reason: Optional[str] = None
        if not self._policy.enable_sandbox:
            self._sandbox_disabled_reason = "sandbox_disabled_by_policy"

        sandbox_enabled = bool(self._policy.enable_sandbox)
        if sandbox_enabled:
            socket_path = privileged_sandbox_socket or DEFAULT_SANDBOX_SOCKET_PATH
            self._sandbox_security = SandboxSecurityIpc(
                repo_root=self._repo_root,
                enabled=True,
                socket_path=socket_path,
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
        # - 如果是服务端沙箱执行失败，则多半是 root/mount 权限或内核能力受限。
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

    def _risk_for_action(self, action: SandboxOffAction) -> RiskLevel:
        if action == SandboxOffAction.BLOCK:
            return RiskLevel.HIGH
        if action == SandboxOffAction.CONFIRM:
            return RiskLevel.MEDIUM
        return RiskLevel.LOW

    def _apply_fallback_rule_match(
        self,
        analysis: Dict[str, Any],
        *,
        command: str,
        reason: str,
        sandbox_off_action: SandboxOffAction,
        sandbox_details: Optional[Dict[str, Any]] = None,
    ) -> Optional[Tuple[RiskLevel, Dict[str, Any]]]:
        fallback_assessment = self._fallback_rule_engine.assess_disabled_command(command)
        if fallback_assessment is None:
            return None

        primary_rule = fallback_assessment.primary_rule
        reasons = (
            [primary_rule.reason]
            if primary_rule.reason
            else list(fallback_assessment.reasons[:1])
        )

        alternatives: list[str] = []
        if primary_rule.suggestion:
            alternatives = [
                line.strip()
                for line in primary_rule.suggestion.splitlines()
                if line.strip()
            ]

        analysis["risk_level"] = fallback_assessment.level.value
        analysis["reasons"] = reasons
        analysis["changes"] = [
            {"path": path, "kind": "fallback_deleted"}
            for path in fallback_assessment.matched_paths
        ]
        sandbox_info: Dict[str, Any] = {"enabled": False, "reason": reason}
        if sandbox_details:
            sandbox_info.update(sandbox_details)
        analysis["sandbox"] = sandbox_info
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

    def _analyze_without_trusted_sandbox(
        self,
        analysis: Dict[str, Any],
        *,
        command: str,
        reason: str,
        sandbox_off_action: SandboxOffAction,
        reason_key: str,
        reason_kwargs: Optional[Dict[str, Any]] = None,
        sandbox_details: Optional[Dict[str, Any]] = None,
        force_confirm: bool = False,
        show_panel: bool = False,
        suppress_panel_reasons: Optional[set[str]] = None,
        use_fallback_rule: bool = True,
    ) -> Tuple[RiskLevel, Dict[str, Any]]:
        if use_fallback_rule:
            fallback_result = self._apply_fallback_rule_match(
                analysis,
                command=command,
                reason=reason,
                sandbox_off_action=sandbox_off_action,
                sandbox_details=sandbox_details,
            )
            if fallback_result is not None:
                return fallback_result

        effective_action = (
            SandboxOffAction.CONFIRM if force_confirm else sandbox_off_action
        )
        effective_risk = self._risk_for_action(effective_action)
        action_display = t(
            f"security.sandbox_off_action.{effective_action.value.lower()}"
        )

        kwargs: Dict[str, Any] = dict(reason_kwargs or {})
        kwargs.setdefault("action", action_display)

        analysis["risk_level"] = effective_risk.value
        analysis["reasons"].append(t(reason_key, **kwargs))
        sandbox_info: Dict[str, Any] = {"enabled": False, "reason": reason}
        if sandbox_details:
            sandbox_info.update(sandbox_details)
        analysis["sandbox"] = sandbox_info
        analysis["sandbox_off_action"] = effective_action.value
        analysis["fail_open"] = effective_action == SandboxOffAction.ALLOW

        if os.geteuid() != 0 and show_panel:
            if not suppress_panel_reasons or reason not in suppress_panel_reasons:
                self._show_fail_open_panel_once(analysis)

        return effective_risk, analysis

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

        # 以下为 AI 命令路径：
        # 如果沙箱关闭、不可用或执行失败，则无法获取变更信息做风险评估。
        # 在此情况下，系统将使用策略中定义的 sandbox_off_action 作为最终处理动作，
        # 并将其映射为对应的风险等级用于内部展示。
        if not self._sandbox_security or not self._sandbox_security.enabled:
            reason = self._sandbox_disabled_reason or "sandbox_disabled"
            reason_key = "security.risk_reason.sandbox_disabled"
            if reason == "sandbox_disabled_by_policy":
                reason_key = "security.risk_reason.sandbox_disabled_by_policy"
            return self._analyze_without_trusted_sandbox(
                analysis,
                command=command,
                reason=reason,
                sandbox_off_action=sandbox_off_action,
                reason_key=reason_key,
            )

        # 当前 code_exec 场景下 repo_root 通常为 "/"；若后续主流程传入更窄的
        # repo_root，这里继续按 sandbox_off_action 做保守降级，但不再额外走
        # 命令+路径兜底匹配，避免和真正的“无沙箱模式”混淆。
        if not effective_cwd.is_relative_to(self._repo_root):
            return self._analyze_without_trusted_sandbox(
                analysis,
                command=command,
                reason="cwd_outside_repo_root",
                sandbox_off_action=sandbox_off_action,
                reason_key="security.risk_reason.cwd_outside_repo_root",
                reason_kwargs={
                    "cwd": str(effective_cwd),
                    "root": str(self._repo_root),
                },
                sandbox_details={
                    "repo_root": str(self._repo_root),
                    "cwd": str(effective_cwd),
                },
                show_panel=True,
                use_fallback_rule=False,
            )

        # 在 repo_root 视图下执行 AI 命令
        try:
            sandbox_result = self._sandbox_security.run(command, cwd=effective_cwd)
        except SandboxUnavailableError as exc:
            use_fallback_rule = exc.reason in {
                "sandbox_disabled",
                "sandbox_disabled_by_policy",
                "sandbox_ipc_unavailable",
            }
            forced_confirm = exc.reason in {
                "sandbox_ipc_failed",
                "sandbox_ipc_protocol_error",
                "sandbox_execute_failed",
                "sandbox_ipc_timeout",
                "sandbox_timeout",
            }
            return self._analyze_without_trusted_sandbox(
                analysis,
                command=command,
                reason=exc.reason,
                sandbox_off_action=sandbox_off_action,
                reason_key="security.risk_reason.sandbox_unavailable",
                sandbox_details={"error": exc.details or str(exc)},
                force_confirm=forced_confirm,
                show_panel=True,
                suppress_panel_reasons={
                    "sandbox_execute_failed",
                    "sandbox_ipc_timeout",
                    "sandbox_timeout",
                },
                use_fallback_rule=use_fallback_rule,
            )
        except Exception as exc:
            return self._analyze_without_trusted_sandbox(
                analysis,
                command=command,
                reason="sandbox_exception",
                sandbox_off_action=sandbox_off_action,
                reason_key="security.risk_reason.sandbox_exception",
                sandbox_details={"error": f"{type(exc).__name__}: {exc}"},
                force_confirm=True,
                show_panel=True,
                use_fallback_rule=False,
            )
        if sandbox_result is None:
            return self._analyze_without_trusted_sandbox(
                analysis,
                command=command,
                reason="sandbox_failed",
                sandbox_off_action=sandbox_off_action,
                reason_key="security.risk_reason.sandbox_failed",
                force_confirm=True,
                show_panel=True,
                use_fallback_rule=False,
            )

        # Sandbox returned a result but the command itself failed.
        # In this case, we cannot reliably assess the real side effects.
        # Force a confirmation fallback regardless of sandbox_off_action.
        if int(getattr(sandbox_result.sandbox, "exit_code", 1) or 0) != 0:
            return self._analyze_without_trusted_sandbox(
                analysis,
                command=command,
                reason="sandbox_execute_failed",
                sandbox_off_action=sandbox_off_action,
                reason_key="security.risk_reason.sandbox_unavailable",
                sandbox_details={
                    "exit_code": int(sandbox_result.sandbox.exit_code),
                },
                force_confirm=True,
                use_fallback_rule=False,
            )

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
