"""Interactive setup wizard and tool-call verification helpers."""

from __future__ import annotations

import io
import json
import os
import shutil
import sys
from typing import Optional
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen

import anyio
from rich import box
from rich.console import Console
from rich.panel import Panel
from rich.progress import (Progress, SpinnerColumn, TextColumn,
                           TimeElapsedColumn)
from rich.table import Table

from ..config import Config, ConfigModel
from ..i18n import t
from ..litellm_loader import load_litellm, preload_litellm
from ..providers.openai_codex import is_openai_codex_model
from .constants import (_HUGGINGFACE_DEFAULT_MODEL,
                        _KILOCODE_DEFAULT_MODEL, _MISTRAL_DEFAULT_MODEL,
                        _OLLAMA_DEFAULT_MODEL, _QIANFAN_MODELS,
                        _QWEN_DEFAULT_MODEL, _STATIC_FILTER_SKIP_PROVIDERS,
                        _TOGETHER_DEFAULT_MODEL, _VLLM_DEFAULT_MODEL,
                        _XAI_DEFAULT_MODEL)
from .helpers import (_ask_value, _display_width, _is_blank, _is_valid_url,
                      _mask_secret, _matches_filter_query,
                      _prompt_secret_with_mask, _sanitize_filter_input)
from .provider_helpers import (ProviderEndpointInfo, get_provider_endpoints,
                               get_provider_models, has_multi_endpoints)
from .providers import (_filter_provider_options, _get_provider_options,
                        _maybe_resolve_api_base, _provider_note,
                        _with_api_base)
from .types import ProviderOption, ToolSupportResult
from .verification import (_check_tool_support, _quick_static_check,
                           _status_text, build_failure_reason,
                           run_verification)

console = Console()


def _resolve_list_viewport(total: int, selected_index: int) -> tuple[int, int]:
    if total <= 0:
        return (0, 0)

    term_lines = shutil.get_terminal_size(fallback=(80, 24)).lines
    max_visible = max(5, term_lines - 12)
    if total <= max_visible:
        return (0, total)

    half = max_visible // 2
    start = max(0, selected_index - half)
    end = start + max_visible
    if end > total:
        end = total
        start = max(0, end - max_visible)
    return (start, end)


_REALTIME_UNAVAILABLE = object()
_CUSTOM_MODEL_ENTRY = object()


def _render_panel_ansi(panel: Panel) -> str:
    width = max(shutil.get_terminal_size(fallback=(80, 24)).columns, 20)
    buffer = io.StringIO()
    export_console = Console(
        file=buffer,
        record=True,
        force_terminal=True,
        width=width,
    )
    export_console.print(panel)
    return export_console.export_text(styles=True).rstrip("\n")


def _select_provider_realtime(
    options: list[ProviderOption],
) -> ProviderOption | None | object:
    try:
        from prompt_toolkit import Application
        from prompt_toolkit.buffer import Buffer
        from prompt_toolkit.formatted_text import ANSI
        from prompt_toolkit.key_binding import KeyBindings
        from prompt_toolkit.layout import HSplit, Layout, VSplit, Window
        from prompt_toolkit.layout.controls import (BufferControl,
                                                    FormattedTextControl)
        from prompt_toolkit.styles import Style
    except Exception:
        return _REALTIME_UNAVAILABLE

    filtered: list[ProviderOption] = options
    selected_index = 0

    def get_filtered() -> list[ProviderOption]:
        text = buffer.text
        return _filter_provider_options(options, text)

    def render_list() -> list[tuple[str, str]]:
        nonlocal filtered
        filtered = get_filtered()
        if not filtered:
            return [("class:warning", t("cli.setup.filter_no_results"))]

        if selected_index >= len(filtered):
            return [("class:warning", t("cli.setup.filter_no_results"))]

        start, end = _resolve_list_viewport(len(filtered), selected_index)
        lines: list[tuple[str, str]] = []
        if start > 0:
            lines.append(("class:hint", "..."))
            lines.append(("", "\n"))

        for idx in range(start, end):
            provider = filtered[idx]
            style = "class:selected" if idx == selected_index else ""
            note = _provider_note(provider)
            suffix = f"  [{note}]" if note else ""
            lines.append((style, f"{idx + 1}. {provider.label}{suffix}"))
            if idx < end - 1:
                lines.append(("", "\n"))

        if end < len(filtered):
            lines.append(("", "\n"))
            lines.append(("class:hint", "..."))
        return lines

    app_ref: dict[str, Application | None] = {"app": None}

    def handle_text_change(_):
        nonlocal selected_index
        selected_index = 0
        if app_ref["app"] is not None:
            app_ref["app"].invalidate()

    buffer = Buffer(on_text_changed=handle_text_change)

    kb = KeyBindings()

    @kb.add("up")
    def _move_up(event):
        nonlocal selected_index
        if filtered:
            selected_index = max(0, selected_index - 1)
            event.app.invalidate()

    @kb.add("down")
    def _move_down(event):
        nonlocal selected_index
        if filtered:
            selected_index = min(len(filtered) - 1, selected_index + 1)
            event.app.invalidate()

    @kb.add("enter")
    def _select(event):
        if not filtered:
            return
        event.app.exit(result=filtered[selected_index])

    @kb.add("escape")
    @kb.add("c-c")
    def _cancel(event):
        event.app.exit(result=None)

    style = Style.from_dict(
        {
            "selected": "reverse",
            "warning": "yellow",
            "hint": "ansibrightblack",
        }
    )

    filter_label = t("cli.setup.provider_filter_prompt")
    filter_label_text = f"{filter_label} "
    header_panel = ANSI(
        _render_panel_ansi(
            Panel(
                t("cli.setup.provider_header"),
                title=t("cli.setup.step_provider"),
                border_style="blue",
            )
        )
    )
    hint_text = t("cli.setup.provider_filter_hint")

    def render_filter_status() -> str:
        query = _sanitize_filter_input(buffer.text).strip()
        count = len(get_filtered())
        if not query:
            return f"{count} items"
        return f"{query} ({count} matches)"

    layout = Layout(
        HSplit(
            [
                Window(
                    content=FormattedTextControl(header_panel),
                    dont_extend_height=True,
                    always_hide_cursor=True,
                ),
                Window(
                    content=FormattedTextControl(render_filter_status),
                    style="class:hint",
                    wrap_lines=True,
                    dont_extend_height=True,
                    always_hide_cursor=True,
                ),
                VSplit(
                    [
                        Window(
                            content=FormattedTextControl(lambda: filter_label_text),
                            width=_display_width(filter_label_text),
                        ),
                        Window(
                            height=1,
                            content=BufferControl(buffer=buffer),
                        ),
                    ]
                ),
                Window(
                    content=FormattedTextControl(render_list),
                    wrap_lines=True,
                    dont_extend_height=True,
                    always_hide_cursor=True,
                ),
                Window(
                    content=FormattedTextControl(lambda: hint_text),
                    style="class:hint",
                    wrap_lines=True,
                    dont_extend_height=True,
                    always_hide_cursor=True,
                ),
            ]
        )
    )

    app = Application(
        layout=layout,
        key_bindings=kb,
        style=style,
        full_screen=False,
        erase_when_done=True,
    )
    app_ref["app"] = app
    result = app.run()
    return result


