"""
Help Manager for AI Shell - Unified help system for built-in commands
"""

from textwrap import dedent
from typing import Dict, Optional

from rich.console import Console
from rich.markdown import Markdown
from rich.panel import Panel

from .i18n import get_value, t


class HelpManager:
    """Unified help system for shell built-in commands"""

    def __init__(self, console: Console):
        self.console = console
        self._command_help: Dict[str, Dict] = {}
        self._setup_default_help()

    def _setup_default_help(self):
        """Setup help information for all built-in commands"""

        commands = get_value("help.command")
        if not isinstance(commands, dict):
            commands = {}

        for command, help_info in commands.items():
            if isinstance(command, str) and isinstance(help_info, dict):
                self._command_help[command] = help_info

    def register_command_help(self, command: str, help_info: Dict):
        """Register help information for a custom command"""
        self._command_help[command] = help_info

    def has_help(self, command: str) -> bool:
        """Check if help is available for a command"""
        return command in self._command_help

    def show_help(self, command: str, show_full: bool = True) -> bool:
        """
        Show help for a specific command

        Args:
            command: The command name
            show_full: Whether to show full help or just usage summary

        Returns:
            True if help was shown, False if command not found
        """
        if command not in self._command_help:
            return False

        help_info = self._command_help[command]

        if show_full:
            # Build full help content
            content_parts = []

            # Description
            if help_info.get("description"):
                content_parts.append(f"**{help_info['description']}**")

            # Usage
            if help_info.get("usage"):
                content_parts.append(
                    f"\n**{t('help.labels.usage')}:**\n`{help_info['usage']}`"
                )

            # Options
            if help_info.get("options"):
                content_parts.append(f"\n**{t('help.labels.options')}:**")
                for option, description in help_info["options"]:
                    content_parts.append(f"- `{option}` - {description}")

            # Examples
            if help_info.get("examples"):
                content_parts.append(f"\n**{t('help.labels.examples')}:**")
                content_parts.append("```bash")
                content_parts.extend(help_info["examples"])
                content_parts.append("```")

            # Notes
            if help_info.get("notes"):
                content_parts.append(
                    f"\n**{t('help.labels.notes')}:** {help_info['notes']}"
                )

            content = "\n".join(content_parts)

            self.console.print(
                Panel(Markdown(content), title=help_info["title"], border_style="cyan")
            )
        else:
            # Show brief usage
            usage = help_info.get("usage", command)
            self.console.print(t("help.labels.brief_usage", usage=usage))

        return True

    def show_general_help(self):
        """Show general help for all available commands"""
        self.console.print(
            Panel(
                Markdown(dedent(t("help.general.markdown"))),
                title=t("help.general.title"),
                border_style="yellow",
            )
        )

    def parse_help_request(self, user_input: str) -> tuple[Optional[str], str]:
        """
        Parse user input to detect help requests

        Args:
            user_input: The raw user input

        Returns:
            Tuple of (command_name, remaining_input) or (None, original_input) if not a help request
        """
        if not user_input or not user_input.strip():
            return None, user_input

        parts = user_input.strip().split()
        if len(parts) < 2:
            return None, user_input

        # Check for --help or -h at the end
        if parts[-1] in ["--help", "-h"]:
            command = parts[0]
            return command, user_input

        return None, user_input
