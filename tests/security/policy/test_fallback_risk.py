from __future__ import annotations

import io
from pathlib import Path

from rich.console import Console

from aish.security.sandbox import SandboxSecurityResult
from aish.security.sandbox_types import SandboxResult
from aish.security.security_config import load_security_policy
from aish.security.security_manager import SimpleSecurityManager
from aish.security.security_policy import PolicyRule, RiskLevel, SandboxOffAction, SecurityPolicy


def _quiet_console() -> Console:
    return Console(file=io.StringIO(), force_terminal=False, width=120)


def test_load_security_policy_parses_sandbox_off_action(tmp_path: Path):
    policy_path = tmp_path / "security_policy.yaml"
    policy_path.write_text(
        "global:\n"
        "  enable_sandbox: false\n"
        "  sandbox_off_action: BLOCK\n"
        "rules: []\n",
        encoding="utf-8",
    )

    policy = load_security_policy(config_path=policy_path)
    assert policy.enable_sandbox is False
    assert policy.sandbox_off_action == SandboxOffAction.BLOCK


def test_sandbox_fallback_high_blocks_ai_command():
    policy = SecurityPolicy(
        enable_sandbox=False,
        rules=[],
        sandbox_off_action=SandboxOffAction.BLOCK,
    )
    mgr = SimpleSecurityManager(
        policy=policy,
        console=_quiet_console(),
    )

    decision = mgr.decide("echo hi", is_ai_command=True)
    assert decision.allow is False
    assert decision.require_confirmation is False


def test_sandbox_disabled_confirm_action_allows_without_confirmation_popup():
    policy = SecurityPolicy(
        enable_sandbox=False,
        rules=[],
        sandbox_off_action=SandboxOffAction.CONFIRM,
    )
    mgr = SimpleSecurityManager(
        policy=policy,
        console=_quiet_console(),
    )

    decision = mgr.decide("echo hi", is_ai_command=True)
    assert decision.allow is True
    assert decision.require_confirmation is False


def test_sandbox_fallback_low_allows_without_confirmation():
    policy = SecurityPolicy(
        enable_sandbox=False,
        rules=[],
        sandbox_off_action=SandboxOffAction.ALLOW,
    )
    mgr = SimpleSecurityManager(
        policy=policy,
        console=_quiet_console(),
    )

    decision = mgr.decide("echo hi", is_ai_command=True)
    assert decision.allow is True
    assert decision.require_confirmation is False


def test_load_security_policy_parses_rule_command_list(tmp_path: Path):
    policy_path = tmp_path / "security_policy.yaml"
    policy_path.write_text(
        "global:\n"
        "  enable_sandbox: false\n"
        "rules:\n"
        "  - id: H-001\n"
        "    command_list: [rm]\n"
        "    path: [/etc/**]\n"
        "    operations: [DELETE]\n"
        "    risk: HIGH\n",
        encoding="utf-8",
    )

    policy = load_security_policy(config_path=policy_path)
    matched = next(rule for rule in policy.rules if rule.rule_id == "H-001")
    assert matched.command_list == {"rm"}


def test_policy_disabled_rm_high_risk_path_blocks_without_sandbox():
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
    mgr = SimpleSecurityManager(policy=policy, console=_quiet_console())

    decision = mgr.decide("rm -rf /etc", is_ai_command=True)

    assert decision.allow is False
    assert decision.require_confirmation is False
    assert decision.analysis.get("fallback_rule_matched") is True


def test_policy_disabled_bash_lc_rm_high_risk_path_blocks_without_sandbox():
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
    mgr = SimpleSecurityManager(policy=policy, console=_quiet_console())

    decision = mgr.decide("bash -lc 'rm -rf /etc'", is_ai_command=True)

    assert decision.allow is False
    assert decision.require_confirmation is False
    assert decision.analysis.get("fallback_rule_matched") is True


def test_policy_disabled_complex_bash_lc_falls_back_to_global_action():
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
    mgr = SimpleSecurityManager(policy=policy, console=_quiet_console())

    decision = mgr.decide("bash -lc 'echo ok; rm -rf /etc'", is_ai_command=True)

    assert decision.allow is True
    assert decision.require_confirmation is False
    assert decision.analysis.get("fallback_rule_matched") is None


def test_policy_disabled_cp_write_path_matches_rule_without_hardcode():
    policy = SecurityPolicy(
        enable_sandbox=False,
        rules=[
            PolicyRule(
                pattern="/home/**",
                risk=RiskLevel.MEDIUM,
                operations={"WRITE"},
                command_list={"cp"},
                rule_id="M-001",
            )
        ],
        sandbox_off_action=SandboxOffAction.ALLOW,
    )
    mgr = SimpleSecurityManager(policy=policy, console=_quiet_console())

    decision = mgr.decide("cp /tmp/a.txt /home/lixin/a.txt", is_ai_command=True)

    assert decision.allow is True
    assert decision.require_confirmation is True
    assert decision.analysis.get("fallback_rule_matched") is True


def test_policy_disabled_rm_wildcard_path_matches_rule():
    policy = SecurityPolicy(
        enable_sandbox=False,
        rules=[
            PolicyRule(
                pattern="/home/**",
                risk=RiskLevel.MEDIUM,
                operations={"DELETE"},
                command_list={"rm"},
                rule_id="M-001",
            )
        ],
        sandbox_off_action=SandboxOffAction.ALLOW,
    )
    mgr = SimpleSecurityManager(policy=policy, console=_quiet_console())

    decision = mgr.decide("rm -rf /home/lixin/testdir/*", is_ai_command=True)

    assert decision.allow is True
    assert decision.require_confirmation is True
    assert decision.analysis.get("fallback_rule_matched") is True


def test_fallback_rule_does_not_affect_enabled_sandbox_flow(tmp_path: Path):
    class DummySandbox:
        enabled = True

        def run(self, command: str, cwd: Path | None = None) -> SandboxSecurityResult:
            return SandboxSecurityResult(
                command=command,
                cwd=(cwd or tmp_path),
                sandbox=SandboxResult(exit_code=0, stdout="", stderr="", changes=[]),
            )

    policy = SecurityPolicy(
        enable_sandbox=True,
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
    mgr = SimpleSecurityManager(policy=policy, console=_quiet_console(), repo_root=tmp_path, use_privileged_sandbox=False)
    mgr._sandbox_security = DummySandbox()  # type: ignore[attr-defined]

    decision = mgr.decide("bash -lc 'rm -rf /etc'", is_ai_command=True, cwd=tmp_path)

    assert decision.allow is True
    assert decision.require_confirmation is False
    assert decision.analysis.get("sandbox", {}).get("enabled") is True
    assert decision.analysis.get("fallback_rule_matched") is None