def _prompt_custom_provider() -> Optional[ProviderOption]:
    console.print(
        Panel(
            t("cli.setup.custom_api_base_header"),
            title=t("cli.setup.custom_api_base_title"),
            border_style="blue",
        )
    )
    api_base = _prompt_url_with_inline_validation(
        t("cli.setup.provider_custom_api_base"),
        required_message=t("cli.setup.provider_custom_api_base_required"),
    )
    if not api_base:
        return None

    return ProviderOption(
        key="custom",
        label=t("cli.setup.provider_custom_default"),
        api_base=api_base,
        env_key=None,
        requires_api_base=True,
    )


def _select_provider() -> Optional[ProviderOption]:
    providers = _get_provider_options()

    selected = _select_provider_realtime(providers)
    if selected is None:
        return None
    if selected is _REALTIME_UNAVAILABLE:
        console.print("[yellow]Interactive selection requires prompt_toolkit.[/yellow]")
        console.print("[yellow]Please install: pip install prompt_toolkit[/yellow]")
        return None

    if not isinstance(selected, ProviderOption):
        return None

    selected_note = _provider_note(selected)
    selected_suffix = f"  [{selected_note}]" if selected_note else ""
    console.print(
        Panel(
            t("cli.setup.provider_header"),
            title=t("cli.setup.step_provider"),
            border_style="blue",
        )
    )
    console.print(
        f"{t('cli.setup.provider_selected_label')}: {selected.label}{selected_suffix}"
    )

    if selected.requires_api_base:
        return _prompt_custom_provider()

    # Special handling for providers with multiple endpoints
    if has_multi_endpoints(selected.key):
        return _select_provider_endpoint(selected)

    return selected


def _select_provider_endpoint(
    base_provider: ProviderOption,
) -> Optional[ProviderOption]:
    """Show endpoint selection for providers with multiple endpoints."""

    endpoints = get_provider_endpoints(base_provider.key)

    selected = _select_endpoint_realtime(base_provider.key, endpoints)
    if selected is None:
        return None
    if selected is _REALTIME_UNAVAILABLE:
        console.print("[yellow]Interactive selection requires prompt_toolkit.[/yellow]")
        return None

    console.print(
        Panel(
            t("cli.setup.provider_endpoint_header", provider=base_provider.label),
            title=t("cli.setup.step_provider_endpoint"),
            border_style="blue",
        )
    )
    console.print(
        f"{t('cli.setup.provider_endpoint_selected_label')}: {selected.label}"
    )
    # Return a new ProviderOption with the endpoint's api_base
    return ProviderOption(
        key=selected.key,
        label=f"{base_provider.label} - {selected.label}",
        api_base=selected.api_base,
        env_key=base_provider.env_key,
        allow_custom_model=True,
        requires_api_base=False,
    )


