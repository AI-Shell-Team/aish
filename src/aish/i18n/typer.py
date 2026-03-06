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
_RE_MISSING_PARAMETER = re.compile(r"^Missing parameter: (.+)$")
_RE_INVALID_VALUE_FOR = re.compile(r"^Invalid value for '([^']+)': (.+)$")


def _translate_click_usage_error(message: str) -> str:
    msg = (message or "").strip()

    m = _RE_OPTION_REQUIRES_ARGUMENT.match(msg)
    if m:
        return t("cli.parse_errors.option_requires_argument", option=m.group(1))

    m = _RE_NO_SUCH_OPTION.match(msg)
    if m:
        return t("cli.parse_errors.no_such_option", option=m.group(1))

    m = _RE_MISSING_PARAMETER.match(msg)
    if m:
        return t("cli.parse_errors.missing_parameter", param=m.group(1))

    m = _RE_INVALID_VALUE_FOR.match(msg)
    if m:
        return t(
            "cli.parse_errors.invalid_value_for_option",
            option=m.group(1),
            reason=m.group(2),
        )

    return t("cli.parse_errors.generic", message=msg or "")


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

    def main(self, *args, **kwargs):  # type: ignore[override]
        """Override Click main() to localize common parse errors."""

        # Ensure we can intercept exceptions and control exit code.
        kwargs["standalone_mode"] = False

        try:
            super().main(*args, **kwargs)
            raise SystemExit(0)
        except click.UsageError as e:
            msg = _translate_click_usage_error(e.format_message())
            _print_cli_parse_error(msg)
            raise SystemExit(getattr(e, "exit_code", 2) or 2)
        except click.ClickException as e:
            msg = _translate_click_usage_error(e.format_message())
            _print_cli_parse_error(msg)
            raise SystemExit(getattr(e, "exit_code", 1) or 1)
