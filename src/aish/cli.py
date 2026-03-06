"""CLI entry point for AI Shell."""

import os
import sys
from typing import Optional

import anyio
import typer
import yaml
from rich.console import Console
from rich.panel import Panel

from .config import Config, ConfigModel
from .i18n import t
from .i18n.typer import I18nTyperCommand, I18nTyperGroup
from .logging_utils import init_logging
from .shell import AIShell
from .skills import SkillManager
from .wizard.setup_wizard import (needs_interactive_setup,
                                  run_interactive_setup,
                                  run_live_tool_support_check_debug)

app = typer.Typer(
    name="aish",
    help=t("cli.app_help"),
    add_completion=False,
    invoke_without_command=True,
    cls=I18nTyperGroup,
)

console = Console()


def _load_raw_yaml_config(config_file: str | os.PathLike[str]) -> dict:
    """Load raw YAML mapping for presence/blank checks.

    This is intentionally separate from ConfigModel validation so we can
    distinguish between "not provided" vs "defaulted" values.
    """

    try:
        # config_file is expected to be a Path-like.
        with open(config_file, "r", encoding="utf-8") as f:
            data = yaml.safe_load(f) or {}
        return data if isinstance(data, dict) else {}
    except Exception:
        return {}


def get_effective_config(
    config: Config,
    model: Optional[str] = None,
    api_key: Optional[str] = None,
    api_base: Optional[str] = None,
) -> ConfigModel:
    """Get effective configuration with priority: CLI args > env vars > config file"""

    # Start with config file values (copy all fields from the config model)
    config_data = config.model_config.model_dump()

    # Override with environment variables
    model_env = os.getenv("AISH_MODEL")
    if model_env:
        config_data["model"] = model_env

    api_base_env = os.getenv("AISH_API_BASE")
    if api_base_env:
        config_data["api_base"] = api_base_env

    api_key_env = os.getenv("AISH_API_KEY")
    if api_key_env:
        config_data["api_key"] = api_key_env

    # Override with command line arguments (highest priority)
    if model is not None:
        config_data["model"] = model

    if api_key is not None:
        config_data["api_key"] = api_key

    if api_base is not None:
        config_data["api_base"] = api_base

    # Create and return a new ConfigModel with the effective configuration
    return ConfigModel.model_validate(config_data)


@app.callback()
def _default(ctx: typer.Context):
    """Default entrypoint.

    Running `aish` with no subcommand behaves like `aish run`.
    """

    if ctx.invoked_subcommand is None:
        # Call run() with explicit None values so we don't accidentally pass
        # Typer OptionInfo objects (which only make sense under Click parsing).
        run(model=None, api_key=None, api_base=None, config_file=None)


def check_langfuse_config(effective_config: ConfigModel):
    """Check Langfuse configuration and print diagnostics."""
    if not effective_config.enable_langfuse:
        return True

    langfuse_vars = {
        "LANGFUSE_PUBLIC_KEY": os.getenv("LANGFUSE_PUBLIC_KEY"),
        "LANGFUSE_SECRET_KEY": os.getenv("LANGFUSE_SECRET_KEY"),
        "LANGFUSE_HOST": os.getenv("LANGFUSE_HOST"),
    }

    missing_vars = [var for var, value in langfuse_vars.items() if not value]

    if missing_vars:
        console.print(t("cli.langfuse.incomplete"), style="yellow")
        for var in missing_vars:
            console.print(t("cli.langfuse.missing", var=var), style="yellow")
        return False
    else:
        console.print(t("cli.langfuse.complete"), style="green")
        return True


@app.command(help=t("cli.run_command_help"), cls=I18nTyperCommand)
def run(
    model: Optional[str] = typer.Option(
        None,
        "--model",
        "-m",
        help=t("cli.option.model"),
    ),
    api_key: Optional[str] = typer.Option(
        None,
        "--api-key",
        "-k",
        help=t("cli.option.api_key"),
    ),
    api_base: Optional[str] = typer.Option(
        None,
        "--api-base",
        "-b",
        help=t("cli.option.api_base"),
    ),
    config_file: Optional[str] = typer.Option(
        None,
        "--config",
        "-c",
        help=t("cli.option.config"),
    ),
):
    """Run the AI Shell"""

    # Load configuration
    try:
        config = Config(config_file_path=config_file)
    except FileNotFoundError as e:
        console.print(t("cli.startup.config_file_error", error=str(e)), style="red")
        console.print(t("cli.startup.config_file_hint"), style="dim")
        sys.exit(1)

    raw_config = _load_raw_yaml_config(config.config_file) if config.config_file else {}
    needs_setup = needs_interactive_setup(raw_config, model, api_key)
    if needs_setup and run_interactive_setup(config) is None:
        console.print(t("cli.setup.required_cancelled"), style="red")
        sys.exit(1)

    # Get effective configuration with priority handling
    effective_config = get_effective_config(config, model, api_key, api_base)

    init_logging(effective_config)

    # Keep startup output minimal: the interactive shell welcome screen will display
    # the key fields (version/model/config path) in a structured template.
    # Inject config file path for welcome rendering (ConfigModel allows extra fields).
    try:
        effective_config.config_file = str(config.config_file)
    except Exception:
        # Best-effort only; welcome screen will fall back to "-".
        pass

    skill_manager = SkillManager()
    skills = skill_manager.load_all_skills()
    try:
        effective_config.skills_count = len(skills)
    except Exception:
        pass

    # Create and run the shell
    shell = AIShell(
        config=effective_config,
        skill_manager=skill_manager,
        config_manager=config,
    )

    try:
        anyio.run(shell.run)
    except KeyboardInterrupt:
        console.print("\n" + t("cli.startup.goodbye"), style="green")
        sys.exit(0)