def _select_endpoint_realtime(
    provider_key: str,
    endpoints: list[ProviderEndpointInfo],
) -> ProviderEndpointInfo | None | object:
    """Realtime filtered selection for provider endpoints."""
    try:
        from prompt_toolkit import Application
        from prompt_toolkit.buffer import Buffer
        from prompt_toolkit.formatted_text import ANSI
        from prompt_toolkit.key_binding import KeyBindings
        from prompt_toolkit.layout import HSplit, Layout, VSplit, Window
        from prompt_toolkit.layout.controls import (BufferControl,
                                                    FormattedTextControl)
        from prompt_toolkit.styles import Style
    except Exception:
        return _REALTIME_UNAVAILABLE

    filtered: list[ProviderEndpointInfo] = endpoints
    selected_index = 0

    def get_filtered() -> list[ProviderEndpointInfo]:
        text = buffer.text
        if not text.strip():
            return endpoints
        return [
            ep
            for ep in endpoints
            if _matches_filter_query(text, [ep.label, ep.key, ep.hint])
        ]

    def render_list() -> list[tuple[str, str]]:
        nonlocal filtered
        filtered = get_filtered()
        if not filtered:
            return [("class:warning", t("cli.setup.filter_no_results"))]

        if selected_index >= len(filtered):
            return [("class:warning", t("cli.setup.filter_no_results"))]

        start, end = _resolve_list_viewport(len(filtered), selected_index)
        lines: list[tuple[str, str]] = []
        if start > 0:
            lines.append(("class:hint", "..."))
            lines.append(("", "\n"))

        for idx in range(start, end):
            endpoint = filtered[idx]
            style = "class:selected" if idx == selected_index else ""
            lines.append((style, f"{idx + 1}. {endpoint.label}  [{endpoint.hint}]"))
            if idx < end - 1:
                lines.append(("", "\n"))

        if end < len(filtered):
            lines.append(("", "\n"))
            lines.append(("class:hint", "..."))
        return lines

    app_ref: dict[str, Application | None] = {"app": None}

    def handle_text_change(_):
        nonlocal selected_index
        selected_index = 0
        if app_ref["app"] is not None:
            app_ref["app"].invalidate()

    buffer = Buffer(on_text_changed=handle_text_change)

    kb = KeyBindings()

    @kb.add("up")
    def _move_up(event):
        nonlocal selected_index
        if filtered:
            selected_index = max(0, selected_index - 1)
            event.app.invalidate()

    @kb.add("down")
    def _move_down(event):
        nonlocal selected_index
        if filtered:
            selected_index = min(len(filtered) - 1, selected_index + 1)
            event.app.invalidate()

    @kb.add("enter")
    def _select(event):
        if not filtered:
            return
        event.app.exit(result=filtered[selected_index])

    @kb.add("escape")
    @kb.add("c-c")
    def _cancel(event):
        event.app.exit(result=None)

    style = Style.from_dict(
        {
            "selected": "reverse",
            "warning": "yellow",
            "hint": "ansibrightblack",
        }
    )

    filter_label = t("cli.setup.provider_filter_prompt")
    filter_label_text = f"{filter_label} "
    header_panel = ANSI(
        _render_panel_ansi(
            Panel(
                t("cli.setup.provider_endpoint_header", provider=provider_key.upper()),
                title=t("cli.setup.step_provider_endpoint"),
                border_style="blue",
            )
        )
    )
    hint_text = t("cli.setup.provider_filter_hint")

    def render_filter_status() -> str:
        query = _sanitize_filter_input(buffer.text).strip()
        count = len(get_filtered())
        if not query:
            return f"{count} items"
        return f"{query} ({count} matches)"

    layout = Layout(
        HSplit(
            [
                Window(
                    content=FormattedTextControl(header_panel),
                    dont_extend_height=True,
                    always_hide_cursor=True,
                ),
                Window(
                    content=FormattedTextControl(render_filter_status),
                    style="class:hint",
                    wrap_lines=True,
                    dont_extend_height=True,
                    always_hide_cursor=True,
                ),
                VSplit(
                    [
                        Window(
                            content=FormattedTextControl(lambda: filter_label_text),
                            width=_display_width(filter_label_text),
                        ),
                        Window(
                            height=1,
                            content=BufferControl(buffer=buffer),
                        ),
                    ]
                ),
                Window(
                    content=FormattedTextControl(render_list),
                    wrap_lines=True,
                    dont_extend_height=True,
                    always_hide_cursor=True,
                ),
                Window(
                    content=FormattedTextControl(lambda: hint_text),
                    style="class:hint",
                    wrap_lines=True,
                    dont_extend_height=True,
                    always_hide_cursor=True,
                ),
            ]
        )
    )

    app = Application(
        layout=layout,
        key_bindings=kb,
        style=style,
        full_screen=False,
        erase_when_done=True,
    )
    app_ref["app"] = app
    return app.run()


def _prompt_api_key(env_key: Optional[str]) -> Optional[str]:
    console.print(
        Panel(
            t("cli.setup.api_key_header"),
            title=t("cli.setup.step_key"),
            border_style="blue",
        )
    )

    env_value = os.getenv(env_key) if env_key else None
    if env_value:
        console.print(
            t(
                "cli.setup.api_key_env_found",
                env_key=env_key,
                masked=_mask_secret(env_value),
            ),
            style="dim",
        )
        console.print(t("cli.setup.api_key_env_hint"), style="dim")

    while True:
        try:
            value = _prompt_secret_with_mask(f"{t('cli.setup.api_key_prompt')}: ")
        except (KeyboardInterrupt, EOFError):
            return None
        if value is None:
            return None
        value = value.strip()
        if not value and env_value:
            value = env_value.strip()
        if value:
            return value
        console.print(t("cli.setup.api_key_required"), style="red")


def _prompt_api_base_for_retry(current: Optional[str]) -> Optional[str]:
    label = t("cli.setup.retry_api_base_prompt")
    if current:
        label = t("cli.setup.retry_api_base_prompt_with_current", current=current)
    return _prompt_url_with_inline_validation(
        label,
        allow_back=True,
        required_message=t("cli.setup.retry_api_base_required"),
    )


