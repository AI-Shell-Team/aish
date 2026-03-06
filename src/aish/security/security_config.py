from __future__ import annotations

import logging
import os
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable, Optional

import yaml

from ..i18n import get_ui_locale
from .security_policy import (PolicyRule, RiskLevel, SandboxOffAction,
                              SecurityPolicy)

_LOGGER = logging.getLogger(__name__)


def _user_security_policy_path() -> Path:
    """Return the per-user security policy path.

    Linux default follows XDG:
    - $XDG_CONFIG_HOME/aish/security_policy.yaml
    - ~/.config/aish/security_policy.yaml
    """

    xdg_config_home = os.environ.get("XDG_CONFIG_HOME")
    base_dir = Path(xdg_config_home) if xdg_config_home else (Path.home() / ".config")
    return base_dir / "aish" / "security_policy.yaml"


_EMPTY_POLICY_TEMPLATE_ZH = """# -AI-Shell Security Policy

# - global: 全局默认行为配置
#   - default_risk_level: 未命中任何 rules 时的默认风险等级。
#     可选值通常为：LOW | MEDIUM | HIGH（具体以实现侧枚举为准）。
#   - enable_sandbox: 是否启用沙箱预跑/受控执行（如实现侧支持，可映射到对应开关）。
#   - sandbox_off_action: 当沙箱关闭/不可用/执行失败时，无法根据 rules 评估命令的实际文件变更，
#     将使用该动作作为兜底决策：BLOCK=阻断，CONFIRM=确认，ALLOW=直接放行。
#
# - rules: 规则列表（自上而下匹配，命中第一条即生效）
#   每条规则字段：
#   - id: 规则唯一标识（建议稳定且可追踪，如 H-001）。
#   - name: 规则名称（用于展示/日志）。
#   - path: 资源路径模式列表。
#     - 支持绝对路径；支持通配符 "*"（单级）与 "**"（递归）。
#     - 目录语义示例："/boot"、"/boot/*"、"/boot/**"。
#   - exclude:（可选）排除路径模式列表；命中 exclude 的路径不适用该规则。
#   - operations: 允许该规则匹配的操作类型列表。
#     典型值：WRITE | DELETE（具体以实现侧枚举为准）。
#   - risk: 该规则对应的风险等级：LOW | MEDIUM | HIGH。
#   - reason: 风险原因说明（用于提示用户）。
#   - confirm_message:（可选）当该规则命中且需要二次确认时展示的提示文案。
#   - suggestion:（可选）建议/替代方案（可使用多行文本 "|"）。

# 资源对象（path）规范：
# 1) 路径格式：仅支持绝对路径；支持通配符 "*" 和 "**"。
# 2) 目录区分：
#    - "/boot"   ：目录本身
#    - "/boot/*" ：目录下一级内容
#    - "/boot/**":目录下递归所有内容
# 3) 变量：支持在路径中使用环境/命令变量（由实现侧在加载或匹配时展开）。例如：
#    - "$PWD"、"$HOME"、"$(uname -r)"
# 4) 排除规则：通过 exclude 字段排除特定路径（同样支持通配符与变量）。

# 全局配置
global:
    # 未命中任何规则时的默认风险
    default_risk_level: LOW

    # 是否开启沙箱预跑（若当前实现已支持，可映射到现有 enable_sandbox 逻辑）
    enable_sandbox: false

    # 沙箱关闭/失败时的兜底动作（BLOCK=阻断，CONFIRM=确认，ALLOW=直接放行）
    sandbox_off_action: ALLOW

# 规则列表：从上到下匹配，第一条命中生效
rules:
    # ===== 高风险：系统核心目录，禁止 AI 修改 =====
    - id: H-001
      name: "系统配置目录保护"
      path: ["/etc/**"]
      operations: [WRITE, DELETE]
      risk: HIGH
      reason: "系统配置目录，误修改会导致严重故障"
      suggestion: |
        如确需修改 /etc 下文件，建议由人工完成变更并使用变更管理/备份策略。


    # ===== 中风险： /home目录 =====
    - id: M-001
      name: "家目录保护"
      path: ["/home/**"]
      operations: [WRITE, DELETE]
      risk: MEDIUM
      reason: "测试目录"
      confirm_message: "将对 /home/ 下文件执行写入/删除操作，是否继续？"

    # ===== 低风险：工作区 =====
    - id: L-001
      name: "临时区可写"
      path: ["/tmp/**"]
      operations: [WRITE, DELETE]
      risk: LOW
      reason: "临时区代码和项目文件，允许 AI 修改"
"""