@app.command(help=t("cli.setup_command_help"), cls=I18nTyperCommand)
def setup(
    config_file: Optional[str] = typer.Option(
        None,
        "--config",
        "-c",
        help=t("cli.option.config"),
    ),
):
    """Run interactive setup and exit."""
    try:
        config = Config(config_file_path=config_file)
    except FileNotFoundError as e:
        console.print(t("cli.startup.config_file_error", error=str(e)), style="red")
        console.print(t("cli.startup.config_file_hint"), style="dim")
        sys.exit(1)

    result = run_interactive_setup(config)
    if result is None:
        console.print(t("cli.setup.cancelled"), style="yellow")
        sys.exit(1)


@app.command(help=t("cli.check_tool_support_command_help"), cls=I18nTyperCommand)
def check_tool_support(
    model: str = typer.Option(
        ...,
        "--model",
        "-m",
        help=t("cli.option.model"),
    ),
    api_key: Optional[str] = typer.Option(
        None,
        "--api-key",
        "-k",
        help=t("cli.option.api_key"),
    ),
    api_base: Optional[str] = typer.Option(
        None,
        "--api-base",
        "-b",
        help=t("cli.option.api_base"),
    ),
):
    """Run live tool-call verification with debug output."""
    result = run_live_tool_support_check_debug(
        model=model,
        api_base=api_base,
        api_key=api_key,
    )
    if result.supports is not True:
        sys.exit(1)


@app.command(help=t("cli.check_langfuse_command_help"), cls=I18nTyperCommand)
def check_langfuse():
    """Check Langfuse configuration."""
    import subprocess
    import sys
    from pathlib import Path

    script_path = Path(__file__).parent.parent.parent / "check_langfuse.py"

    if script_path.exists():
        try:
            subprocess.run([sys.executable, str(script_path)], check=True)
        except subprocess.CalledProcessError as e:
            console.print(t("cli.langfuse.script_failed", error=str(e)), style="red")
    else:
        console.print(t("cli.langfuse.script_not_found"), style="red")
        console.print(t("cli.langfuse.run_from_root"), style="yellow")


@app.command(help=t("cli.info_command_help"), cls=I18nTyperCommand)
def info():
    """Show information about AI Shell"""
    info_text = """
# AI Shell

A modern shell with built-in LLM capabilities for enhanced productivity.

## Features:
- 🤖 AI assistant integration
- 📚 Command explanations
- 💡 Task-based command suggestions
- 🎨 Rich terminal interface
- 📝 Command history and auto-suggestions
- 🔧 Full shell command execution

## Configuration Priority:
1. Command line arguments (highest priority)
2. Environment variables
3. Configuration file (lowest priority)

## Configuration File:
- Location: ~/.config/aish/config.yaml
- Example:
```yaml
model: gpt-4
temperature: 0.7
enable_langfuse: false
```

## Environment Variables:
- AISH_MODEL: Set default model
- OPENAI_API_KEY: For OpenAI models
- ANTHROPIC_API_KEY: For Anthropic models
- GOOGLE_API_KEY: For Google models

## Logs:
- Default path: ~/.config/aish/logs/aish.log

## Langfuse Integration:
- LANGFUSE_PUBLIC_KEY: Langfuse public API key
- LANGFUSE_SECRET_KEY: Langfuse secret API key
- LANGFUSE_HOST: Langfuse server URL (e.g., https://cloud.langfuse.com)

## Supported Models:
- OpenAI: gpt-3.5-turbo, gpt-4, gpt-4-turbo
- Anthropic: claude-3-sonnet-20240229, claude-3-haiku-20240307
- Google: gemini-pro, gemini-1.5-pro
- And many more via LiteLLM

## Usage Examples:
```bash
# Use command line arguments
aish run --model gpt-4 --api-key your-key

# Use custom configuration file
aish run --config /path/to/my-config.yaml

# Use environment variables
export AISH_MODEL=claude-3-sonnet-20240229
export ANTHROPIC_API_KEY=your-key
aish run

# Check Langfuse configuration
aish check-langfuse

# Use config file
cat > ~/.config/aish/config.yaml << EOF
model: gpt-4
EOF
aish run

# Combine custom config with CLI overrides
aish run --config ./project-config.yaml --model gpt-4
```
    """

    from rich.markdown import Markdown
    from rich.panel import Panel

    console.print(
        Panel(Markdown(info_text), title="🚀 AI Shell Info", border_style="blue")
    )


def main():
    """Main entry point for the CLI"""
    app()


if __name__ == "__main__":
    from dotenv import load_dotenv

    load_dotenv()
    main()