def _prompt_url_with_inline_validation(
    label: str,
    *,
    allow_back: bool = False,
    required_message: Optional[str] = None,
) -> Optional[str]:
    required_text = required_message or t("cli.setup.retry_api_base_required")

    try:
        from prompt_toolkit import Application
        from prompt_toolkit.buffer import Buffer
        from prompt_toolkit.key_binding import KeyBindings
        from prompt_toolkit.layout import HSplit, Layout, VSplit, Window
        from prompt_toolkit.layout.controls import (BufferControl,
                                                    FormattedTextControl)
        from prompt_toolkit.styles import Style

        error_text = ""
        prompt_label = f"{label}: "
        app_ref: dict[str, Application | None] = {"app": None}

        def render_error_line() -> list[tuple[str, str]]:
            if error_text:
                return [("class:error", error_text)]
            return [("", "")]

        def handle_text_change(_):
            nonlocal error_text
            if error_text:
                error_text = ""
                if app_ref["app"] is not None:
                    app_ref["app"].invalidate()

        buffer = Buffer(on_text_changed=handle_text_change)
        kb = KeyBindings()

        @kb.add("enter")
        def _submit(event):
            nonlocal error_text
            normalized = buffer.text.strip()

            if allow_back and normalized.lower() in {"b", "back"}:
                event.app.exit(result=None)
                return

            if not normalized:
                error_text = required_text
                event.app.invalidate()
                return

            if not _is_valid_url(normalized):
                error_text = t("cli.setup.provider_custom_api_base_invalid")
                event.app.invalidate()
                return

            event.app.exit(result=normalized)

        @kb.add("c-c")
        @kb.add("escape")
        def _cancel(event):
            event.app.exit(result=None)

        style = Style.from_dict({"error": "yellow"})
        layout = Layout(
            HSplit(
                [
                    VSplit(
                        [
                            Window(
                                content=FormattedTextControl(lambda: prompt_label),
                                width=_display_width(prompt_label),
                            ),
                            Window(height=1, content=BufferControl(buffer=buffer)),
                        ]
                    ),
                    Window(
                        height=1,
                        dont_extend_height=True,
                        always_hide_cursor=True,
                        content=FormattedTextControl(render_error_line),
                    ),
                ]
            )
        )

        app = Application(
            layout=layout, key_bindings=kb, style=style, full_screen=False
        )
        app_ref["app"] = app
        return app.run()
    except Exception:
        pass

    while True:
        value = _ask_value(label).strip()
        if allow_back and value.lower() in {"b", "back"}:
            return None
        if not value:
            console.print(required_text, style="red")
            continue
        if not _is_valid_url(value):
            console.print(t("cli.setup.provider_custom_api_base_invalid"), style="red")
            continue
        return value


def _normalize_custom_model(value: str) -> str:
    value = value.strip()
    if not value:
        return ""
    if "/" in value:
        value = value.split("/", 1)[-1]
    return f"openai/{value}"


def _normalize_model_input(value: str) -> str:
    return "".join(value.split())


def _normalize_model_for_provider(value: str, provider: ProviderOption) -> str:
    if provider.key in {"custom"}:
        return _normalize_custom_model(value)
    if provider.key == "openrouter" and "/" not in value:
        return _normalize_custom_model(value)

    # Special handling for multi-endpoint providers
    # Check if the provider key is an endpoint key (contains hyphen)
    is_endpoint_key = (
        provider.key
        and "-" in provider.key
        and any(
            provider.key.startswith(prefix)
            for prefix in ["zai-", "minimax-", "moonshot-"]
        )
    )

    if is_endpoint_key and "/" not in value:
        # MiniMax uses Anthropic-compatible API, others use OpenAI-compatible
        if provider.key.startswith("minimax"):
            return f"anthropic/{value.strip()}"
        # Z.AI and Moonshot use OpenAI-compatible API
        return f"openai/{value.strip()}"

    if "/" not in value and provider.key:
        return f"{provider.key}/{value.strip()}"
    return value.strip()


def _extract_models_from_payload(payload: object) -> list[str]:
    if isinstance(payload, dict):
        items = None
        for key in ("data", "models", "result", "model_list"):
            if key in payload:
                items = payload[key]
                break
        if items is None:
            return []
    elif isinstance(payload, list):
        items = payload
    else:
        return []

    if not isinstance(items, (list, tuple)):
        return []

    names: list[str] = []
    for item in items:
        if isinstance(item, str):
            name = item
        elif isinstance(item, dict):
            name = item.get("id") or item.get("name") or item.get("model")
        else:
            name = str(item)
        if name and str(name).strip():
            names.append(str(name))

    seen: set[str] = set()
    unique: list[str] = []
    for name in names:
        if name in seen:
            continue
        seen.add(name)
        unique.append(name)
    return unique


def _fetch_ollama_models(api_base: str) -> list[str]:
    """Fetch models from Ollama /api/tags endpoint."""
    if not api_base:
        return []
    base = api_base.rstrip("/").replace("/v1", "")
    url = f"{base}/api/tags"

    headers = {"Accept": "application/json"}

    try:
        request = Request(url, headers=headers)
        with urlopen(request, timeout=5) as response:
            raw = response.read()
        try:
            payload = json.loads(raw.decode("utf-8", errors="ignore"))
        except json.JSONDecodeError:
            return []
        # Ollama returns {"models": [{"name": "model:tag"}, ...]}
        if isinstance(payload, dict) and "models" in payload:
            models = payload["models"]
            if isinstance(models, list):
                # Extract model names (remove tag suffix if present)
                result = []
                for model in models:
                    name = model.get("name", "")
                    if name:
                        # Remove tag suffix (e.g., "llama3.2:latest" -> "llama3.2")
                        if ":" in name:
                            name = name.split(":")[0]
                        result.append(name)
                return result
        return []
    except (HTTPError, URLError, TimeoutError, ValueError):
        return []


