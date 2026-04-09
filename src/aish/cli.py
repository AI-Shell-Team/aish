"""CLI entry point for AI Shell."""

import os
import sys
from enum import Enum
from typing import Optional

import typer
import yaml
from rich.console import Console
from rich.panel import Panel

from .config import Config, ConfigModel
from .i18n import t
from .i18n.typer import I18nTyperCommand, I18nTyperGroup
from .llm.providers.openai_codex import OPENAI_CODEX_DEFAULT_CALLBACK_PORT
from .llm.providers.registry import (
    get_provider_by_id as get_provider_by_id,
    get_provider_for_model as get_provider_for_model,
    list_auth_capable_provider_ids as list_auth_capable_provider_ids,
    resolve_provider_metadata as resolve_provider_metadata,
)
from .state.logging import init_logging
from .skills import SkillManager
from .wizard.setup_wizard import (
    needs_interactive_setup as needs_interactive_setup,
    run_interactive_setup as run_interactive_setup,
    run_live_tool_support_check_debug,
)

app = typer.Typer(
    name="aish",
    help=t("cli.app_help"),
    add_completion=False,
    invoke_without_command=True,
    cls=I18nTyperGroup,
)

console = Console()
models_app = typer.Typer(help=t("cli.models.help"), cls=I18nTyperGroup)
models_auth_app = typer.Typer(
    help=t("cli.models.auth.help"),
    cls=I18nTyperGroup,
    invoke_without_command=True,
)
models_app.add_typer(models_auth_app, name="auth")
app.add_typer(models_app, name="models")


class ProviderAuthFlow(str, Enum):
    BROWSER = "browser"
    DEVICE_CODE = "device-code"
    CODEX_CLI = "codex-cli"


def _cli_module():
    import aish.cli as cli_module

    return cli_module


def _mask_secret(secret: str) -> str:
    trimmed = secret.strip()
    if len(trimmed) <= 8:
        return "*" * len(trimmed)
    return f"{trimmed[:4]}...{trimmed[-4:]}"


def _normalize_provider_id(provider: str | None) -> str | None:
    trimmed = (provider or "").strip()
    if not trimmed:
        return None
    return trimmed.lower().replace("_", "-")


def _load_cli_config_or_exit(config_file: Optional[str]) -> Config:
    cli_module = _cli_module()

    try:
        return cli_module.Config(config_file_path=config_file)
    except FileNotFoundError as exc:
        console.print(t("cli.startup.config_file_error", error=str(exc)), style="red")
        console.print(t("cli.startup.config_file_hint"), style="dim")
        raise SystemExit(1) from exc


def _resolve_models_auth_provider(
    *,
    provider: str | None,
):
    cli_module = _cli_module()
    normalized_provider = cli_module._normalize_provider_id(provider)
    if normalized_provider is not None:
        resolved_provider = cli_module.get_provider_by_id(normalized_provider)
        auth_config = None if resolved_provider is None else resolved_provider.auth_config
        if resolved_provider is None or auth_config is None:
            supported = ", ".join(sorted(cli_module.list_auth_capable_provider_ids())) or "-"
            console.print(
                t(
                    "cli.models.auth.unsupported_provider",
                    provider=normalized_provider,
                    supported=supported,
                ),
                style="red",
            )
            raise SystemExit(1)
        return resolved_provider, False

    auth_capable_provider_ids = tuple(sorted(cli_module.list_auth_capable_provider_ids()))
    if not auth_capable_provider_ids:
        console.print(t("cli.models.auth.no_supported_provider"), style="red")
        raise SystemExit(1)

    console.print(
        t(
            "cli.models.auth.provider_required",
            providers=", ".join(auth_capable_provider_ids),
        ),
        style="red",
    )
    console.print(
        t(
            "cli.models.auth.provider_required_example",
            command="aish models auth --provider openai-codex",
        ),
        style="dim",
    )
    raise SystemExit(1)


def _run_models_auth(
    *,
    provider: str | None,
    model: str,
    set_default: bool,
    auth_flow: ProviderAuthFlow,
    force: bool,
    open_browser: bool,
    callback_port: int,
    config_file: Optional[str],
    show_deprecation_notice: bool,
) -> None:
    cli_module = _cli_module()

    if show_deprecation_notice:
        console.print(t("cli.models.auth.deprecated_login_hint"), style="yellow")

    config = cli_module._load_cli_config_or_exit(config_file)
    resolved_provider, _ = cli_module._resolve_models_auth_provider(
        provider=provider,
    )
    auth_config = resolved_provider.auth_config
    if auth_config is None:
        console.print(t("cli.models.auth.no_supported_provider"), style="red")
        raise SystemExit(1)

    auth_path = getattr(config.config_model, auth_config.auth_path_config_key, None)
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
                    t(
                        "cli.models.auth.unsupported_auth_flow",
                        auth_flow=auth_flow.value,
                        provider=resolved_provider.display_name,
                    ),
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
        except SystemExit:
            raise
        except Exception as exc:
            console.print(str(exc), style="red")
            raise SystemExit(1) from exc

    resolved_model = model.strip() or auth_config.default_model
    config_data = config.config_model.model_dump()
    config_data[auth_config.auth_path_config_key] = str(auth_state.auth_path)
    if set_default:
        config_data["model"] = f"{resolved_provider.model_prefix}/{resolved_model}"
        config_data["api_key"] = None
    config.config_model = ConfigModel.model_validate(config_data)
    config.save_config()

    console.print(
        t(
            "cli.models.auth.auth_ready",
            provider=resolved_provider.display_name,
            auth_path=str(auth_state.auth_path),
        ),
        style="green",
    )
    if set_default:
        console.print(
            t("cli.models.auth.default_model_set", model=config.config_model.model),
            style="green",
        )
    else:
        console.print(
            t(
                "cli.models.auth.model_available",
                provider=resolved_provider.display_name,
                model=f"{resolved_provider.model_prefix}/{resolved_model}",
            ),
            style="dim",
        )


