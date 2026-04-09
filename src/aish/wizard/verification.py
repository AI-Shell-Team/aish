"""Two-layer verification for setup wizard.

Layer 1 – Connectivity: can we reach the model endpoint and get a response?
Layer 2 – Tool support: does the model handle tool/function-calling?
"""

from __future__ import annotations

import io
import json
import time
from contextlib import redirect_stderr
from typing import Optional

import anyio
from rich.console import Console
from rich.progress import Progress, ProgressColumn, SpinnerColumn, Task, TextColumn
from rich.text import Text

from ..i18n import t
from ..llm.loader import load_litellm
from .helpers import _mask_secret
from .types import ConnectivityResult, ToolSupportResult

console = Console()

# ---------------------------------------------------------------------------
# Shared helpers
# ---------------------------------------------------------------------------

CONNECTIVITY_PROMPT = "Reply with OK."
CONNECTIVITY_TIMEOUT = 15.0
TOOL_CHECK_TIMEOUT = 30.0


def _validate_model_name(model: str, api_base: Optional[str]) -> str | None:
    trimmed = (model or "").strip()
    if not trimmed:
        return t("cli.setup.model_custom_required")
    if api_base and "/" not in trimmed:
        return t("cli.setup.verify_model_prefix_required", model=trimmed)
    return None


def _compact_error_message(error: object, *, max_len: int = 180) -> str:
    if error is None:
        return t("cli.setup.verify_failed_unknown")
    text = str(error).strip()
    if not text:
        return t("cli.setup.verify_failed_unknown")
    first_line = text.splitlines()[0].strip() or text
    if " - {" in first_line:
        first_line = first_line.split(" - {", 1)[0].strip()
    if len(first_line) > max_len:
        return first_line[: max_len - 1] + "…"
    return first_line


def _status_text(supported: Optional[bool]) -> Text:
    if supported is True:
        return Text(t("cli.setup.support_yes"), style="green")
    if supported is False:
        return Text(t("cli.setup.support_no"), style="red")
    return Text(t("cli.setup.support_unknown"), style="yellow")


def _connectivity_status_text(ok: bool) -> Text:
    if ok:
        return Text(t("cli.setup.support_yes"), style="green")
    return Text(t("cli.setup.support_no"), style="red")


class _MMSSProgressColumn(ProgressColumn):
    """Render elapsed time as mm:ss for short setup checks."""

    def render(self, task: Task) -> Text:
        elapsed = task.finished_time or task.elapsed or 0.0
        total_seconds = max(0, int(elapsed))
        minutes, seconds = divmod(total_seconds, 60)
        return Text(f"{minutes:02d}:{seconds:02d}", style="progress.elapsed")


# ---------------------------------------------------------------------------
# Debug helpers
# ---------------------------------------------------------------------------


def _serialize_response(response: object) -> object:
    if response is None:
        return None
    if isinstance(response, dict):
        return response
    if hasattr(response, "model_dump"):
        return response.model_dump()
    if hasattr(response, "dict"):
        return response.dict()
    return {"repr": repr(response)}


def _print_debug_block(title: str, payload: object, border_style: str = "blue") -> None:
    from rich.panel import Panel

    console.print(Panel(title, border_style=border_style))
    try:
        serialized = json.dumps(payload, ensure_ascii=False, default=str, indent=2)
        console.print(serialized)
    except Exception:
        console.print(payload)


# ---------------------------------------------------------------------------
# Layer 1 – Connectivity check
# ---------------------------------------------------------------------------


async def _check_connectivity(
    litellm,
    model: str,
    api_base: Optional[str],
    api_key: Optional[str],
    timeout_seconds: float = CONNECTIVITY_TIMEOUT,
    debug: bool = False,
) -> ConnectivityResult:
    """Send a minimal completion request to verify the model is reachable."""
    model_error = _validate_model_name(model, api_base)
    if model_error:
        return ConnectivityResult(ok=False, error=model_error)

    messages = [{"role": "user", "content": CONNECTIVITY_PROMPT}]
    kwargs = dict(
        model=model,
        api_base=api_base,
        api_key=api_key,
        messages=messages,
        max_tokens=16,
        stream=False,
        timeout=timeout_seconds,
    )

    if debug:
        debug_payload = dict(kwargs)
        if debug_payload.get("api_key"):
            debug_payload["api_key"] = _mask_secret(str(debug_payload["api_key"]))
        _print_debug_block(
            t("cli.setup.debug_request_title", mode="connectivity"),
            debug_payload,
        )

    start = time.monotonic()
    try:
        response = await litellm.acompletion(**kwargs)
        latency_ms = int((time.monotonic() - start) * 1000)
        if debug:
            _print_debug_block(
                t("cli.setup.debug_response_title", mode="connectivity"),
                _serialize_response(response),
            )
        return ConnectivityResult(ok=True, latency_ms=latency_ms)
    except TimeoutError:
        return ConnectivityResult(
            ok=False,
            timed_out=True,
            error=t("cli.setup.verify_timeout"),
        )
    except Exception as exc:
        message = _compact_error_message(exc)
        if debug:
            _print_debug_block(
                t("cli.setup.debug_error_title", mode="connectivity"),
                message,
                border_style="red",
            )
        return ConnectivityResult(ok=False, error=message)