def _fetch_models_from_api_base(api_base: str, api_key: Optional[str]) -> list[str]:
    if not api_base:
        return []
    base = api_base.rstrip("/")
    if base.endswith("/models"):
        candidates = [base]
    else:
        candidates = [f"{base}/models"]
        if not base.endswith("/v1"):
            candidates.append(f"{base}/v1/models")

    headers = {"Accept": "application/json"}
    if api_key:
        headers["Authorization"] = f"Bearer {api_key}"
        headers["X-API-Key"] = api_key

    for url in candidates:
        try:
            request = Request(url, headers=headers)
            with urlopen(request, timeout=5) as response:
                raw = response.read()
            try:
                payload = json.loads(raw.decode("utf-8", errors="ignore"))
            except json.JSONDecodeError:
                continue
            models = _extract_models_from_payload(payload)
            if models:
                return models
        except HTTPError as exc:
            if exc.code == 404:
                continue
            return []
        except (URLError, TimeoutError, ValueError):
            return []
    return []


def _get_models_for_provider(
    provider: ProviderOption, api_key: Optional[str]
) -> list[str]:
    # Special handling for multi-endpoint providers: use predefined model list
    # Extract base provider key from endpoint key (e.g., "minimax-global" -> "minimax")
    if "-" in provider.key:
        base_provider = provider.key.split("-")[0]
        if has_multi_endpoints(base_provider):
            return get_provider_models(base_provider)

    if has_multi_endpoints(provider.key):
        return get_provider_models(provider.key)

    # Providers with predefined model lists (no multi-endpoint but need special handling)
    predefined_list_providers = {
        "qianfan",
        "mistral",
        "together",
        "huggingface",
        "qwen",
        "xai",
        "kilocode",
    }
    if provider.key in predefined_list_providers:
        return get_provider_models(provider.key)

    # Vercel AI Gateway is a proxy, no predefined models
    if provider.key == "ai_gateway":
        return []

    # Ollama: try to fetch from /api/tags endpoint, fallback to predefined list
    if provider.key == "ollama":
        if provider.api_base:
            models = _fetch_ollama_models(provider.api_base)
            if models:
                return models
        return get_provider_models("ollama")

    # vLLM: try to fetch from /models endpoint, fallback to predefined list
    if provider.key == "vllm":
        if provider.api_base:
            models = _fetch_models_from_api_base(provider.api_base, api_key)
            if models:
                return models
        return get_provider_models("vllm")

    if provider.key == "custom":
        if provider.api_base:
            return _fetch_models_from_api_base(provider.api_base, api_key)
        return []
    litellm = load_litellm()
    if litellm is None:
        if provider.api_base:
            return _fetch_models_from_api_base(provider.api_base, api_key)
        return []
    models_by_provider = getattr(litellm, "models_by_provider", None)
    if isinstance(models_by_provider, dict):
        models = models_by_provider.get(provider.key)
        if isinstance(models, (list, tuple, set)):
            return [str(item) for item in models if str(item).strip()]
    if provider.api_base:
        return _fetch_models_from_api_base(provider.api_base, api_key)
    return []


def _filter_models_by_static_support(
    provider: ProviderOption, models: list[str]
) -> list[str]:
    if not models:
        return []
    if provider.key in _STATIC_FILTER_SKIP_PROVIDERS:
        return models
    litellm = load_litellm()
    if litellm is None:
        return models

    supported: list[str] = []
    with Progress(
        SpinnerColumn(),
        TextColumn("{task.description}"),
        TimeElapsedColumn(),
        console=console,
    ) as progress:
        task = progress.add_task(
            t("cli.setup.model_filtering"), total=len(models), start=True
        )
        for model in models:
            normalized = _normalize_model_for_provider(model, provider)
            supports = _quick_static_check(litellm, normalized)
            if supports is True:
                supported.append(model)
            progress.advance(task, 1)

    if supported:
        console.print(
            t("cli.setup.model_filter_result", count=len(supported)), style="dim"
        )
        return supported

    console.print(t("cli.setup.model_filter_empty"), style="yellow")
    return models