def _load_raw_yaml_config(config_file: str | os.PathLike[str]) -> dict:
    """Load raw YAML mapping for presence/blank checks.

    This is intentionally separate from ConfigModel validation so we can
    distinguish between "not provided" vs "defaulted" values.
    """

    try:
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

    config_data = config.model_config.model_dump()

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

    if model is not None:
        config_data["model"] = model

    if api_key is not None:
        config_data["api_key"] = api_key

    if api_base is not None:
        config_data["api_base"] = api_base

    return ConfigModel.model_validate(config_data)


@app.callback()
def _default(ctx: typer.Context):
    """Default entrypoint.

    Running `aish` with no subcommand behaves like `aish run`.
    """

    if ctx.invoked_subcommand is None:
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

    cli_module = _cli_module()

    try:
        config = cli_module.Config(config_file_path=config_file)
    except FileNotFoundError as e:
        console.print(t("cli.startup.config_file_error", error=str(e)), style="red")
        console.print(t("cli.startup.config_file_hint"), style="dim")
        sys.exit(1)

    raw_config = cli_module._load_raw_yaml_config(config.config_file) if config.config_file else {}
    needs_setup = cli_module.needs_interactive_setup(raw_config, model, api_key)
    if needs_setup and cli_module.run_interactive_setup(config) is None:
        console.print(t("cli.setup.required_cancelled"), style="red")
        sys.exit(1)

    effective_config = get_effective_config(config, model, api_key, api_base)

    init_logging(effective_config)

    try:
        setattr(effective_config, "config_file", str(config.config_file))
    except Exception:
        pass

    skill_manager = SkillManager()
    skills = skill_manager.load_all_skills()
    try:
        setattr(effective_config, "skills_count", len(skills))
    except Exception:
        pass

    from aish.shell import run_shell as run_pty_shell

    try:
        run_pty_shell(effective_config, skill_manager, config)
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
    cli_module = _cli_module()

    try:
        config = cli_module.Config(config_file_path=config_file)
    except FileNotFoundError as e:
        console.print(t("cli.startup.config_file_error", error=str(e)), style="red")
        console.print(t("cli.startup.config_file_hint"), style="dim")
        sys.exit(1)

    result = cli_module.run_interactive_setup(config)
    if result is None:
        console.print(t("cli.setup.cancelled"), style="yellow")
        sys.exit(1)


@models_auth_app.callback(invoke_without_command=True)
def models_auth(
    ctx: typer.Context,
    provider: Optional[str] = typer.Option(
        None,
        "--provider",
        help=t("cli.models.auth.option.provider"),
    ),
    model: str = typer.Option(
        "",
        "--model",
        help=t("cli.models.auth.option.model"),
    ),
    set_default: bool = typer.Option(
        True,
        "--set-default/--no-set-default",
        help=t("cli.models.auth.option.set_default"),
    ),
    auth_flow: ProviderAuthFlow = typer.Option(
        ProviderAuthFlow.BROWSER,
        "--auth-flow",
        help=t("cli.models.auth.option.auth_flow"),
    ),
    force: bool = typer.Option(
        False,
        "--force/--no-force",
        help=t("cli.models.auth.option.force"),
    ),
    open_browser: bool = typer.Option(
        True,
        "--open-browser/--no-open-browser",
        help=t("cli.models.auth.option.open_browser"),
    ),
    callback_port: int = typer.Option(
        OPENAI_CODEX_DEFAULT_CALLBACK_PORT,
        "--callback-port",
        min=0,
        max=65535,
        help=t("cli.models.auth.option.callback_port"),
    ),
    config_file: Optional[str] = typer.Option(
        None,
        "--config",
        "-c",
        help=t("cli.option.config"),
    ),
):
    if ctx.invoked_subcommand is not None:
        return
    _run_models_auth(
        provider=provider,
        model=model,
        set_default=set_default,
        auth_flow=auth_flow,
        force=force,
        open_browser=open_browser,
        callback_port=callback_port,
        config_file=config_file,
        show_deprecation_notice=False,
    )


