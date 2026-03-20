from __future__ import annotations

import json
import sys
from typing import Callable, Optional

from aish.i18n import t
from aish.tools.base import ToolBase
from aish.tools.result import ToolResult


class AskUserTool(ToolBase):
    """Ask the user to choose one option.

    Cancellation or unavailable interactive UI MUST pause the task and ask the user
    to decide how to proceed (manual selection or continue with default).
    """

    def __init__(
        self,
        request_choice: Callable[[dict], tuple[Optional[str], str]],
    ) -> None:
        super().__init__(
            name="ask_user",
            description=(
                "\n".join(
                    [
                        "Ask the user to choose one option from a list.",
                        "If the UI is unavailable or the user cancels, the task will pause and require user input.",
                    ]
                )
            ),
            parameters={
                "type": "object",
                "properties": {
                    "prompt": {
                        "type": "string",
                        "description": "Question/description shown to the user.",
                    },
                    "options": {
                        "type": "array",
                        "description": "List of options to choose from (must be non-empty).",
                        "items": {
                            "type": "object",
                            "properties": {
                                "value": {"type": "string"},
                                "label": {"type": "string"},
                                "description": {"type": "string"},
                            },
                            "required": ["value", "label"],
                        },
                        "minItems": 1,
                    },
                    "default": {
                        "type": "string",
                        "description": "Default option value used when user chooses to continue with default later.",
                    },
                    "title": {
                        "type": "string",
                        "description": "Optional UI title.",
                    },
                    "allow_cancel": {
                        "type": "boolean",
                        "description": "Whether user can cancel/ESC.",
                        "default": True,
                    },
                    "cancel_hint": {
                        "type": "string",
                        "description": "Optional custom hint shown when the tool pauses due to cancel/unavailable.",
                    },
                    "allow_custom_input": {
                        "type": "boolean",
                        "description": "Whether to allow the user to input a custom value.",
                        "default": False,
                    },
                    "custom_label": {
                        "type": "string",
                        "description": "Label for the custom input option (when allow_custom_input=true).",
                    },
                    "custom_prompt": {
                        "type": "string",
                        "description": "Prompt shown when asking for a custom input value.",
                    },
                },
                "required": ["prompt", "options"],
            },
        )
        self._request_choice = request_choice

    @staticmethod
    def _normalize_options(options: object) -> list[dict[str, str]]:
        if not isinstance(options, list):
            return []
        normalized: list[dict[str, str]] = []
        for item in options:
            if not isinstance(item, dict):
                continue
            value = item.get("value")
            label = item.get("label")
            if not isinstance(value, str) or not value.strip():
                continue
            if not isinstance(label, str) or not label.strip():
                continue
            normalized.append({"value": value.strip(), "label": label.strip()})
        return normalized

    @staticmethod
    def _pick_default(default: object, options: list[dict[str, str]]) -> str:
        if options:
            fallback = options[0]["value"]
        else:
            fallback = ""
        if isinstance(default, str) and default in {o["value"] for o in options}:
            return default
        return fallback

    def _build_pause_message(
        self,
        *,
        prompt: str,
        options: list[dict[str, str]],
        default_value: str,
        reason: str,
        cancel_hint: str | None,
        allow_custom_input: bool,
    ) -> str:
        lines: list[str] = []
        lines.append(t("shell.ask_user.paused.title"))
        lines.append(t("shell.ask_user.paused.prompt", prompt=prompt))
        lines.append(t("shell.ask_user.paused.reason", reason=reason))
        lines.append(t("shell.ask_user.paused.options_header"))
        for idx, opt in enumerate(options, start=1):
            lines.append(f"  {idx}. {opt['label']} ({opt['value']})")
        if allow_custom_input:
            lines.append(t("shell.ask_user.paused.custom_input"))
        if cancel_hint and str(cancel_hint).strip():
            lines.append("")
            lines.append(str(cancel_hint).strip())
        lines.append("")
        lines.append(
            t(
                "shell.ask_user.paused.how_to",
                default=default_value,
            )
        )
        lines.append("")
        # Include structured context to help the model reliably continue on the next user turn.
        context = {
            "kind": "ask_user_context",
            "prompt": prompt,
            "default": default_value,
            "options": options,
            "suggested_continue_commands": [
                "; continue with default",
                "; 使用默认继续",
            ],
        }
        lines.append("```json")
        lines.append(json.dumps(context, ensure_ascii=False))
        lines.append("```")
        return "\n".join(lines).strip()

    def __call__(
        self,
        prompt: str,
        options: list[dict],
        default: str | None = None,
        title: str | None = None,
        allow_cancel: bool = True,
        cancel_hint: str | None = None,
        allow_custom_input: bool = False,
        custom_label: str | None = None,
        custom_prompt: str | None = None,
    ) -> ToolResult:
        normalized_options = self._normalize_options(options)
        if not normalized_options:
            return ToolResult(
                ok=False,
                output="Error: options must be a non-empty list of {value,label}.",
                meta={"kind": "invalid_args"},
            )

        default_value = self._pick_default(default, normalized_options)

        # Fast fail for non-interactive environments.
        if not (sys.stdin.isatty() and sys.stdout.isatty()):
            reason = "unavailable"
            pause_text = self._build_pause_message(
                prompt=prompt,
                options=normalized_options,
                default_value=default_value,
                reason=reason,
                cancel_hint=cancel_hint,
                allow_custom_input=allow_custom_input,
            )
            return ToolResult(
                ok=False,
                output=pause_text,
                meta={
                    "kind": "user_input_required",
                    "reason": reason,
                    "prompt": prompt,
                    "default": default_value,
                    "options": normalized_options,
                },
                stop_tool_chain=True,
            )

        try:
            selected, status = self._request_choice(
                {
                    "prompt": prompt,
                    "options": normalized_options,
                    "default": default_value,
                    "title": title,
                    "allow_cancel": bool(allow_cancel),
                    "allow_custom_input": bool(allow_custom_input),
                    "custom_label": custom_label,
                    "custom_prompt": custom_prompt,
                }
            )
        except KeyboardInterrupt:
            raise
        except Exception:
            selected, status = None, "error"

        allowed_values = {o["value"] for o in normalized_options}
        if isinstance(selected, str) and selected in allowed_values:
            label_lookup = {o["value"]: o["label"] for o in normalized_options}
            label = label_lookup.get(selected, selected)
            # Return a clear, LLM-friendly message instead of JSON
            # Store the structured data in the data field for potential future use
            return ToolResult(
                ok=True,
                output=f"User selected: {label}",
                data={
                    "value": selected,
                    "label": label,
                    "status": "selected",
                },
            )
        if (
            allow_custom_input
            and isinstance(selected, str)
            and selected.strip()
            and selected not in allowed_values
        ):
            return ToolResult(
                ok=True,
                output=f"User input: {selected}",
                data={
                    "value": selected,
                    "label": selected,
                    "status": "custom",
                },
            )

        reason = (
            "cancelled"
            if status == "cancelled"
            else ("unavailable" if status == "unavailable" else "error")
        )
        pause_text = self._build_pause_message(
            prompt=prompt,
            options=normalized_options,
            default_value=default_value,
            reason=reason,
            cancel_hint=cancel_hint,
            allow_custom_input=allow_custom_input,
        )
        return ToolResult(
            ok=False,
            output=pause_text,
            meta={
                "kind": "user_input_required",
                "reason": reason,
                "prompt": prompt,
                "default": default_value,
                "options": normalized_options,
            },
            stop_tool_chain=True,
        )