def _select_model_realtime(
    models: list[str],
    *,
    header: str,
    title: str,
) -> str | None | object:
    try:
        from prompt_toolkit import Application
        from prompt_toolkit.buffer import Buffer
        from prompt_toolkit.formatted_text import ANSI
        from prompt_toolkit.key_binding import KeyBindings
        from prompt_toolkit.layout import HSplit, Layout, VSplit, Window
        from prompt_toolkit.layout.controls import (BufferControl,
                                                    FormattedTextControl)
        from prompt_toolkit.styles import Style
    except Exception:
        return _REALTIME_UNAVAILABLE

    filtered: list[str | object] = [_CUSTOM_MODEL_ENTRY, *models]
    selected_index = 1 if models else 0
    custom_label = t("cli.setup.model_custom_option")

    def get_filtered() -> list[object]:
        text = _sanitize_filter_input(buffer.text)
        if not text.strip():
            matches = models
        else:
            matches = [item for item in models if _matches_filter_query(text, [item])]
        return [_CUSTOM_MODEL_ENTRY, *matches]

    def render_list() -> list[tuple[str, str]]:
        nonlocal filtered
        filtered = get_filtered()
        if selected_index >= len(filtered):
            return [("class:warning", t("cli.setup.filter_no_results"))]

        start, end = _resolve_list_viewport(len(filtered), selected_index)
        lines: list[tuple[str, str]] = []
        if start > 0:
            lines.append(("class:hint", f"↑ {start} ..."))
            lines.append(("", "\n"))

        for idx in range(start, end):
            model = filtered[idx]
            style = "class:selected" if idx == selected_index else ""
            if model is _CUSTOM_MODEL_ENTRY:
                label = custom_label
            else:
                label = str(model)
            lines.append((style, f"{idx + 1}. {label}"))
            if idx < end - 1:
                lines.append(("", "\n"))

        if end < len(filtered):
            lines.append(("", "\n"))
            lines.append(("class:hint", "..."))
        return lines

    app_ref: dict[str, Application | None] = {"app": None}

    def handle_text_change(_):
        nonlocal selected_index
        new_filtered = get_filtered()
        selected_index = 0 if len(new_filtered) <= 1 else 1
        if app_ref["app"] is not None:
            app_ref["app"].invalidate()

    buffer = Buffer(on_text_changed=handle_text_change)

    kb = KeyBindings()

    @kb.add("up")
    def _move_up(event):
        nonlocal selected_index
        if filtered:
            selected_index = max(0, selected_index - 1)
            event.app.invalidate()

    @kb.add("down")
    def _move_down(event):
        nonlocal selected_index
        if filtered:
            selected_index = min(len(filtered) - 1, selected_index + 1)
            event.app.invalidate()

    @kb.add("enter")
    def _select(event):
        if not filtered:
            return
        selected = filtered[selected_index]
        if selected is _CUSTOM_MODEL_ENTRY:
            custom_value = _sanitize_filter_input(buffer.text).strip()
            if custom_value:
                event.app.exit(result=custom_value)
            else:
                event.app.exit(result=_CUSTOM_MODEL_ENTRY)
            return
        event.app.exit(result=selected)

    @kb.add("escape")
    @kb.add("c-c")
    def _cancel(event):
        event.app.exit(result=None)

    style = Style.from_dict(
        {
            "selected": "reverse",
            "warning": "yellow",
            "hint": "ansibrightblack",
        }
    )

    filter_label = t("cli.setup.model_filter_prompt")
    filter_label_text = f"{filter_label} "
    header_panel = ANSI(
        _render_panel_ansi(Panel(header, title=title, border_style="blue"))
    )
    hint_text = t("cli.setup.model_filter_hint")

    def render_filter_status() -> str:
        query = _sanitize_filter_input(buffer.text).strip()
        count = len(get_filtered()) - 1
        if count < 0:
            count = 0
        if not query:
            return f"{count} items"
        return f"{query} ({count} matches)"

    layout = Layout(
        HSplit(
            [
                Window(
                    content=FormattedTextControl(header_panel),
                    dont_extend_height=True,
                    always_hide_cursor=True,
                ),
                Window(
                    content=FormattedTextControl(render_filter_status),
                    style="class:hint",
                    wrap_lines=True,
                    dont_extend_height=True,
                    always_hide_cursor=True,
                ),
                VSplit(
                    [
                        Window(
                            content=FormattedTextControl(lambda: filter_label_text),
                            width=_display_width(filter_label_text),
                        ),
                        Window(
                            height=1,
                            content=BufferControl(buffer=buffer),
                        ),
                    ]
                ),
                Window(
                    content=FormattedTextControl(render_list),
                    wrap_lines=True,
                    dont_extend_height=True,
                    always_hide_cursor=True,
                ),
                Window(
                    content=FormattedTextControl(lambda: hint_text),
                    style="class:hint",
                    wrap_lines=True,
                    dont_extend_height=True,
                    always_hide_cursor=True,
                ),
            ]
        )
    )

    app = Application(
        layout=layout,
        key_bindings=kb,
        style=style,
        full_screen=False,
        erase_when_done=True,
    )
    app_ref["app"] = app
    return app.run()


def _prompt_model(provider: ProviderOption, api_key: Optional[str]) -> Optional[str]:
    models = _get_models_for_provider(provider, api_key)
    if models:
        model_panel_title = t("cli.setup.step_model")
        model_panel_header = t("cli.setup.model_header", provider=provider.label)
        models = _filter_models_by_static_support(provider, models)
        selection = _select_model_realtime(
            models,
            header=model_panel_header,
            title=model_panel_title,
        )
        if selection is _REALTIME_UNAVAILABLE:
            console.print(t("cli.setup.model_list_unavailable"), style="yellow")
        elif selection is None:
            return None
        elif selection is _CUSTOM_MODEL_ENTRY:
            pass
        else:
            candidate = selection
            if candidate not in models:
                candidate = _normalize_model_input(str(candidate))
            normalized = _normalize_model_for_provider(str(candidate), provider)
            console.print(
                Panel(
                    t("cli.setup.model_header", provider=provider.label),
                    title=t("cli.setup.step_model"),
                    border_style="blue",
                )
            )
            if normalized != str(candidate).strip():
                console.print(
                    t("cli.setup.model_custom_saved_as", model=normalized),
                    style="dim",
                )
            console.print(f"{t('cli.setup.model_selected_label')}: {normalized}")
            return normalized

    while True:
        console.print(
            Panel(
                t("cli.setup.model_header", provider=provider.label),
                title=t("cli.setup.step_model"),
                border_style="blue",
            )
        )
        if provider.key == "openrouter":
            console.print(t("cli.setup.model_hint_openrouter"), style="dim")
        if provider.key == "custom":
            console.print(t("cli.setup.model_hint_custom"), style="dim")
        # Add hint for multi-endpoint providers
        if has_multi_endpoints(provider.key):
            # Try to get default model from endpoint key
            default_model = ""
            if provider.key and "/" not in provider.key:
                # This is a base provider (like "zai"), not an endpoint key
                # Get the first endpoint's default model
                endpoints = get_provider_endpoints(provider.key)
                if endpoints:
                    default_model = endpoints[0].default_model
            if default_model:
                console.print(
                    t("cli.setup.model_hint_provider", default=default_model),
                    style="dim",
                )
        # Add hint for providers with predefined models
        predefined_model_hints = {
            "qianfan": _QIANFAN_MODELS[0] if _QIANFAN_MODELS else "deepseek-v3.2",
            "ollama": _OLLAMA_DEFAULT_MODEL,
            "vllm": _VLLM_DEFAULT_MODEL,
            "mistral": _MISTRAL_DEFAULT_MODEL,
            "together": _TOGETHER_DEFAULT_MODEL,
            "huggingface": _HUGGINGFACE_DEFAULT_MODEL,
            "qwen": _QWEN_DEFAULT_MODEL,
            "xai": _XAI_DEFAULT_MODEL,
            "kilocode": _KILOCODE_DEFAULT_MODEL,
        }
        if provider.key in predefined_model_hints:
            console.print(
                t(
                    "cli.setup.model_hint_provider",
                    default=predefined_model_hints[provider.key],
                ),
                style="dim",
            )
        # Vercel AI Gateway is a proxy, no default model
        if provider.key == "ai_gateway":
            console.print(t("cli.setup.model_hint_custom"), style="dim")

        value = _normalize_model_input(_ask_value(t("cli.setup.model_prompt")))
        if not value:
            console.print(t("cli.setup.model_custom_required"), style="red")
            continue
        if value.lower() in {"b", "back"}:
            return None

        normalized = _normalize_model_for_provider(value, provider)
        if not normalized:
            console.print(t("cli.setup.model_custom_required"), style="red")
            continue
        if normalized != value.strip():
            console.print(
                t("cli.setup.model_custom_saved_as", model=normalized),
                style="dim",
            )
        console.print(f"{t('cli.setup.model_selected_label')}: {normalized}")
        return normalized