# ---------------------------------------------------------------------------
# Layer 2 – Tool support check
# ---------------------------------------------------------------------------


def _quick_static_check(litellm, model: str) -> Optional[bool]:
    """Fast, offline heuristic via litellm metadata. Returns True/False/None."""
    supports_function = None
    supports_params = None

    try:
        if hasattr(litellm, "supports_function_calling"):
            supports_function = litellm.supports_function_calling(model=model)
    except Exception:
        pass

    try:
        if hasattr(litellm, "get_supported_openai_params"):
            raw = litellm.get_supported_openai_params(model)
            if raw is not None:
                if isinstance(raw, dict):
                    params_set = {str(k).lower() for k in raw.keys()}
                elif isinstance(raw, (list, tuple, set)):
                    params_set = {str(p).lower() for p in raw}
                else:
                    params_set = set()
                if params_set:
                    supports_params = bool(
                        {"tools", "tool_choice", "functions", "function_call"}
                        & params_set
                    )
    except Exception:
        pass

    if supports_function is True or supports_params is True:
        return True
    if supports_function is False or supports_params is False:
        # One explicitly says False and the other didn't say True
        if supports_function is not True and supports_params is not True:
            return False
    return None


def _coerce_response_message(response: object) -> dict:
    if response is None:
        return {}
    if isinstance(response, dict):
        data = response
    elif hasattr(response, "model_dump"):
        data = response.model_dump()
    elif hasattr(response, "dict"):
        data = response.dict()
    else:
        return {}
    choices = data.get("choices")
    if not isinstance(choices, list) or not choices:
        return {}
    first = choices[0]
    if not isinstance(first, dict):
        return {}
    return first.get("message", {})


def _error_indicates_no_tool_support(message: str) -> bool:
    lowered = message.lower()
    if "tool_choice" in lowered or "tools" in lowered:
        return any(
            term in lowered for term in ("unknown", "unsupported", "not supported")
        )
    if "function_call" in lowered or "functions" in lowered:
        return any(
            term in lowered for term in ("unknown", "unsupported", "not supported")
        )
    return False


def _error_indicates_tool_choice_unsupported(message: str) -> bool:
    lowered = message.lower()
    if "tool_choice" not in lowered:
        return False
    return any(
        term in lowered
        for term in ("unknown", "unsupported", "not supported", "invalid")
    )


async def _check_tool_support(
    litellm,
    model: str,
    api_base: Optional[str],
    api_key: Optional[str],
    timeout_seconds: float = TOOL_CHECK_TIMEOUT,
    debug: bool = False,
) -> ToolSupportResult:
    """Live tool-call probe: send a ping tool and see if the model invokes it."""
    model_error = _validate_model_name(model, api_base)
    if model_error:
        return ToolSupportResult(supports=None, error=model_error)

    # Quick static pre-check – if litellm metadata says "no", skip the live call.
    static = _quick_static_check(litellm, model)
    if static is False:
        return ToolSupportResult(
            supports=False,
            error=t("cli.setup.verify_static_no_tool_support"),
        )

    tools = [
        {
            "type": "function",
            "function": {
                "name": "ping",
                "description": "Return pong.",
                "parameters": {
                    "type": "object",
                    "properties": {},
                    "required": [],
                },
            },
        }
    ]
    messages = [{"role": "user", "content": "Call the ping tool with no arguments."}]

    def _parse(response: object, *, strict: bool) -> ToolSupportResult:
        msg = _coerce_response_message(response)
        if msg.get("tool_calls") or msg.get("function_call"):
            return ToolSupportResult(supports=True)
        if strict:
            return ToolSupportResult(
                supports=False,
                error=t("cli.setup.verify_no_tool_call_required"),
            )
        return ToolSupportResult(
            supports=None,
            error=t("cli.setup.verify_no_tool_call_auto"),
        )

    async def _call(tool_choice, mode_label: str):
        kwargs = dict(
            model=model,
            api_base=api_base,
            api_key=api_key,
            messages=messages,
            tools=tools,
            max_tokens=1000,
            stream=False,
            timeout=timeout_seconds,
        )
        if tool_choice is not None:
            kwargs["tool_choice"] = tool_choice
        if debug:
            debug_payload = dict(kwargs)
            if debug_payload.get("api_key"):
                debug_payload["api_key"] = _mask_secret(str(debug_payload["api_key"]))
            _print_debug_block(
                t("cli.setup.debug_request_title", mode=mode_label),
                debug_payload,
            )
        response = await litellm.acompletion(**kwargs)
        if debug:
            _print_debug_block(
                t("cli.setup.debug_response_title", mode=mode_label),
                _serialize_response(response),
            )
        return response

    # Runtime uses tool_choice=auto. Verify the same path instead of a stricter
    # mode that some gateways do not implement consistently.
    try:
        with anyio.fail_after(timeout_seconds):
            response = await _call(None, "auto")
        return _parse(response, strict=False)
    except TimeoutError:
        return ToolSupportResult(
            supports=None,
            timed_out=True,
            error=t("cli.setup.verify_timeout"),
        )
    except Exception as exc:
        raw_message = str(exc)
        message = _compact_error_message(raw_message)
        if debug:
            _print_debug_block(
                t("cli.setup.debug_error_title", mode="auto"),
                message,
                border_style="red",
            )
        if _error_indicates_no_tool_support(raw_message):
            return ToolSupportResult(supports=False, error=message)

        return ToolSupportResult(supports=None, error=message)


