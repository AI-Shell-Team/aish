"""CLI entry point for AI Shell."""

import os
import sys
from enum import Enum
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
from .providers.openai_codex import OPENAI_CODEX_DEFAULT_CALLBACK_PORT
from .providers.registry import get_provider_by_id, list_auth_capable_provider_ids
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
models_app = typer.Typer(help="Manage models and provider auth", cls=I18nTyperGroup)
models_auth_app = typer.Typer(help="Manage provider login state", cls=I18nTyperGroup)
models_app.add_typer(models_auth_app, name="auth")
app.add_typer(models_app, name="models")


class ProviderAuthFlow(str, Enum):
    BROWSER = "browser"
    DEVICE_CODE = "device-code"
    CODEX_CLI = "codex-cli"


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

    codex_auth_path_env = os.getenv("AISH_CODEX_AUTH_PATH")
    if codex_auth_path_env:
        config_data["codex_auth_path"] = codex_auth_path_env

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
    tui: bool = typer.Option(
        False,
        "--tui",
        help="Enable TUI (Terminal User Interface) mode",
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

    # Check if TUI mode is enabled (CLI flag or config)
    enable_tui = tui or effective_config.tui.enabled

    if enable_tui:
        # Run in TUI mode
        from aish.tui import TUIApp

        tui_app = TUIApp(effective_config, shell)
        shell._tui_app = tui_app

        try:
            anyio.run(tui_app.run)
        except KeyboardInterrupt:
            console.print("\n" + t("cli.startup.goodbye"), style="green")
            sys.exit(0)
    else:
        # Run in standard mode
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


@models_auth_app.command("login", cls=I18nTyperCommand)
def models_auth_login(
    provider: str = typer.Option(
        ...,
        "--provider",
        help="Provider id to log in (for example: openai-codex).",
    ),
    model: str = typer.Option(
        "",
        "--model",
        help="Default provider model to store in config after login.",
    ),
    set_default: bool = typer.Option(
        True,
        "--set-default/--no-set-default",
        help="Update the config model to the provider model after login.",
    ),
    auth_flow: ProviderAuthFlow = typer.Option(
        ProviderAuthFlow.BROWSER,
        "--auth-flow",
        help="Auth flow to use: browser, device-code, or codex-cli.",
    ),
    force: bool = typer.Option(
        False,
        "--force/--no-force",
        help="Force a fresh provider login even if local auth already exists.",
    ),
    open_browser: bool = typer.Option(
        True,
        "--open-browser/--no-open-browser",
        help="Open the browser automatically for browser auth.",
    ),
    callback_port: int = typer.Option(
        OPENAI_CODEX_DEFAULT_CALLBACK_PORT,
        "--callback-port",
        min=0,
        max=65535,
        help="Local callback port for browser auth. Use 0 for an ephemeral port.",
    ),
    config_file: Optional[str] = typer.Option(
        None,
        "--config",
        "-c",
        help=t("cli.option.config"),
    ),
):
    normalized_provider = provider.strip().lower().replace("_", "-")
    resolved_provider = get_provider_by_id(normalized_provider)
    auth_config = None if resolved_provider is None else resolved_provider.auth_config
    if resolved_provider is None or auth_config is None:
        supported = ", ".join(sorted(list_auth_capable_provider_ids()))
        console.print(
            f"Unsupported provider `{normalized_provider}`. Supported providers: {supported}.",
            style="red",
        )
        raise SystemExit(1)

    try:
        config = Config(config_file_path=config_file)
    except FileNotFoundError as exc:
        console.print(t("cli.startup.config_file_error", error=str(exc)), style="red")
        console.print(t("cli.startup.config_file_hint"), style="dim")
        raise typer.Exit(1) from exc

    auth_path = getattr(config.model_config, auth_config.auth_path_config_key, None)
    auth_state = None
    if not force:
        try:
            auth_state = auth_config.load_auth_state(auth_path)
        except Exception:
            auth_state = None

    if auth_state is None:
        try:
            login_handler = auth_config.get_login_handler(auth_flow.value)
            if login_handler is None:
                console.print(
                    f"Auth flow `{auth_flow.value}` is not supported for {resolved_provider.display_name}.",
                    style="red",
                )
                raise SystemExit(1)

            handler_kwargs = {
                "auth_path": auth_path,
                "notify": lambda message: console.print(message, style="dim"),
            }
            if auth_flow == ProviderAuthFlow.BROWSER:
                handler_kwargs["open_browser"] = open_browser
                handler_kwargs["callback_port"] = callback_port
            auth_state = login_handler(**handler_kwargs)
        except Exception as exc:
            console.print(str(exc), style="red")
            raise typer.Exit(1) from exc

    config_data = config.model_config.model_dump()
    config_data[auth_config.auth_path_config_key] = str(auth_state.auth_path)
    if set_default:
        config_data["model"] = (
            f"{resolved_provider.model_prefix}/{model.strip() or auth_config.default_model}"
        )
        config_data["api_key"] = None
    config.config_model = ConfigModel.model_validate(config_data)
    config.save_config()

    console.print(
        f"{resolved_provider.display_name} auth ready: {auth_state.auth_path}",
        style="green",
    )
    if set_default:
        console.print(f"Default model set to {config.config_model.model}", style="green")
    else:
        console.print(
            f"{resolved_provider.display_name} model available: {resolved_provider.model_prefix}/{model.strip() or auth_config.default_model}",
            style="dim",
        )


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

# Log into OpenAI Codex account auth
aish models auth login --provider openai-codex

# Use built-in device-code auth on headless servers
aish models auth login --provider openai-codex --auth-flow device-code

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

    console.print(
        Panel(Markdown(info_text), title="🚀 AI Shell Info", border_style="blue")
    )


# Plan commands group
plan_app = typer.Typer(
    name="plan",
    help="Manage execution plans for complex tasks",
    cls=I18nTyperGroup,
)
app.add_typer(plan_app, name="plan")


@plan_app.command(name="list", help="List all plans")
def plan_list(
    status: Optional[str] = typer.Option(
        None,
        "--status",
        "-s",
        help="Filter by status (draft, approved, in_progress, paused, completed, failed, cancelled)",
    ),
    limit: int = typer.Option(
        50,
        "--limit",
        "-n",
        help="Maximum number of plans to show",
    ),
):
    """List all plans with optional status filtering."""
    from .plans.manager import PlanManager
    from .plans.models import PlanStatus
    from rich.table import Table

    plan_manager = PlanManager()

    # Validate status if provided
    status_filter = None
    if status:
        try:
            status_filter = status.lower()
            # Validate it's a valid status
            PlanStatus(status_filter)
        except ValueError:
            console.print(f"Invalid status: {status}", style="red")
            console.print("Valid statuses: draft, approved, in_progress, paused, completed, failed, cancelled")
            raise typer.Exit(1)

    plans = plan_manager.list_plans(status=status_filter, limit=limit)

    if not plans:
        if status_filter:
            console.print(f"No plans found with status '{status_filter}'", style="dim")
        else:
            console.print("No plans found. Use 'plan_agent' tool to create a plan.", style="dim")
        return

    # Create table
    table = Table(title=f"Plans ({len(plans)} found)")
    table.add_column("ID", style="cyan", no_wrap=True)
    table.add_column("Title", style="green")
    table.add_column("Status", style="yellow")
    table.add_column("Progress", style="blue")
    table.add_column("Updated", style="dim")

    for p in plans:
        progress = f"{p['completed_steps']}/{p['total_steps']}"
        # Format timestamp
        updated = p['updated_at'][:19] if p['updated_at'] else "N/A"

        # Status color
        status_style = {
            "draft": "dim",
            "approved": "green",
            "in_progress": "yellow",
            "paused": "yellow",
            "completed": "green",
            "failed": "red",
            "cancelled": "red",
        }.get(p['status'], "")

        table.add_row(
            p['plan_id'],
            p['title'][:50] + "..." if len(p['title']) > 50 else p['title'],
            f"[{status_style}]{p['status']}[/{status_style}]",
            progress,
            updated,
        )

    console.print(table)


@plan_app.command(name="show", help="Show plan details")
def plan_show(
    plan_id: str = typer.Argument(..., help="Plan ID"),
):
    """Show detailed information about a plan."""
    from .plans.manager import PlanManager

    plan_manager = PlanManager()
    plan = plan_manager.load_plan(plan_id)

    if plan is None:
        console.print(f"Plan not found: {plan_id}", style="red")
        raise typer.Exit(1)


    # Show plan info
    console.print(f"[bold cyan]Plan ID:[/bold cyan] {plan.plan_id}")
    console.print(f"[bold green]Title:[/bold green] {plan.title}")
    console.print(f"[bold yellow]Status:[/bold yellow] {plan.status.value}")
    console.print(f"[bold blue]Progress:[/bold blue] {plan.get_progress_summary()['completed']}/{len(plan.steps)} steps")

    # Show steps
    if plan.steps:
        console.print("\n[bold]Steps:[/bold]")
        for step in plan.steps:
            icon = {
                "pending": "○",
                "in_progress": "◐",
                "completed": "[green]✓[/green]",
                "skipped": "[dim]⊘[/dim]",
                "failed": "[red]✗[/red]",
            }.get(step.status.value, "?")

            console.print(f"  {icon} [bold]Step {step.number}:[/bold] {step.title}")
            if step.description:
                console.print(f"      {step.description}")
            if step.error_message:
                console.print(f"      [red]Error: {step.error_message}[/red]")

    # Show markdown path
    md_path = plan_manager.get_markdown_path(plan_id)
    console.print(f"\n[dim]Markdown file: {md_path}[/dim]")


@plan_app.command(name="execute", help="Execute a plan")
def plan_execute(
    plan_id: str = typer.Argument(..., help="Plan ID to execute"),
    start_from_step: Optional[int] = typer.Option(
        None,
        "--from",
        "-f",
        help="Start from specific step number",
    ),
):
    """Execute an approved plan."""
    from .plans.manager import PlanManager
    from .plans.models import PlanStatus

    plan_manager = PlanManager()
    plan = plan_manager.load_plan(plan_id)

    if plan is None:
        console.print(f"Plan not found: {plan_id}", style="red")
        raise typer.Exit(1)

    if plan.status != PlanStatus.APPROVED:
        console.print(f"Plan is not approved. Current status: {plan.status.value}", style="yellow")
        console.print("Use the following to approve the plan first:")
        console.print("  In shell: Use 'approve_plan' tool or set status to approved")
        raise typer.Exit(1)

    console.print(f"[green]Executing plan: {plan.title}[/green]")
    if start_from_step:
        console.print(f"[yellow]Starting from step {start_from_step}[/yellow]")

    # Note: Actual execution happens through build_agent in the shell
    # This is a CLI shortcut that starts the shell with the plan ready to execute
    console.print("\n[yellow]Plan execution requires running in shell mode.[/yellow]")
    console.print("Start the shell and use: build_agent(plan_id='" + plan_id + "')")


@plan_app.command(name="approve", help="Approve a plan for execution")
def plan_approve(
    plan_id: str = typer.Argument(..., help="Plan ID to approve"),
):
    """Approve a plan, making it ready for execution."""
    from .plans.manager import PlanManager
    from .plans.models import PlanStatus

    plan_manager = PlanManager()
    plan = plan_manager.load_plan(plan_id)

    if plan is None:
        console.print(f"Plan not found: {plan_id}", style="red")
        raise typer.Exit(1)

    if plan.status == PlanStatus.APPROVED:
        console.print(f"Plan is already approved: {plan.title}", style="green")
        return

    if plan.status != PlanStatus.DRAFT:
        console.print(f"Cannot approve plan with status: {plan.status.value}", style="yellow")
        raise typer.Exit(1)

    plan = plan_manager.update_plan_status(plan_id, PlanStatus.APPROVED)

    if plan:
        console.print(f"[green]✓ Plan approved: {plan.title}[/green]")
        console.print(f"  Plan ID: {plan.plan_id}")
        console.print(f"  Steps: {len(plan.steps)}")
        console.print(f"\nTo execute, use in shell: build_agent(plan_id='{plan.plan_id}')")
    else:
        console.print("Failed to approve plan", style="red")
        raise typer.Exit(1)


@plan_app.command(name="cancel", help="Cancel a plan")
def plan_cancel(
    plan_id: str = typer.Argument(..., help="Plan ID to cancel"),
):
    """Cancel a plan."""
    from .plans.manager import PlanManager
    from .plans.models import PlanStatus

    plan_manager = PlanManager()
    plan = plan_manager.load_plan(plan_id)

    if plan is None:
        console.print(f"Plan not found: {plan_id}", style="red")
        raise typer.Exit(1)

    if plan.status == PlanStatus.CANCELLED:
        console.print(f"Plan is already cancelled: {plan.title}", style="dim")
        return

    plan = plan_manager.update_plan_status(plan_id, PlanStatus.CANCELLED)

    if plan:
        console.print(f"[yellow]✗ Plan cancelled: {plan.title}[/yellow]")
    else:
        console.print("Failed to cancel plan", style="red")
        raise typer.Exit(1)


@plan_app.command(name="delete", help="Delete a plan")
def plan_delete(
    plan_id: str = typer.Argument(..., help="Plan ID to delete"),
):
    """Delete a plan permanently."""
    from .plans.manager import PlanManager

    plan_manager = PlanManager()

    if not plan_manager.load_plan(plan_id):
        console.print(f"Plan not found: {plan_id}", style="red")
        raise typer.Exit(1)

    if plan_manager.delete_plan(plan_id):
        console.print(f"[green]✓ Plan deleted: {plan_id}[/green]")
    else:
        console.print("Failed to delete plan", style="red")
        raise typer.Exit(1)


def main():
    """Main entry point for the CLI"""
    app()


if __name__ == "__main__":
    from dotenv import load_dotenv

    load_dotenv()
    main()