_EMPTY_POLICY_TEMPLATE_EN = """# -AI-Shell Security Policy

# - global: global default behavior
#   - default_risk_level: default risk level when no rules match.
#     Common values: LOW | MEDIUM | HIGH (see the implementation enum).
#   - enable_sandbox: enable sandbox pre-run/controlled execution (if supported by the implementation).
#   - sandbox_off_action: when sandbox is disabled/unavailable/failed, rules cannot be evaluated (no file change list).
#     This action is used as a fallback decision: BLOCK, CONFIRM, ALLOW.
#
# - rules: rule list (top-down match; first match wins)
#   Fields:
#   - id: stable unique identifier (e.g. H-001)
#   - name: rule name (for UI/logs)
#   - path: list of path patterns
#     - absolute paths only; supports "*" (single level) and "**" (recursive)
#     - directory semantics examples: "/boot", "/boot/*", "/boot/**"
#   - exclude: (optional) list of exclude patterns; excluded paths do not apply to this rule
#   - operations: allowed operation types for this rule
#     Typical: WRITE | DELETE (see the implementation enum)
#   - risk: risk level: LOW | MEDIUM | HIGH
#   - reason: explanation shown to the user
#   - confirm_message: (optional) message shown when confirmation is required
#   - suggestion: (optional) suggestion/alternatives (multi-line with "|")

# Path pattern notes:
# 1) Only absolute paths are supported; wildcards "*" and "**" are allowed.
# 2) Directory semantics:
#    - "/boot"    : the directory itself
#    - "/boot/*"  : one-level children
#    - "/boot/**" : all descendants recursively
# 3) Variables: environment/command variables can be used and will be expanded by the implementation.
#    Examples: "$PWD", "$HOME", "$(uname -r)"
# 4) Exclusions: use exclude to carve out specific paths (also supports wildcards and variables).

# Global config
global:
    # Default risk when no rules match
    default_risk_level: LOW

    # Enable sandbox pre-run (if supported)
    enable_sandbox: false

    # Fallback action when sandbox is unavailable (BLOCK, CONFIRM, ALLOW)
    sandbox_off_action: ALLOW

# Rules: top-down match, first match wins
rules:
    # ===== High risk: critical system directories =====
    - id: H-001
      name: "Protect system configuration"
      path: ["/etc/**"]
      operations: [WRITE, DELETE]
      risk: HIGH
      reason: "System configuration directory; incorrect changes may cause serious failures"
      suggestion: |
        If you must modify files under /etc, do it manually and use change management and backups.


    # ===== Medium risk: user home directories =====
    - id: M-001
      name: "Protect home directories"
      path: ["/home/**"]
      operations: [WRITE, DELETE]
      risk: MEDIUM
      reason: "User data directory"
      confirm_message: "This will write/delete files under /home/. Continue?"

    # ===== Low risk: temporary directories =====
    - id: L-001
      name: "Allow temporary directories"
      path: ["/tmp/**"]
      operations: [WRITE, DELETE]
      risk: LOW
      reason: "Temporary directory; allow AI to modify files"
"""


def _get_empty_policy_template() -> str:
    return (
        _EMPTY_POLICY_TEMPLATE_ZH
        if get_ui_locale() == "zh-CN"
        else _EMPTY_POLICY_TEMPLATE_EN
    )


