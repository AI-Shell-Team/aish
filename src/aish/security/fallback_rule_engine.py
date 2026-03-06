from __future__ import annotations

from dataclasses import dataclass
from fnmatch import fnmatch
from pathlib import PurePosixPath
from typing import Iterable, Optional
import os
import shlex

from aish.i18n import t

from .sandbox import strip_sudo_prefix
from .security_policy import PolicyRule, RiskLevel, SecurityPolicy


_SHELL_WRAPPERS = {"bash", "sh"}
_WRAPPER_OPTIONS = {"-c", "-lc", "-cl"}
_CONTROL_TOKENS = {";", "&&", "||", "|", "&", "(", ")", "<", ">", ">>", "<<"}
_META_CHARS = set("$~`")


@dataclass(frozen=True)
class FallbackRuleAssessment:
    level: RiskLevel
    reasons: list[str]
    matched_paths: list[str]
    matched_rules: list[PolicyRule]
    primary_rule: PolicyRule


@dataclass(frozen=True)
class _ParsedDeleteCommand:
    command_name: str
    paths: list[str]


class FallbackRuleEngine:
    def __init__(self, policy: SecurityPolicy) -> None:
        self._policy = policy

    def assess_disabled_command(self, command: str) -> Optional[FallbackRuleAssessment]:
        parsed = self._parse_delete_command(command)
        if parsed is None:
            return None

        hits: list[tuple[PolicyRule, str]] = []
        for path in parsed.paths:
            rule = self._match_rule(command_name=parsed.command_name, path=path)
            if rule is not None:
                hits.append((rule, path))

        if not hits:
            return None

        high_hits = [(rule, path) for rule, path in hits if rule.risk == RiskLevel.HIGH]
        medium_hits = [(rule, path) for rule, path in hits if rule.risk == RiskLevel.MEDIUM]
        low_hits = [(rule, path) for rule, path in hits if rule.risk == RiskLevel.LOW]

        if high_hits:
            selected_hits = high_hits
            level = RiskLevel.HIGH
        elif medium_hits:
            selected_hits = medium_hits
            level = RiskLevel.MEDIUM
        else:
            selected_hits = low_hits
            level = RiskLevel.LOW

        preview_paths = ", ".join(path for _rule, path in selected_hits[:3])
        reasons = [
            t(
                "security.risk_reason.policy_fallback_rule_match",
                command=parsed.command_name,
                count=len(selected_hits),
                risk=level.value,
            )
        ]
        if preview_paths:
            reasons.append(t("security.ai_risk.preview_paths", paths=preview_paths))

        return FallbackRuleAssessment(
            level=level,
            reasons=reasons,
            matched_paths=[path for _rule, path in selected_hits],
            matched_rules=[rule for rule, _path in selected_hits],
            primary_rule=selected_hits[0][0],
        )

    def _parse_delete_command(self, command: str) -> Optional[_ParsedDeleteCommand]:
        stripped_command, _sudo_detected, ok = strip_sudo_prefix(command)
        if not ok:
            return None

        argv = self._split_shell_like(stripped_command)
        if not argv:
            return None

        wrapper_script = self._extract_wrapper_script(argv)
        if wrapper_script is not None:
            argv = self._split_simple_script(wrapper_script)
            if not argv:
                return None

        command_name = self._normalize_command_name(argv[0])
        if command_name is None:
            return None

        supported_commands = self._get_policy_command_list()
        if not supported_commands or command_name not in supported_commands:
            return None

        paths = self._extract_paths(argv)
        if not paths:
            return None

        return _ParsedDeleteCommand(command_name=command_name, paths=paths)

    def _get_policy_command_list(self) -> set[str]:
        commands: set[str] = set()
        for rule in self._policy.rules:
            if not rule.command_list:
                continue
            for command in rule.command_list:
                name = self._normalize_command_name(command)
                if name:
                    commands.add(name)
        return commands

    def _extract_wrapper_script(self, argv: list[str]) -> Optional[str]:
        command_name = self._normalize_command_name(argv[0])
        if command_name not in _SHELL_WRAPPERS:
            return None

        idx = 1
        while idx < len(argv):
            arg = argv[idx]
            if arg in _WRAPPER_OPTIONS:
                return argv[idx + 1] if idx + 1 < len(argv) else None
            idx += 1

        return None

    def _split_shell_like(self, command: str) -> Optional[list[str]]:
        try:
            argv = shlex.split(command, posix=True)
        except ValueError:
            return None
        return argv or None

    def _split_simple_script(self, script: str) -> Optional[list[str]]:
        lexer = shlex.shlex(script, posix=True, punctuation_chars=True)
        lexer.whitespace_split = True
        try:
            tokens = list(lexer)
        except ValueError:
            return None

        if not tokens:
            return None
        if any(token in _CONTROL_TOKENS for token in tokens):
            return None
        return tokens

    def _extract_paths(self, argv: list[str]) -> list[str]:
        if not argv:
            return []

        paths: list[str] = []
        options_ended = False
        for token in argv[1:]:
            if not options_ended and token == "--":
                options_ended = True
                continue
            if not options_ended and token.startswith("-"):
                continue
            if self._is_explicit_absolute_path(token):
                paths.append(self._normalize_path(token))

        return paths

    def _match_rule(self, *, command_name: str, path: str) -> Optional[PolicyRule]:
        for rule in self._policy.rules:
            if not self._command_in_rule(command_name, rule.command_list):
                continue
            if not self._path_matches(path, rule.pattern):
                continue
            if rule.exclude and any(self._path_matches(path, ex) for ex in rule.exclude):
                continue
            return rule

        return None

    def _path_matches(self, path: str, pattern: str) -> bool:
        if fnmatch(path, pattern):
            return True
        if pattern.endswith("/**"):
            prefix = pattern[:-3]
            return path == prefix or path.startswith(prefix + "/")
        return False

    def _normalize_command_name(self, value: str) -> Optional[str]:
        text = (value or "").strip()
        if not text:
            return None
        return os.path.basename(text).lower()

    def _command_in_rule(self, command_name: str, command_list: Optional[set[str]]) -> bool:
        if not command_list:
            return False
        normalized = {self._normalize_command_name(command) for command in command_list}
        return command_name in {item for item in normalized if item}

    def _normalize_path(self, value: str) -> str:
        path = PurePosixPath(value)
        return str(path)

    def _is_explicit_absolute_path(self, token: str) -> bool:
        if not token or not token.startswith("/"):
            return False
        if any(ch in token for ch in _META_CHARS):
            return False
        return True


__all__ = ["FallbackRuleAssessment", "FallbackRuleEngine"]