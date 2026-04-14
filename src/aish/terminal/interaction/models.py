from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import Any


class InteractionKind(str, Enum):
    TEXT_INPUT = "text_input"
    CHOICE_OR_TEXT = "choice_or_text"
    PLAN_APPROVAL = "plan_approval"
    CONFIRM = "confirm"


class InteractionStatus(str, Enum):
    SUBMITTED = "submitted"
    CANCELLED = "cancelled"
    DISMISSED = "dismissed"
    UNAVAILABLE = "unavailable"


class InteractionAnswerType(str, Enum):
    OPTION = "option"
    TEXT = "text"
    CONFIRM = "confirm"


@dataclass(frozen=True)
class InteractionSource:
    type: str
    name: str

    def to_dict(self) -> dict[str, str]:
        return {"type": self.type, "name": self.name}

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "InteractionSource":
        return cls(
            type=str(data.get("type") or "tool"),
            name=str(data.get("name") or "ask_user"),
        )


@dataclass(frozen=True)
class InteractionOption:
    value: str
    label: str
    description: str | None = None

    def to_dict(self) -> dict[str, str]:
        item = {"value": self.value, "label": self.label}
        if self.description:
            item["description"] = self.description
        return item

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "InteractionOption":
        description = data.get("description")
        return cls(
            value=str(data.get("value") or ""),
            label=str(data.get("label") or ""),
            description=str(description) if isinstance(description, str) else None,
        )


@dataclass(frozen=True)
class InteractionValidation:
    required: bool = True
    min_length: int | None = None

    def to_dict(self) -> dict[str, Any]:
        data: dict[str, Any] = {"required": self.required}
        if self.min_length is not None:
            data["min_length"] = self.min_length
        return data

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "InteractionValidation":
        min_length = data.get("min_length")
        return cls(
            required=bool(data.get("required", True)),
            min_length=min_length if isinstance(min_length, int) else None,
        )


@dataclass(frozen=True)
class InteractionCustomConfig:
    label: str
    placeholder: str | None = None
    submit_mode: str = "inline"

    def to_dict(self) -> dict[str, Any]:
        data: dict[str, Any] = {
            "label": self.label,
            "submit_mode": self.submit_mode,
        }
        if self.placeholder is not None:
            data["placeholder"] = self.placeholder
        return data

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "InteractionCustomConfig":
        placeholder = data.get("placeholder")
        return cls(
            label=str(data.get("label") or ""),
            placeholder=str(placeholder) if isinstance(placeholder, str) else None,
            submit_mode=str(data.get("submit_mode") or "inline"),
        )


@dataclass(frozen=True)
class InteractionAnswer:
    type: InteractionAnswerType
    value: str
    label: str | None = None

    def to_dict(self) -> dict[str, Any]:
        data: dict[str, Any] = {
            "type": self.type.value,
            "value": self.value,
        }
        if self.label is not None:
            data["label"] = self.label
        return data

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "InteractionAnswer":
        label = data.get("label")
        return cls(
            type=InteractionAnswerType(
                str(data.get("type") or InteractionAnswerType.TEXT.value)
            ),
            value=str(data.get("value") or ""),
            label=str(label) if isinstance(label, str) else None,
        )


@dataclass(frozen=True)
class InteractionRequest:
    id: str
    kind: InteractionKind
    prompt: str
    title: str | None = None
    required: bool = True
    allow_cancel: bool = True
    source: InteractionSource = field(
        default_factory=lambda: InteractionSource(type="tool", name="ask_user")
    )
    metadata: dict[str, Any] = field(default_factory=dict)
    options: list[InteractionOption] = field(default_factory=list)
    default: str | None = None
    placeholder: str | None = None
    validation: InteractionValidation | None = None
    custom: InteractionCustomConfig | None = None

    def get_option_by_value(self, value: str) -> InteractionOption | None:
        for option in self.options:
            if option.value == value:
                return option
        return None

    def to_dict(self) -> dict[str, Any]:
        data: dict[str, Any] = {
            "id": self.id,
            "kind": self.kind.value,
            "prompt": self.prompt,
            "required": self.required,
            "allow_cancel": self.allow_cancel,
            "source": self.source.to_dict(),
            "metadata": dict(self.metadata),
            "options": [option.to_dict() for option in self.options],
        }
        if self.title is not None:
            data["title"] = self.title
        if self.default is not None:
            data["default"] = self.default
        if self.placeholder is not None:
            data["placeholder"] = self.placeholder
        if self.validation is not None:
            data["validation"] = self.validation.to_dict()
        if self.custom is not None:
            data["custom"] = self.custom.to_dict()
        return data

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "InteractionRequest":
        source_data = data.get("source") if isinstance(data.get("source"), dict) else {}
        validation_data = (
            data.get("validation") if isinstance(data.get("validation"), dict) else None
        )
        custom_data = data.get("custom") if isinstance(data.get("custom"), dict) else None
        raw_options = data.get("options")
        options_data: list[dict[str, Any]] = (
            [option for option in raw_options if isinstance(option, dict)]
            if isinstance(raw_options, list)
            else []
        )
        return cls(
            id=str(data.get("id") or ""),
            kind=InteractionKind(
                str(data.get("kind") or InteractionKind.CHOICE_OR_TEXT.value)
            ),
            prompt=str(data.get("prompt") or ""),
            title=str(data.get("title")) if isinstance(data.get("title"), str) else None,
            required=bool(data.get("required", True)),
            allow_cancel=bool(data.get("allow_cancel", True)),
            source=InteractionSource.from_dict(source_data),
            metadata=dict(data.get("metadata") or {}),
            options=[
                InteractionOption.from_dict(option) for option in options_data
            ],
            default=str(data.get("default")) if isinstance(data.get("default"), str) else None,
            placeholder=str(data.get("placeholder")) if isinstance(data.get("placeholder"), str) else None,
            validation=InteractionValidation.from_dict(validation_data)
            if validation_data is not None
            else None,
            custom=InteractionCustomConfig.from_dict(custom_data)
            if custom_data is not None
            else None,
        )


@dataclass(frozen=True)
class InteractionResponse:
    interaction_id: str
    status: InteractionStatus
    answer: InteractionAnswer | None = None
    reason: str | None = None
    metadata: dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        data: dict[str, Any] = {
            "interaction_id": self.interaction_id,
            "status": self.status.value,
            "metadata": dict(self.metadata),
        }
        if self.answer is not None:
            data["answer"] = self.answer.to_dict()
        if self.reason is not None:
            data["reason"] = self.reason
        return data

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "InteractionResponse":
        answer_data = data.get("answer") if isinstance(data.get("answer"), dict) else None
        return cls(
            interaction_id=str(data.get("interaction_id") or ""),
            status=InteractionStatus(
                str(data.get("status") or InteractionStatus.DISMISSED.value)
            ),
            answer=InteractionAnswer.from_dict(answer_data) if answer_data is not None else None,
            reason=str(data.get("reason")) if isinstance(data.get("reason"), str) else None,
            metadata=dict(data.get("metadata") or {}),
        )