@models_auth_app.command(
    "login",
    cls=I18nTyperCommand,
    help=t("cli.models.auth.login_command_help"),
    hidden=True,
)
def models_auth_login(
    provider: Optional[str] = typer.Option(
        None,
        "--provider",
        help=t("cli.models.auth.option.provider"),
    ),
    model: str = typer.Option(
        "",
        "--model",
        help=t("cli.models.auth.option.model"),
    ),
    set_default: bool = typer.Option(
        True,
        "--set-default/--no-set-default",
        help=t("cli.models.auth.option.set_default"),
    ),
    auth_flow: ProviderAuthFlow = typer.Option(
        ProviderAuthFlow.BROWSER,
        "--auth-flow",
        help=t("cli.models.auth.option.auth_flow"),
    ),
    force: bool = typer.Option(
        False,
        "--force/--no-force",
        help=t("cli.models.auth.option.force"),
    ),
    open_browser: bool = typer.Option(
        True,
        "--open-browser/--no-open-browser",
        help=t("cli.models.auth.option.open_browser"),
    ),
    callback_port: int = typer.Option(
        OPENAI_CODEX_DEFAULT_CALLBACK_PORT,
        "--callback-port",
        min=0,
        max=65535,
        help=t("cli.models.auth.option.callback_port"),
    ),
    config_file: Optional[str] = typer.Option(
        None,
        "--config",
        "-c",
        help=t("cli.option.config"),
    ),
):
    _run_models_auth(
        provider=provider,
        model=model,
        set_default=set_default,
        auth_flow=auth_flow,
        force=force,
        open_browser=open_browser,
        callback_port=callback_port,
        config_file=config_file,
        show_deprecation_notice=True,
    )


@models_app.command("usage", help=t("cli.models.usage_command_help"))
def models_usage(
    config_file: Optional[str] = typer.Option(
        None,
        "--config",
        "-c",
        help=t("cli.option.config"),
    ),
):
    cli_module = _cli_module()

    try:
        config = cli_module.Config(config_file_path=config_file)
    except FileNotFoundError as exc:
        console.print(t("cli.startup.config_file_error", error=str(exc)), style="red")
        console.print(t("cli.startup.config_file_hint"), style="dim")
        raise typer.Exit(1) from exc

    effective_config = get_effective_config(config)
    provider = cli_module.get_provider_for_model(effective_config.model)
    metadata = cli_module.resolve_provider_metadata(
        effective_config.model, effective_config.api_base
    )
    status = provider.get_usage_status(effective_config)

    console.print(
        f"[bold]{t('cli.models_usage.current_model')}:[/bold] {effective_config.model or '-'}"
    )
    console.print(
        f"[bold]{t('cli.models_usage.provider')}:[/bold] {metadata.display_name}"
    )
    if effective_config.api_base:
        console.print(
            f"[bold]{t('cli.models_usage.api_base')}:[/bold] {effective_config.api_base}"
        )

    if status is not None:
        console.print(
            f"[bold]{t('cli.models_usage.status')}:[/bold] [{status.style}]{status.summary}[/{status.style}]"
        )
        for detail in status.details:
            console.print(f"  {detail}", style="dim")
    else:
        api_key = effective_config.api_key
        api_key_env_var = metadata.api_key_env_var
        api_key_env_value = None if api_key else (os.getenv(api_key_env_var) if api_key_env_var else None)
        if api_key:
            console.print(
                f"[bold]{t('cli.models_usage.status')}:[/bold] [green]{t('cli.models_usage.api_key_configured')}[/green]"
            )
            console.print(
                f"  {t('cli.models_usage.api_key_masked', masked=_mask_secret(api_key))}",
                style="dim",
            )
        elif api_key_env_value:
            console.print(
                f"[bold]{t('cli.models_usage.status')}:[/bold] [green]{t('cli.models_usage.api_key_from_env', env_var=api_key_env_var)}[/green]"
            )
            console.print(
                f"  {t('cli.models_usage.api_key_masked', masked=_mask_secret(api_key_env_value))}",
                style="dim",
            )
        else:
            console.print(
                f"[bold]{t('cli.models_usage.status')}:[/bold] [yellow]{t('cli.models_usage.not_configured')}[/yellow]"
            )
            if api_key_env_var:
                console.print(
                    f"  {t('cli.models_usage.api_key_hint', env_var=api_key_env_var)}",
                    style="dim",
                )

    if metadata.dashboard_url:
        console.print(
            f"[bold]{t('cli.models_usage.dashboard')}:[/bold] {metadata.dashboard_url}"
        )
    else:
        console.print(
            f"[bold]{t('cli.models_usage.dashboard')}:[/bold] {t('cli.models_usage.dashboard_unavailable')}",
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
    from pathlib import Path

    script_path = Path(__file__).parent.parent / "check_langfuse.py"

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
aish models auth --provider openai-codex

# Use built-in device-code auth on headless servers
aish models auth --provider openai-codex --auth-flow device-code

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


def main():
    """Main entry point for the CLI"""
    app()


if __name__ == "__main__":
    from dotenv import load_dotenv

    load_dotenv()
    main()