from __future__ import annotations

import re

import click
import typer
from rich.console import Console
from rich.panel import Panel

from . import t

_CONSOLE = Console()

_RE_OPTION_REQUIRES_ARGUMENT = re.compile(r"^Option '([^']+)' requires an argument\.$")
_RE_NO_SUCH_OPTION = re.compile(r"^No such option: (.+)$")
_RE_MISSING_OPTION = re.compile(r"^Missing option '([^']+)'\.$")
_RE_MISSING_PARAMETER = re.compile(r"^Missing parameter: (.+)$")
_RE_INVALID_VALUE_FOR = re.compile(r"^Invalid value for '([^']+)': (.+)$")


def _format_models_auth_option_hint(option: str) -> str | None:
    if option == "--provider":
        return t(
            "cli.parse_errors.models_auth.provider_example",
            command="aish models auth --provider openai-codex",
        )
    return None


def _contextual_usage_hint(ctx: click.Context | None, *, option: str | None = None) -> str | None:
    if option == "--provider":
        return _format_models_auth_option_hint(option)

    if ctx is None:
        return None

    command_path = (ctx.command_path or "").strip()
    if command_path:
        return t("cli.parse_errors.try_help_hint", help_command=f"{command_path} --help")

    return None


def _translate_click_usage_error(message: str, ctx: click.Context | None = None) -> str:
    msg = (message or "").strip()

    m = _RE_OPTION_REQUIRES_ARGUMENT.match(msg)
    if m:
        option = m.group(1)
        translated = t("cli.parse_errors.option_requires_argument", option=option)
        hint = _contextual_usage_hint(ctx, option=option)
        return f"{translated}\n{hint}" if hint else translated

    m = _RE_NO_SUCH_OPTION.match(msg)
    if m:
        translated = t("cli.parse_errors.no_such_option", option=m.group(1))
        hint = _contextual_usage_hint(ctx)
        return f"{translated}\n{hint}" if hint else translated

    m = _RE_MISSING_OPTION.match(msg)
    if m:
        option = m.group(1)
        translated = t("cli.parse_errors.missing_option", option=option)
        hint = _contextual_usage_hint(ctx, option=option)
        return f"{translated}\n{hint}" if hint else translated

    m = _RE_MISSING_PARAMETER.match(msg)
    if m:
        translated = t("cli.parse_errors.missing_parameter", param=m.group(1))
        hint = _contextual_usage_hint(ctx)
        return f"{translated}\n{hint}" if hint else translated

    m = _RE_INVALID_VALUE_FOR.match(msg)
    if m:
        translated = t(
            "cli.parse_errors.invalid_value_for_option",
            option=m.group(1),
            reason=m.group(2),
        )
        hint = _contextual_usage_hint(ctx, option=m.group(1))
        return f"{translated}\n{hint}" if hint else translated

    if msg == "Missing command.":
        translated = t("cli.parse_errors.missing_command")
        hint = _contextual_usage_hint(ctx)
        return f"{translated}\n{hint}" if hint else translated

    translated = t("cli.parse_errors.generic", message=msg or "")
    hint = _contextual_usage_hint(ctx)
    return f"{translated}\n{hint}" if hint else translated


def _print_cli_parse_error(message: str) -> None:
    _CONSOLE.print(
        Panel(
            message,
            title=t("cli.parse_errors.title"),
            border_style="red",
        )
    )


class I18nTyperCommand(typer.core.TyperCommand):
    def get_help_option(self, ctx: click.Context) -> click.Option | None:  # type: ignore[override]
        opt = super().get_help_option(ctx)
        if opt is not None:
            opt.help = t("cli.help_option_help")
        return opt


class I18nTyperGroup(typer.core.TyperGroup):
    def get_help_option(self, ctx: click.Context) -> click.Option | None:  # type: ignore[override]
        opt = super().get_help_option(ctx)
        if opt is not None:
            opt.help = t("cli.help_option_help")
        return opt

    def collect_usage_pieces(self, ctx: click.Context) -> list[str]:  # type: ignore[override]
        pieces = super().collect_usage_pieces(ctx)

        # Hidden compatibility subcommands should not leak into the displayed
        # usage line. If a group has no visible subcommands, treat it like an
        # option-only command for help/usage rendering.
        has_visible_subcommands = any(
            not getattr(command, "hidden", False)
            for command in self.commands.values()
        )
        if has_visible_subcommands:
            return pieces

        return [piece for piece in pieces if piece != "COMMAND [ARGS]..."]

    def main(self, *args, **kwargs):  # type: ignore[override]
        """Override Click main() to localize common parse errors."""

        # Ensure we can intercept exceptions and control exit code.
        kwargs["standalone_mode"] = False

        try:
            super().main(*args, **kwargs)
            raise SystemExit(0)
        except click.UsageError as e:
            msg = _translate_click_usage_error(e.format_message(), getattr(e, "ctx", None))
            _print_cli_parse_error(msg)
            raise SystemExit(getattr(e, "exit_code", 2) or 2)
        except click.ClickException as e:
            msg = _translate_click_usage_error(e.format_message(), getattr(e, "ctx", None))
            _print_cli_parse_error(msg)
            raise SystemExit(getattr(e, "exit_code", 1) or 1)