# ---------------------------------------------------------------------------
# Combined verification runner
# ---------------------------------------------------------------------------


async def _run_two_layer_verification(
    litellm,
    model: str,
    api_base: Optional[str],
    api_key: Optional[str],
) -> tuple[ConnectivityResult, ToolSupportResult]:
    """Run Layer 1 (connectivity) then Layer 2 (tool support) with progress."""
    connectivity = ConnectivityResult(ok=False)
    tool_support = ToolSupportResult(supports=None)

    with Progress(
        SpinnerColumn(),
        TextColumn("{task.description}"),
        _MMSSProgressColumn(),
        transient=True,
        console=console,
    ) as progress:
        task = progress.add_task(
            t("cli.setup.verify_connectivity_in_progress"), total=2, start=True
        )

        # Layer 1 – connectivity
        connectivity = await _check_connectivity(
            litellm,
            model=model,
            api_base=api_base,
            api_key=api_key,
        )
        progress.advance(task, 1)

        if not connectivity.ok:
            # Skip Layer 2 when the model is unreachable
            progress.advance(task, 1)
            return connectivity, tool_support

        # Layer 2 – tool support
        progress.update(task, description=t("cli.setup.verify_tool_in_progress"))
        tool_support = await _check_tool_support(
            litellm,
            model=model,
            api_base=api_base,
            api_key=api_key,
        )
        progress.advance(task, 1)

    return connectivity, tool_support


async def run_verification_async(
    *,
    model: str,
    api_base: Optional[str],
    api_key: Optional[str],
) -> tuple[ConnectivityResult, ToolSupportResult]:
    """Async entry point for the 2-layer verification."""
    litellm = load_litellm()
    if litellm is None:
        connectivity = ConnectivityResult(
            ok=False,
            error=t("cli.setup.litellm_missing"),
        )
        tool_support = ToolSupportResult(
            supports=None,
            error=t("cli.setup.litellm_missing"),
        )
        return connectivity, tool_support

    with redirect_stderr(io.StringIO()):
        try:
            return await _run_two_layer_verification(
                litellm,
                model,
                api_base,
                api_key,
            )
        except KeyboardInterrupt:
            connectivity = ConnectivityResult(ok=False, cancelled=True)
            tool_support = ToolSupportResult(supports=None, cancelled=True)
            return connectivity, tool_support
        except Exception as exc:
            error_msg = _compact_error_message(exc)
            connectivity = ConnectivityResult(ok=False, error=error_msg)
            tool_support = ToolSupportResult(supports=None, error=error_msg)
            return connectivity, tool_support


async def _run_verification_async_entry(
    model: str,
    api_base: Optional[str],
    api_key: Optional[str],
) -> tuple[ConnectivityResult, ToolSupportResult]:
    return await run_verification_async(
        model=model,
        api_base=api_base,
        api_key=api_key,
    )


def run_verification(
    *,
    model: str,
    api_base: Optional[str],
    api_key: Optional[str],
) -> tuple[ConnectivityResult, ToolSupportResult]:
    """Synchronous entry point for the 2-layer verification."""
    connectivity, tool_support = anyio.run(
        _run_verification_async_entry,
        model,
        api_base,
        api_key,
    )
    if connectivity.error == t("cli.setup.litellm_missing"):
        console.print(t("cli.setup.litellm_missing"), style="red")
    if connectivity.cancelled or tool_support.cancelled:
        console.print(t("cli.setup.verify_cancelled"), style="yellow")
    return connectivity, tool_support


def build_failure_reason(
    connectivity: ConnectivityResult,
    tool_support: ToolSupportResult,
) -> str:
    """Build a user-facing failure reason from the 2-layer results."""
    for candidate in (connectivity.error, tool_support.error):
        if candidate and str(candidate).strip():
            return str(candidate).strip()
    if connectivity.cancelled or tool_support.cancelled:
        return t("cli.setup.verify_cancelled")
    if connectivity.timed_out or tool_support.timed_out:
        return t("cli.setup.verify_timeout")
    return t("cli.setup.verify_failed_unknown")
