from __future__ import annotations

import io
import time
from unittest.mock import patch

from rich.console import Console

from aish.llm import LLMCallbackResult, LLMEvent, LLMEventType
from aish.shell_enhanced.shell_prompt_io import display_security_panel, handle_ask_user_required


class _DummyShell:
    def __init__(self) -> None:
        self.current_live = None
        self.console = Console(file=io.StringIO(), force_terminal=False, width=120)

    def _stop_animation(self) -> None:
        return

    def _finalize_content_preview(self) -> None:
        return

    def _compute_ask_user_max_visible(
        self,
        total_options: int,
        term_rows: int,
        allow_custom_input: bool,
        max_visible_cap: int = 12,
    ) -> int:
        _ = term_rows, allow_custom_input, max_visible_cap
        return max(1, min(total_options, 3))

    def _read_terminal_size(self) -> tuple[int, int]:
        return (24, 80)

    def _is_ui_resize_enabled(self) -> bool:
        return False


def test_handle_ask_user_required_sets_selected_value():
    shell = _DummyShell()
    event = LLMEvent(
        event_type=LLMEventType.ASK_USER_REQUIRED,
        data={
            "prompt": "Pick one",
            "options": [
                {"value": "opt1", "label": "Option 1"},
                {"value": "opt2", "label": "Option 2"},
            ],
            "default": "opt1",
            "allow_cancel": True,
            "allow_custom_input": True,
            "custom_prompt": "This is intentionally very long to avoid squeezing input space",
        },
        timestamp=time.time(),
    )

    class _DummyApp:
        def __init__(self, *args, **kwargs) -> None:
            class _Input:
                @staticmethod
                def flush() -> None:
                    return

                @staticmethod
                def flush_keys() -> None:
                    return

            self.input = _Input()

        def run(self, in_thread: bool = True) -> str:
            _ = in_thread
            return "opt2"

    with patch("prompt_toolkit.Application", _DummyApp):
        result = handle_ask_user_required(shell, event)

    assert result == LLMCallbackResult.CONTINUE
    assert event.data.get("selected_value") == "opt2"


def test_display_security_panel_shows_fallback_rule_details():
    shell = _DummyShell()

    display_security_panel(
        shell,
        {
            "tool_name": "bash_exec",
            "command": "sudo rm /etc/aish/123",
            "security_analysis": {
                "risk_level": "HIGH",
                "sandbox": {"enabled": False, "reason": "sandbox_disabled_by_policy"},
                "fallback_rule_matched": True,
                "matched_rule": {"id": "H-001", "name": "系统配置目录保护"},
                "matched_paths": ["/etc/aish/123"],
                "reasons": ["系统配置目录，误修改会导致严重故障"],
                "impact_description": "系统配置目录，误修改会导致严重故障",
                "suggested_alternatives": ["如确需修改 /etc 下文件，建议由人工完成变更。"],
            },
        },
        panel_mode="blocked",
    )

    output = shell.console.file.getvalue()
    assert "风险等级" in output
    assert "原因" in output
    assert "系统配置目录，误修改会导致严重故障" in output


def test_display_security_panel_for_fallback_rule_confirm_hides_generic_fallback_hint():
    shell = _DummyShell()

    display_security_panel(
        shell,
        {
            "tool_name": "bash_exec",
            "command": "rm -rf /home/lixin/123",
            "security_analysis": {
                "risk_level": "MEDIUM",
                "sandbox": {"enabled": False, "reason": "sandbox_disabled_by_policy"},
                "fallback_rule_matched": True,
                "reasons": ["用户业务数据变更需人工确认"],
            },
        },
        panel_mode="confirm",
    )

    output = shell.console.file.getvalue()
    assert "用户业务数据变更需人工确认" in output
    assert "未能完成命令风险评估" not in output

