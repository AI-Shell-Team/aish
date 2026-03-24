from __future__ import annotations

from pathlib import Path

import aish.security.security_manager as sm
from aish.security.sandbox import strip_sudo_prefix
from aish.security.sandbox_types import SandboxResult, SandboxSecurityResult
from aish.security.security_manager import SimpleSecurityManager


def test_strip_sudo_prefix_preserves_shell_operators() -> None:
    stripped, sudo_detected, ok = strip_sudo_prefix(
        "sudo apt update && sudo apt install -y nginx"
    )
    assert sudo_detected is True
    assert ok is True
    assert stripped == "apt update && sudo apt install -y nginx"


def test_strip_sudo_prefix_strips_options_and_preserves_quotes() -> None:
    cmd = "sudo -E -u root bash -lc 'echo hi && echo ok'"
    stripped, sudo_detected, ok = strip_sudo_prefix(cmd)
    assert sudo_detected is True
    assert ok is True
    assert stripped == "bash -lc 'echo hi && echo ok'"


def test_sandbox_execute_failed_does_not_show_global_unavailable_panel(
    tmp_path: Path,
) -> None:
    class DummySandbox:
        enabled = True

        def set_enabled(self, enabled: bool) -> None:
            self.enabled = enabled

        def run(self, command: str, cwd: Path | None = None) -> SandboxSecurityResult:
            return SandboxSecurityResult(
                command=command,
                cwd=(cwd or tmp_path),
                sandbox=SandboxResult(
                    exit_code=100, stdout="", stderr="E: fail", changes=[]
                ),
            )

    manager = SimpleSecurityManager(
        repo_root=tmp_path,
    )
    manager._sandbox_security = DummySandbox()  # type: ignore[attr-defined]

    sm._FAIL_OPEN_PANEL_SHOWN = False

    _level, analysis = manager.analyze_command_risk(
        "sudo apt update && sudo apt install -y nginx",
        is_ai_command=True,
        cwd=tmp_path,
    )

    assert isinstance(analysis.get("sandbox"), dict)
    assert analysis["sandbox"]["reason"] == "sandbox_execute_failed"
    assert sm._FAIL_OPEN_PANEL_SHOWN is False


def test_policy_disabled_sudo_bash_lc_rm_hits_fallback_rule() -> None:
    from aish.security.security_policy import PolicyRule, RiskLevel, SandboxOffAction, SecurityPolicy

    policy = SecurityPolicy(
        enable_sandbox=False,
        rules=[
            PolicyRule(
                pattern="/etc/**",
                risk=RiskLevel.HIGH,
                operations={"DELETE"},
                command_list={"rm"},
                rule_id="H-001",
            )
        ],
        sandbox_off_action=SandboxOffAction.ALLOW,
    )
    manager = SimpleSecurityManager(policy=policy)

    decision = manager.decide("sudo -E -u root bash -lc 'rm -rf /etc'", is_ai_command=True)

    assert decision.allow is False
    assert decision.analysis.get("fallback_rule_matched") is True
