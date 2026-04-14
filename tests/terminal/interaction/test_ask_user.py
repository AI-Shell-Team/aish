from __future__ import annotations

from aish.terminal.interaction import (
    AskUserRequestBuilder,
    AskUserInteractionAdapter,
    InteractionAnswer,
    InteractionAnswerType,
    InteractionKind,
    InteractionRequest,
    InteractionResponse,
    InteractionService,
    InteractionStatus,
    apply_interaction_response_to_data,
)


def test_ask_user_request_builder_builds_choice_or_text_request():
    request = AskUserRequestBuilder.from_tool_args(
        kind="choice_or_text",
        prompt="pick one",
        options=[
            {"value": "a", "label": "A", "description": "Alpha option"},
            {"value": "b", "label": "B"},
        ],
        default="missing",
        custom={"label": "Other", "placeholder": "Type here"},
        metadata={"cancel_hint": "custom cancel hint"},
    )

    assert request.kind == InteractionKind.CHOICE_OR_TEXT
    assert request.default == "a"
    assert request.custom is not None
    assert request.custom.label == "Other"
    assert request.custom.placeholder == "Type here"
    assert request.options[0].description == "Alpha option"
    assert request.metadata["cancel_hint"] == "custom cancel hint"


def test_ask_user_request_builder_builds_choice_or_text_with_default():
    request = AskUserRequestBuilder.from_tool_args(
        kind="choice_or_text",
        prompt="pick one",
        options=[
            {"value": "a", "label": "A"},
            {"value": "b", "label": "B"},
        ],
        default="b",
        placeholder="ignored",
        custom={"label": "Other", "placeholder": "Should not appear"},
    )

    assert request.kind == InteractionKind.CHOICE_OR_TEXT
    assert request.default == "b"
    assert request.custom is not None
    assert request.placeholder is None


def test_ask_user_request_builder_builds_text_input_request():
    request = AskUserRequestBuilder.from_tool_args(
        kind="text_input",
        prompt="type a fruit",
        placeholder="Enter fruit name",
        default="dragonfruit",
    )

    assert request.kind == InteractionKind.TEXT_INPUT
    assert request.options == []
    assert request.custom is None
    assert request.default == "dragonfruit"
    assert request.placeholder == "Enter fruit name"
    assert request.validation is not None
    assert request.validation.required is True
    assert request.validation.min_length == 1


def test_apply_interaction_response_writes_standard_payload_only():
    data: dict[str, object] = {
        "selected_value": "stale",
        "custom_input": "stale",
    }
    response = InteractionResponse(
        interaction_id="interaction_1",
        status=InteractionStatus.SUBMITTED,
        answer=InteractionAnswer(
            type=InteractionAnswerType.TEXT,
            value="mango",
        ),
    )

    apply_interaction_response_to_data(data, response)

    assert data["interaction_response"] == response.to_dict()
    assert "selected_value" not in data
    assert "custom_input" not in data


def test_interaction_request_round_trips_via_dict():
    request = AskUserRequestBuilder.from_tool_args(
        kind="choice_or_text",
        prompt="pick one",
        options=[{"value": "a", "label": "A", "description": "Alpha option"}],
        default="a",
        custom={"label": "Other", "placeholder": "Type here"},
    )

    restored = InteractionRequest.from_dict(request.to_dict())

    assert restored.id == request.id
    assert restored.kind == request.kind
    assert restored.options[0].description == "Alpha option"
    assert restored.custom is not None
    assert restored.custom.placeholder == "Type here"


def test_interaction_service_delegates_to_renderer_without_tty_gate():
    request = AskUserRequestBuilder.from_tool_args(
        kind="choice_or_text",
        prompt="pick one",
        options=[{"value": "a", "label": "A"}],
    )
    service = InteractionService(
        renderer=lambda _request: InteractionResponse(
            interaction_id=request.id,
            status=InteractionStatus.SUBMITTED,
        )
    )

    response = service.request(request)

    assert response.interaction_id == request.id
    assert response.status == InteractionStatus.SUBMITTED


def test_ask_user_adapter_builds_pause_message_for_cancelled():
    request = AskUserRequestBuilder.from_tool_args(
        kind="choice_or_text",
        prompt="pick one",
        options=[
            {"value": "a", "label": "A", "description": "Alpha option"},
            {"value": "b", "label": "B"},
        ],
        default="a",
        custom={"label": "Other"},
    )
    response = InteractionResponse(
        interaction_id=request.id,
        status=InteractionStatus.CANCELLED,
        reason="cancelled",
    )

    result = AskUserInteractionAdapter.to_tool_result(request, response)

    assert result.ok is False
    assert result.meta["kind"] == "user_input_required"
    assert result.meta["interaction_id"] == request.id
    assert "Alpha option" in result.output
    assert "continue with default" in result.output
    assert "ask_user_context" not in result.output


def test_ask_user_adapter_builds_selected_tool_result():
    request = AskUserRequestBuilder.from_tool_args(
        kind="choice_or_text",
        prompt="pick one",
        options=[{"value": "a", "label": "A"}],
        default="a",
    )
    response = InteractionResponse(
        interaction_id=request.id,
        status=InteractionStatus.SUBMITTED,
        answer=InteractionAnswer(
            type=InteractionAnswerType.OPTION,
            value="a",
            label="A",
        ),
    )

    result = AskUserInteractionAdapter.to_tool_result(request, response)

    assert result.ok is True
    assert result.output == "User selected: A"
    assert result.data["interaction_id"] == request.id
    assert result.meta["interaction_status"] == "submitted"