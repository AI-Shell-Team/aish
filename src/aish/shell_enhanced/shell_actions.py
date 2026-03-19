"""Strategy-style action handlers for routed shell input."""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING

from .shell_types import ActionContext, ActionOutcome, InputIntent, ShellAction

if TYPE_CHECKING:
    from ..shell import AIShell
    from .shell_command_service import ShellCommandService


@dataclass
class _BaseAction:
    shell: "AIShell"
    command_service: "ShellCommandService"


class EmptyAction(_BaseAction):
    async def execute(self, ctx: ActionContext) -> ActionOutcome:
        return ActionOutcome(handled=True)


class AIAction(_BaseAction):
    async def execute(self, ctx: ActionContext) -> ActionOutcome:
        await self.shell.handle_ai_command(ctx.stripped_input)
        return ActionOutcome(handled=True)


class HelpAction(_BaseAction):
    async def execute(self, ctx: ActionContext) -> ActionOutcome:
        command = str(ctx.route_data.get("help_command") or "")
        if command and self.shell.help_manager.has_help(command):
            self.shell.help_manager.show_help(command)
            return ActionOutcome(handled=True)
        return ActionOutcome(handled=False)


class OperatorCommandAction(_BaseAction):
    async def execute(self, ctx: ActionContext) -> ActionOutcome:
        await self.command_service.handle_operator_command(ctx.stripped_input)
        return ActionOutcome(handled=True)


class SpecialCommandAction(_BaseAction):
    async def execute(self, ctx: ActionContext) -> ActionOutcome:
        handled = await self.command_service.handle_special_command(ctx.stripped_input)
        return ActionOutcome(handled=handled)


class BuiltinQuickAction(_BaseAction):
    async def execute(self, ctx: ActionContext) -> ActionOutcome:
        handled = await self.command_service.handle_quick_builtin_command(
            ctx.stripped_input
        )
        return ActionOutcome(handled=handled)


class CommandOrAIAction(_BaseAction):
    async def execute(self, ctx: ActionContext) -> ActionOutcome:
        cmd_parts = ctx.route_data.get("cmd_parts")
        parse_error = bool(ctx.route_data.get("parse_error"))
        if not isinstance(cmd_parts, list):
            cmd_parts = []
        await self.command_service.handle_command_or_ai(
            ctx.stripped_input,
            cmd_parts=cmd_parts,
            parse_error=parse_error,
        )
        return ActionOutcome(handled=True)


class ScriptCallAction(_BaseAction):
    async def execute(self, ctx: ActionContext) -> ActionOutcome:
        cmd_parts = ctx.route_data.get("cmd_parts")
        if not isinstance(cmd_parts, list) or not cmd_parts:
            return ActionOutcome(handled=False)
        await self.command_service.handle_script_call(
            ctx.stripped_input,
            cmd_parts=cmd_parts,
        )
        return ActionOutcome(handled=True)


def build_default_actions(
    shell: "AIShell", command_service: "ShellCommandService"
) -> dict[InputIntent, ShellAction]:
    return {
        InputIntent.EMPTY: EmptyAction(shell, command_service),
        InputIntent.AI: AIAction(shell, command_service),
        InputIntent.HELP: HelpAction(shell, command_service),
        InputIntent.OPERATOR_COMMAND: OperatorCommandAction(shell, command_service),
        InputIntent.SPECIAL_COMMAND: SpecialCommandAction(shell, command_service),
        InputIntent.BUILTIN_COMMAND: BuiltinQuickAction(shell, command_service),
        InputIntent.SCRIPT_CALL: ScriptCallAction(shell, command_service),
        InputIntent.COMMAND_OR_AI: CommandOrAIAction(shell, command_service),
    }