def _prompt_setup_action(options: list[tuple[str, str]]) -> str:
    """Prompt user to select an action using arrow keys."""
    return _select_action_realtime(options)


def _select_action_realtime(options: list[tuple[str, str]]) -> str:
    """Select an action from a list using arrow keys."""
    try:
        from prompt_toolkit import Application
        from prompt_toolkit.key_binding import KeyBindings
        from prompt_toolkit.layout import HSplit, Layout, Window
        from prompt_toolkit.layout.controls import FormattedTextControl
        from prompt_toolkit.styles import Style
    except Exception:
        # Fallback to simple prompt selection
        console.print()
        console.print(t("cli.setup.action_header"))
        for idx, (_, label) in enumerate(options, start=1):
            console.print(f"{idx}. {label}")
        while True:
            choice = _ask_value(t("cli.setup.action_prompt")).strip()
            if choice.isdigit():
                idx = int(choice) - 1
                if 0 <= idx < len(options):
                    return options[idx][0]
            console.print(t("cli.setup.invalid_choice"), style="red")

    selected_index = 0
    header_text = t("cli.setup.action_header")

    def render_list() -> list[tuple[str, str]]:
        lines: list[tuple[str, str]] = []
        for idx, (_, label) in enumerate(options):
            style = "class:selected" if idx == selected_index else ""
            lines.append((style, f"  {label}  "))
            if idx < len(options) - 1:
                lines.append(("", "\n"))
        return lines

    kb = KeyBindings()

    @kb.add("up")
    def _move_up(event):
        nonlocal selected_index
        selected_index = max(0, selected_index - 1)
        event.app.invalidate()

    @kb.add("down")
    def _move_down(event):
        nonlocal selected_index
        selected_index = min(len(options) - 1, selected_index + 1)
        event.app.invalidate()

    @kb.add("enter")
    def _select(event):
        event.app.exit(result=options[selected_index][0])

    @kb.add("escape")
    @kb.add("c-c")
    def _cancel(event):
        # Default to first option (usually exit or continue)
        event.app.exit(result=options[0][0])

    style = Style.from_dict(
        {
            "selected": "reverse",
            "hint": "ansibrightblack",
        }
    )

    hint_text = t("cli.ask_user.hint_select")

    layout = Layout(
        HSplit(
            [
                Window(
                    content=FormattedTextControl(lambda: header_text),
                    dont_extend_height=True,
                    always_hide_cursor=True,
                ),
                Window(
                    content=FormattedTextControl(render_list),
                    wrap_lines=True,
                    dont_extend_height=True,
                    always_hide_cursor=True,
                ),
                Window(
                    content=FormattedTextControl(lambda: hint_text),
                    style="class:hint",
                    wrap_lines=True,
                    dont_extend_height=True,
                    always_hide_cursor=True,
                ),
            ]
        )
    )

    app = Application(
        layout=layout,
        key_bindings=kb,
        style=style,
        full_screen=False,
        erase_when_done=True,
    )
    return app.run()


def _persist_setup_config(
    *,
    config: Config,
    api_base: Optional[str],
    api_key: str,
    model: str,
) -> None:
    config.set_api_base(api_base)
    config.set_api_key(api_key)
    config.set_model(model)


