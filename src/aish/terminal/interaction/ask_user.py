from __future__ import annotations

import uuid

from aish.i18n import t
from aish.tools.result import ToolResult

from .models import (
    InteractionAnswerType,
    InteractionCustomConfig,
    InteractionKind,
    InteractionOption,
    InteractionRequest,
    InteractionResponse,
    InteractionSource,
    InteractionStatus,
    InteractionValidation,
)


class AskUserRequestBuilder:
    @staticmethod
    def pick_text_default(default: object) -> str | None:
        if isinstance(default, str):
            return default
        return None

    @staticmethod
    def normalize_options(options: object) -> list[InteractionOption]:
        if not isinstance(options, list):
            return []

        normalized: list[InteractionOption] = []
        for item in options:
            if not isinstance(item, dict):
                continue
            value = item.get("value")
            label = item.get("label")
            if not isinstance(value, str) or not value.strip():
                continue
            if not isinstance(label, str) or not label.strip():
                continue
            description = item.get("description")
            normalized.append(
                InteractionOption(
                    value=value.strip(),
                    label=label.strip(),
                    description=description.strip()
                    if isinstance(description, str) and description.strip()
                    else None,
                )
            )
        return normalized

    @staticmethod
    def pick_default(default: object, options: list[InteractionOption]) -> str:
        fallback = options[0].value if options else ""
        if isinstance(default, str) and default in {option.value for option in options}:
            return default
        return fallback

    @classmethod
    def from_tool_args(
        cls,
        *,
        kind: str,
        prompt: str,
        options: object = None,
        default: str | None = None,
        title: str | None = None,
        required: bool = True,
        allow_cancel: bool = True,
        metadata: object = None,
        placeholder: str | None = None,
        validation: object = None,
        custom: object = None,
        interaction_id: str | None = None,
    ) -> InteractionRequest:
        interaction_kind = InteractionKind(kind)
        normalized_options = cls.normalize_options(options)
        default_value = cls.pick_default(default, normalized_options)
        if interaction_kind == InteractionKind.TEXT_INPUT:
            default_value = cls.pick_text_default(default)

        request_metadata = dict(metadata) if isinstance(metadata, dict) else {}

        request_placeholder = (
            placeholder.strip()
            if isinstance(placeholder, str) and placeholder.strip()
            else None
        )

        request_validation = (
            InteractionValidation.from_dict(validation)
            if isinstance(validation, dict)
            else None
        )

        request_custom = None
        if interaction_kind == InteractionKind.CHOICE_OR_TEXT:
            custom_payload = custom if isinstance(custom, dict) else {}
            label = str(
                custom_payload.get("label") or t("shell.ask_user.custom_label")
            ).strip()
            custom_placeholder = custom_payload.get("placeholder")
            request_custom = InteractionCustomConfig(
                label=label,
                placeholder=(
                    str(custom_placeholder).strip()
                    if isinstance(custom_placeholder, str) and custom_placeholder.strip()
                    else request_placeholder
                ),
                submit_mode=str(custom_payload.get("submit_mode") or "inline"),
            )
        elif interaction_kind == InteractionKind.TEXT_INPUT:
            request_custom = None
            if request_validation is None:
                request_validation = InteractionValidation(required=required, min_length=1)

        if interaction_kind == InteractionKind.CHOICE_OR_TEXT:
            request_placeholder = None

        return InteractionRequest(
            id=interaction_id or f"interaction_{uuid.uuid4().hex[:12]}",
            kind=interaction_kind,
            title=title,
            prompt=prompt,
            required=bool(required),
            allow_cancel=bool(allow_cancel),
            source=InteractionSource(type="tool", name="ask_user"),
            metadata=request_metadata,
            options=normalized_options,
            default=default_value,
            placeholder=request_placeholder,
            validation=request_validation,
            custom=request_custom,
        )

class AskUserInteractionAdapter:
    @staticmethod
    def to_tool_result(
        request: InteractionRequest,
        response: InteractionResponse,
    ) -> ToolResult:
        if response.status == InteractionStatus.SUBMITTED and response.answer is not None:
            if response.answer.type == InteractionAnswerType.OPTION:
                label = response.answer.label or response.answer.value
                return ToolResult(
                    ok=True,
                    output=f"User selected: {label}",
                    data={
                        "value": response.answer.value,
                        "label": label,
                        "status": "selected",
                        "interaction_id": response.interaction_id,
                        "answer_type": response.answer.type.value,
                    },
                    meta={
                        "interaction_id": response.interaction_id,
                        "interaction_status": response.status.value,
                    },
                )
            if response.answer.type == InteractionAnswerType.TEXT:
                return ToolResult(
                    ok=True,
                    output=f"User input: {response.answer.value}",
                    data={
                        "value": response.answer.value,
                        "label": response.answer.label or response.answer.value,
                        "status": "custom",
                        "interaction_id": response.interaction_id,
                        "answer_type": response.answer.type.value,
                    },
                    meta={
                        "interaction_id": response.interaction_id,
                        "interaction_status": response.status.value,
                    },
                )

        reason = response.reason or response.status.value
        pause_text = AskUserInteractionAdapter.build_pause_message(
            request=request,
            reason=reason,
        )
        return ToolResult(
            ok=False,
            output=pause_text,
            meta={
                "kind": "user_input_required",
                "reason": reason,
                "prompt": request.prompt,
                "default": request.default,
                "options": [option.to_dict() for option in request.options],
                "interaction_id": response.interaction_id,
                "interaction_status": response.status.value,
            },
            stop_tool_chain=True,
        )

    @staticmethod
    def build_pause_message(*, request: InteractionRequest, reason: str) -> str:
        lines: list[str] = []
        lines.append(t("shell.ask_user.paused.title"))
        lines.append(t("shell.ask_user.paused.prompt", prompt=request.prompt))
        lines.append(t("shell.ask_user.paused.reason", reason=reason))
        lines.append(t("shell.ask_user.paused.options_header"))
        for index, option in enumerate(request.options, start=1):
            lines.append(f"  {index}. {option.label} ({option.value})")
            if option.description:
                lines.append(f"     {option.description}")
        if request.kind in (InteractionKind.CHOICE_OR_TEXT, InteractionKind.TEXT_INPUT):
            lines.append(t("shell.ask_user.paused.custom_input"))
        cancel_hint = request.metadata.get("cancel_hint")
        if isinstance(cancel_hint, str) and cancel_hint.strip():
            lines.append("")
            lines.append(cancel_hint.strip())
        lines.append("")
        lines.append(
            t(
                "shell.ask_user.paused.how_to",
                default=request.default or "",
            )
        )
        return "\n".join(lines).strip()


def apply_interaction_response_to_data(
    data: dict[str, object],
    response: InteractionResponse,
) -> None:
    data["interaction_response"] = response.to_dict()
    data.pop("selected_value", None)
    data.pop("custom_input", None)