from __future__ import annotations

from unittest.mock import AsyncMock, Mock

import pytest

from aish.shell_enhanced.shell_actions import build_default_actions
from aish.shell_enhanced.shell_types import ActionContext, InputIntent


@pytest.mark.asyncio
async def test_build_default_actions_contains_all_intents():
    shell = Mock()
    command_service = Mock()
    actions = build_default_actions(shell, command_service)

    expected = {
        InputIntent.EMPTY,
        InputIntent.AI,
        InputIntent.HELP,
        InputIntent.OPERATOR_COMMAND,
        InputIntent.SPECIAL_COMMAND,
        InputIntent.BUILTIN_COMMAND,
        InputIntent.COMMAND_OR_AI,
    }
    assert expected.issubset(set(actions.keys()))


@pytest.mark.asyncio
async def test_command_or_ai_action_forwards_route_data():
    shell = Mock()
    command_service = Mock()
    command_service.handle_command_or_ai = AsyncMock()

    actions = build_default_actions(shell, command_service)
    ctx = ActionContext(
        raw_input="ls -la",
        stripped_input="ls -la",
        route_data={"cmd_parts": ["ls", "-la"], "parse_error": False},
    )

    outcome = await actions[InputIntent.COMMAND_OR_AI].execute(ctx)
    assert outcome.handled is True
    command_service.handle_command_or_ai.assert_awaited_once_with(
        "ls -la",
        cmd_parts=["ls", "-la"],
        parse_error=False,
    )