def _interactive_setup(config: Config) -> Optional[ConfigModel]:
    if not (sys.stdin.isatty() and sys.stdout.isatty()):
        console.print(t("cli.setup.non_interactive"), style="red")
        return None

    # Kick off litellm import in the background while the user picks a
    # provider / enters API key.  By the time we need litellm (model list,
    # verification) the import is usually already done.
    preload_litellm()

    while True:
        provider = _select_provider()
        if provider is None:
            console.print(t("cli.setup.cancelled"), style="yellow")
            return None

        api_key = _prompt_api_key(provider.env_key)
        if api_key is None:
            console.print(t("cli.setup.cancelled"), style="yellow")
            return None
        provider = _maybe_resolve_api_base(provider, api_key=api_key)

        while True:
            model = _prompt_model(provider, api_key)
            if model is None:
                break
            provider = _maybe_resolve_api_base(
                provider,
                api_key=api_key,
                model_hint=model,
            )

            connectivity, tool_support = run_verification(
                model=model,
                api_base=provider.api_base,
                api_key=api_key,
            )

            # Layer 1 failed – model unreachable
            if not connectivity.ok:
                failure_reason = build_failure_reason(connectivity, tool_support)
                console.print(
                    t(
                        "cli.setup.verify_simple_failed_with_reason",
                        reason=failure_reason,
                    ),
                    style="red",
                )
                action = _prompt_setup_action(
                    [
                        ("retry_api_base", t("cli.setup.action_retry_api_base")),
                        ("retry_model", t("cli.setup.action_retry_model")),
                        ("retry_api_key", t("cli.setup.action_retry_api_key")),
                        ("change_provider", t("cli.setup.action_change_provider")),
                        ("exit", t("cli.setup.action_exit")),
                    ]
                )
                if action == "retry_api_base":
                    new_api_base = _prompt_api_base_for_retry(provider.api_base)
                    if new_api_base:
                        provider = _with_api_base(provider, new_api_base)
                    continue
                if action == "retry_model":
                    continue
                if action == "retry_api_key":
                    new_api_key = _prompt_api_key(provider.env_key)
                    if new_api_key is None:
                        console.print(t("cli.setup.cancelled"), style="yellow")
                        return None
                    api_key = new_api_key
                    provider = _maybe_resolve_api_base(
                        provider,
                        api_key=api_key,
                        model_hint=model,
                    )
                    continue
                if action == "change_provider":
                    break
                console.print(t("cli.setup.cancelled"), style="yellow")
                return None

            # Layer 2 – tool support
            if tool_support.supports is True:
                console.print(t("cli.setup.verify_simple_success"), style="green")
                _persist_setup_config(
                    config=config,
                    api_base=provider.api_base,
                    api_key=api_key,
                    model=model,
                )
                console.print(t("cli.setup.saved"), style="green")
                return config.model_config

            failure_reason = build_failure_reason(connectivity, tool_support)
            if tool_support.supports is False:
                console.print(
                    t(
                        "cli.setup.verify_simple_failed_with_reason",
                        reason=failure_reason,
                    ),
                    style="red",
                )
                action = _prompt_setup_action(
                    [
                        ("retry_model", t("cli.setup.action_retry_model")),
                        ("change_provider", t("cli.setup.action_change_provider")),
                        ("exit", t("cli.setup.action_exit")),
                    ]
                )
                if action == "retry_model":
                    continue
                if action == "change_provider":
                    break
                console.print(t("cli.setup.cancelled"), style="yellow")
                return None

            # tool_support.supports is None – inconclusive
            console.print(
                t("cli.setup.verify_simple_failed_with_reason", reason=failure_reason),
                style="red",
            )
            action = _prompt_setup_action(
                [
                    ("retry_model", t("cli.setup.action_retry_model")),
                    ("change_provider", t("cli.setup.action_change_provider")),
                    ("continue", t("cli.setup.action_continue")),
                    ("exit", t("cli.setup.action_exit")),
                ]
            )
            if action == "retry_model":
                continue
            if action == "change_provider":
                break
            if action == "continue":
                _persist_setup_config(
                    config=config,
                    api_base=provider.api_base,
                    api_key=api_key,
                    model=model,
                )
                console.print(t("cli.setup.saved_with_warning"), style="yellow")
                return config.model_config
            console.print(t("cli.setup.cancelled"), style="yellow")
            return None


def needs_interactive_setup(
    raw_config: dict,
    model_arg: Optional[str],
    api_key_arg: Optional[str],
) -> bool:
    effective_model = model_arg if model_arg is not None else raw_config.get("model")
    if model_arg is None and _is_blank(effective_model):
        return True
    if (
        api_key_arg is None
        and _is_blank(raw_config.get("api_key"))
        and not is_openai_codex_model(str(effective_model or ""))
    ):
        return True
    return False


def run_interactive_setup(config: Config) -> Optional[ConfigModel]:
    return _interactive_setup(config)


def run_live_tool_support_check_debug(
    *,
    model: str,
    api_base: Optional[str],
    api_key: Optional[str],
) -> ToolSupportResult:
    litellm = load_litellm()
    if litellm is None:
        console.print(t("cli.setup.litellm_missing"), style="red")
        return ToolSupportResult(supports=None, error=t("cli.setup.litellm_missing"))

    console.print(
        Panel(
            t("cli.setup.verify_header"),
            title=t("cli.setup.verify_title"),
            border_style="blue",
        )
    )
    try:
        result = anyio.run(
            _check_tool_support,
            litellm,
            model,
            api_base,
            api_key,
            8.0,
            True,
        )
    except KeyboardInterrupt:
        console.print(t("cli.setup.verify_cancelled"), style="yellow")
        return ToolSupportResult(supports=None, cancelled=True)
    except Exception as exc:
        from .verification import _compact_error_message

        error_msg = _compact_error_message(exc)
        console.print(error_msg, style="red")
        return ToolSupportResult(supports=None, error=error_msg)

    table = Table(box=box.SIMPLE, show_header=False)
    table.add_row(t("cli.setup.verify_live_label"), _status_text(result.supports))
    if result.error:
        table.add_row(t("cli.setup.verify_live_detail"), result.error)
    console.print(table)
    return result