def _ensure_user_policy_template(path: Path) -> None:
    """Create a minimal policy template if it doesn't exist."""

    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        # Use exclusive create to avoid clobbering an existing policy.
        with path.open("x", encoding="utf-8") as f:
            f.write(_get_empty_policy_template())
    except FileExistsError:
        return
    except Exception:
        # If creation fails (permissions, etc.), caller will fall back to defaults.
        return


def resolve_security_policy_path(config_path: Optional[Path] = None) -> Optional[Path]:
    """解析实际会使用的安全策略配置文件路径。

    - 默认路径：/etc/aish/security_policy.yaml
        - 若默认路径不存在，尝试使用用户配置目录下的策略文件：
            $XDG_CONFIG_HOME/aish/security_policy.yaml 或 ~/.config/aish/security_policy.yaml
        - 若用户配置目录下也不存在，则自动生成一个空模板策略文件

    返回：存在的策略文件路径；若不存在则返回 None。
    """

    # 1) Explicit path (highest priority)
    if config_path is not None:
        try:
            if config_path.exists():
                return config_path
        except Exception:
            # Fall through to standard resolution.
            pass

    # 2) System-wide policy
    system_path = Path("/etc/aish/security_policy.yaml")
    try:
        if system_path.exists():
            return system_path
    except Exception:
        pass

    # 3) Per-user policy (auto-create template if missing)
    user_path = _user_security_policy_path()
    try:
        if not user_path.exists():
            _ensure_user_policy_template(user_path)
        if user_path.exists():
            return user_path
    except Exception:
        return None

    return None


@dataclass(frozen=True)
class AuditConfig:
    enabled: bool = False
    log_path: Optional[str] = None


def _as_bool(value: Any, default: bool) -> bool:
    if value is None:
        return default
    return bool(value)


def _parse_risk(value: Any, default: RiskLevel = RiskLevel.LOW) -> RiskLevel:
    if value is None:
        return default
    try:
        return RiskLevel(str(value).upper())
    except Exception:
        return default


def _ensure_list(value: Any) -> list[Any]:
    if value is None:
        return []
    if isinstance(value, list):
        return value
    return [value]


def _upper_ops(ops: Iterable[Any]) -> set[str]:
    return {str(op).upper() for op in ops if op is not None}


def _normalize_commands(commands: Iterable[Any]) -> set[str]:
    normalized: set[str] = set()
    for command in commands:
        if command is None:
            continue
        text = str(command).strip()
        if not text:
            continue
        normalized.add(text)
    return normalized


def _parse_v2_rules(raw_rules: list[dict[str, Any]]) -> list[PolicyRule]:
    rules: list[PolicyRule] = []

    for item in raw_rules:
        try:
            patterns = [str(p) for p in _ensure_list(item.get("path")) if p is not None]
            if not patterns:
                continue

            risk = _parse_risk(item.get("risk"), default=RiskLevel.LOW)
            operations = _upper_ops(_ensure_list(item.get("operations")))
            command_list = _normalize_commands(_ensure_list(item.get("command_list")))
            exclude = [
                str(p) for p in _ensure_list(item.get("exclude")) if p is not None
            ]

            rule_id = item.get("id")
            name = item.get("name")
            reason = item.get("reason")
            confirm_message = item.get("confirm_message")
            suggestion = item.get("suggestion")
        except Exception:
            continue

        # v2 规则以 risk + operations 为核心。
        # TODO: 支持 READ 等其它操作的可靠观测与匹配。
        for pattern in patterns:
            rules.append(
                PolicyRule(
                    pattern=pattern,
                    risk=risk,
                    description=None,
                    operations=operations or None,
                    command_list=command_list or None,
                    exclude=exclude or None,
                    rule_id=str(rule_id) if rule_id is not None else None,
                    name=str(name) if name is not None else None,
                    reason=str(reason) if reason is not None else None,
                    confirm_message=(
                        str(confirm_message) if confirm_message is not None else None
                    ),
                    suggestion=str(suggestion) if suggestion is not None else None,
                )
            )

    return rules


