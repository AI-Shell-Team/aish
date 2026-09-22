from __future__ import annotations

import grp
import pwd
from pathlib import Path

import pytest

import aish.security.security_manager as sm
from aish.security.sandbox import SandboxUnavailableError, strip_sudo_prefix
from aish.security.sandbox_types import SandboxResult, SandboxSecurityResult
from aish.security.security_manager import SimpleSecurityManager


def test_strip_sudo_prefix_preserves_shell_operators() -> None:
    stripped = strip_sudo_prefix("sudo apt update && sudo apt install -y nginx")
    assert stripped.sudo_detected is True
    assert stripped.ok is True
    assert stripped.user is None
    assert stripped.command == "apt update && sudo apt install -y nginx"
    assert stripped.payload_kind().kind == "root"


def test_strip_sudo_prefix_strips_options_and_preserves_quotes() -> None:
    cmd = "sudo -E -u root bash -lc 'echo hi && echo ok'"
    stripped = strip_sudo_prefix(cmd)
    assert stripped.sudo_detected is True
    assert stripped.ok is True
    assert stripped.user == "root"
    assert stripped.command == "bash -lc 'echo hi && echo ok'"
    assert stripped.payload_kind().kind == "root"


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
                sandbox=SandboxResult(exit_code=100, changes=[]),
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


def test_sudo_user_and_group_follow_the_target() -> None:
    stripped = strip_sudo_prefix("sudo -u nobody -g nogroup id")
    assert stripped.command == "id"
    assert stripped.user == "nobody"
    assert stripped.group == "nogroup"
    kind = stripped.payload_kind()
    assert kind.kind == "user"
    assert kind.uid == pwd.getpwnam("nobody").pw_uid
    assert kind.gid == grp.getgrnam("nogroup").gr_gid


def test_sudo_attached_and_equals_user_flags() -> None:
    attached = strip_sudo_prefix("sudo -unobody id")
    assert attached.user == "nobody"
    assert attached.command == "id"
    equals = strip_sudo_prefix("sudo --user=nobody id")
    assert equals.user == "nobody"
    assert equals.command == "id"


def test_numeric_sudo_user_resolves_without_becoming_root() -> None:
    stripped = strip_sudo_prefix("sudo -u 65534 id")
    kind = stripped.payload_kind()
    assert kind.kind == "user"
    assert kind.uid == 65534


def test_unknown_sudo_user_is_rejected() -> None:
    stripped = strip_sudo_prefix("sudo -u aish-no-such-user-xyz id")
    with pytest.raises(SandboxUnavailableError) as exc:
        stripped.payload_kind()
    assert exc.value.details == "sudo_unknown_user"


def test_sudo_without_command_is_rejected() -> None:
    stripped = strip_sudo_prefix("sudo -E -u root")
    assert stripped.ok is False
    with pytest.raises(SandboxUnavailableError) as exc:
        stripped.payload_kind()
    assert exc.value.details == "sudo_without_command"
