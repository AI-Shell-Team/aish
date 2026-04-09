"""Welcome screen renderer (figure-2 style).

This module intentionally does NOT depend on `welcome.py` (which is kept as a design sandbox).
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

from rich import box
from rich.cells import cell_len
from rich.console import Group, RenderableType
from rich.panel import Panel
from rich.text import Text

from ... import __version__
from ...config import ConfigModel
from ...i18n import t

_LOGO_RAW_LINES: list[str] = [
    " █████╗ ██╗███████╗██╗  ██╗",
    "██╔══██╗██║██╔════╝██║  ██║",
    "███████║██║███████╗███████║",
    "██╔══██║██║╚════██║██╔══██║",
    "██║  ██║██║███████║██║  ██║",
    "╚═╝  ╚═╝╚═╝╚══════╝╚═╝  ╚═╝",
]

_LOGO_GRAYS: list[str] = [
    "color(250)",
    "color(248)",
    "color(245)",
    "color(243)",
    "color(240)",
    "color(238)",
]


@dataclass(frozen=True)
class WelcomeMeta:
    version: str
    model: str
    config_path: str


def _tilde_path(path: str) -> str:
    """Render a path with ~ if it's under home, otherwise keep absolute."""
    raw = (path or "").strip()
    if not raw:
        return ""

    try:
        p = Path(raw).expanduser()
        home = Path.home()
        try:
            relative = p.resolve().relative_to(home)
        except Exception:
            relative = None
        if relative is not None:
            return str(Path("~") / relative)
        return str(p)
    except Exception:
        return raw


def _get_welcome_meta(config: ConfigModel) -> WelcomeMeta:
    version = f"v{__version__}"
    model = str(getattr(config, "model", "") or "").strip() or "-"

    # `ConfigModel` is `extra = allow`, so CLI can inject this.
    config_file = getattr(config, "config_file", None)
    config_path = _tilde_path(str(config_file)) if config_file else "-"

    return WelcomeMeta(version=version, model=model, config_path=config_path)


def build_welcome_renderable(config: ConfigModel) -> RenderableType:
    """Build the welcome screen renderable.

    Keep it as a single renderable so callers can `console.print(...)` once.
    """

    meta = _get_welcome_meta(config)

    logo_lines: list[RenderableType] = [
        Text(line, no_wrap=True) for line in _LOGO_RAW_LINES
    ]

    header_text = t("shell.welcome2.header", version=meta.version)
    skills_count = getattr(config, "skills_count", None)

    info_text = Text(no_wrap=True)
    info_text.append(f"{t('shell.welcome2.label.model')}: ", style="bold")
    info_text.append(meta.model)
    info_text.append(" " * 12, style="bold")
    info_text.append(t("shell.welcome2.model_hint"))
    info_text.append("\n")
    info_text.append(f"{t('shell.welcome2.label.config')}: ", style="bold")
    info_text.append(meta.config_path)
    if skills_count is not None:
        skills_label = t("cli.startup.label.skills").strip()
        info_text.append("\n")
        info_text.append(f"{skills_label}: ", style="bold")
        info_text.append("#")
        info_text.append(f"{skills_count}", style="bright_green")
        info_text.append(t("shell.welcome2.skills_loaded_suffix"))

    quick_title = Text(t("shell.welcome2.quick_start.title"), style="bold")
    keyword_style = "bold cyan"

    quick_1_prefix = f" • {t('shell.welcome2.quick_start.item1_prefix')}"
    quick_2_prefix = f" • {t('shell.welcome2.quick_start.item2_prefix')}"
    quick_3_prefix = f" • {t('shell.welcome2.quick_start.item3_prefix')}"
    content_start = (
        max(
            cell_len(quick_1_prefix),
            cell_len(quick_2_prefix),
            cell_len(quick_3_prefix),
        )
        + 1
    )

    quick_1 = Text(quick_1_prefix)
    quick_1.append(" " * max(content_start - cell_len(quick_1_prefix), 0))
    quick_1.append(t("shell.welcome2.quick_start.cmd_ls"), style=keyword_style)
    quick_1.append(", ")
    quick_1.append(t("shell.welcome2.quick_start.cmd_top"), style=keyword_style)
    quick_1.append(", ")
    quick_1.append(t("shell.welcome2.quick_start.cmd_vim"), style=keyword_style)
    quick_1.append(", ")
    quick_1.append(t("shell.welcome2.quick_start.cmd_ssh"), style=keyword_style)
    quick_1.append(t("shell.welcome2.quick_start.item1_suffix"))

    quick_2 = Text(quick_2_prefix)
    quick_2.append(" " * max(content_start - cell_len(quick_2_prefix), 0))
    example_text = t("shell.welcome2.quick_start.item2_example")
    parts = example_text.split(";")
    quick_2.append(parts[0] if parts else "")  # Safe handling for empty string
    for part in parts[1:]:
        quick_2.append(";", style=keyword_style)
        quick_2.append(part)

    quick_3 = Text(quick_3_prefix)
    quick_3.append(" " * max(content_start - cell_len(quick_3_prefix), 0))
    quick_3.append(t("shell.welcome2.quick_start.item3_suffix_1"))
    quick_3.append(t("shell.welcome2.quick_start.item3_keyword"), style=keyword_style)
    quick_3.append(t("shell.welcome2.quick_start.item3_suffix_2"))

    info_panel = Panel(
        info_text,
        box=box.ROUNDED,
        padding=(1, 2),
        border_style="white",
        title=Text(header_text),
        title_align="left",
        expand=False,
    )
    risk = Text(t("shell.welcome2.risk"), style="dim")

    return Group(
        "",
        *logo_lines,
        "",
        info_panel,
        "",
        quick_title,
        "",
        quick_1,
        quick_2,
        quick_3,
        "",
        risk,
        "",
    )