def load_security_policy(config_path: Optional[Path] = None) -> SecurityPolicy:
    """加载安全策略配置。

    - 默认路径：/etc/aish/security_policy.yaml
    - 仅支持 v2（global + rules[].path/operations/exclude）。

    注意：v2 不强制要求 version 字段。
    """

    effective_path = resolve_security_policy_path(config_path=config_path)
    if effective_path is None:
        return SecurityPolicy.default()

    try:
        with effective_path.open("r", encoding="utf-8") as f:
            data = yaml.safe_load(f) or {}
    except Exception:
        return SecurityPolicy.default()

    global_cfg = data.get("global") if isinstance(data.get("global"), dict) else {}
    default_risk = _parse_risk(
        global_cfg.get("default_risk_level") if isinstance(global_cfg, dict) else None,
        default=RiskLevel.LOW,
    )
    raw_enable_sandbox = (
        global_cfg.get("enable_sandbox") if isinstance(global_cfg, dict) else None
    )
    if raw_enable_sandbox is None:
        enable_sandbox = False
    elif isinstance(raw_enable_sandbox, bool):
        enable_sandbox = raw_enable_sandbox
    else:
        enable_sandbox = False
        _LOGGER.warning(
            "security_policy: enable_sandbox must be boolean; treating as false"
        )

    raw_off_action = (
        global_cfg.get("sandbox_off_action") if isinstance(global_cfg, dict) else None
    )
    if raw_off_action is None:
        sandbox_off_action = SandboxOffAction.ALLOW
    else:
        try:
            sandbox_off_action = SandboxOffAction(str(raw_off_action).upper())
        except Exception:
            sandbox_off_action = SandboxOffAction.ALLOW
            _LOGGER.warning(
                "security_policy: invalid sandbox_off_action; falling back to ALLOW"
            )

    # Backward compatibility: support the old key sandbox_fallback_risk (LOW/MEDIUM/HIGH)
    # and map it to sandbox_off_action (ALLOW/CONFIRM/BLOCK).
    if (
        isinstance(global_cfg, dict)
        and global_cfg.get("sandbox_off_action") is None
        and global_cfg.get("sandbox_fallback_risk") is not None
    ):
        legacy_risk = _parse_risk(
            global_cfg.get("sandbox_fallback_risk"), default=RiskLevel.MEDIUM
        )
        if legacy_risk == RiskLevel.HIGH:
            sandbox_off_action = SandboxOffAction.BLOCK
        elif legacy_risk == RiskLevel.MEDIUM:
            sandbox_off_action = SandboxOffAction.CONFIRM
        else:
            sandbox_off_action = SandboxOffAction.ALLOW

    audit_cfg_raw = data.get("audit") if isinstance(data.get("audit"), dict) else {}
    audit_cfg = AuditConfig(
        enabled=_as_bool(
            audit_cfg_raw.get("enabled") if isinstance(audit_cfg_raw, dict) else None,
            default=False,
        ),
        log_path=(
            str(audit_cfg_raw.get("log_path"))
            if isinstance(audit_cfg_raw, dict)
            and audit_cfg_raw.get("log_path") is not None
            else None
        ),
    )

    raw_rules = data.get("rules", []) or []
    if not isinstance(raw_rules, list):
        raw_rules = []

    rules: list[PolicyRule] = []
    rules.extend(SecurityPolicy.default().rules)

    # 仅支持 v2：忽略未携带 path 的旧规则形态。
    v2_items = [r for r in raw_rules if isinstance(r, dict) and ("path" in r)]
    rules.extend(_parse_v2_rules(v2_items))

    return SecurityPolicy(
        enable_sandbox=enable_sandbox,
        rules=rules,
        sandbox_off_action=sandbox_off_action,
        default_risk_level=default_risk,
        audit_enabled=audit_cfg.enabled,
        audit_log_path=audit_cfg.log_path,
    )
