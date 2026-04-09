"""
Configuration management for AI Shell
"""

import os
import shutil
import sys
import tempfile
from pathlib import Path
from typing import Any, Optional, TypedDict, cast

import yaml
from pydantic import BaseModel, ConfigDict, Field, field_validator

from aish.memory.config import MemoryConfig


class ToolArgPreviewSettingsDict(TypedDict):
    enabled: bool
    max_lines: int
    max_chars: int
    max_items: int


TOOL_ARG_PREVIEW_DEFAULTS: ToolArgPreviewSettingsDict = {
    "enabled": False,
    "max_lines": 3,
    "max_chars": 240,
    "max_items": 4,
}


def get_default_aish_data_dir() -> Path:
    """Return default persistent data directory for aish."""
    xdg_data_home = os.environ.get("XDG_DATA_HOME")
    if xdg_data_home:
        base_dir = Path(xdg_data_home).expanduser()
    else:
        base_dir = Path.home() / ".local" / "share"
    return base_dir / "aish"


def get_default_session_db_path() -> str:
    """Return default SQLite path under XDG data home."""
    return str(get_default_aish_data_dir() / "sessions.db")


def _coerce_preview_bool(value: Any, default: bool) -> bool:
    if isinstance(value, bool):
        return value
    if isinstance(value, str):
        lowered = value.strip().lower()
        if lowered in {"1", "true", "yes", "on"}:
            return True
        if lowered in {"0", "false", "no", "off"}:
            return False
    if isinstance(value, (int, float)):
        return bool(value)
    return default


def _coerce_preview_int(value: Any, default: int) -> int:
    try:
        coerced = int(value)
    except (TypeError, ValueError):
        return default
    return coerced if coerced > 0 else default


def _normalize_preview_settings(
    raw: Any, fallback: ToolArgPreviewSettingsDict
) -> ToolArgPreviewSettingsDict:
    if not isinstance(raw, dict):
        return cast(ToolArgPreviewSettingsDict, dict(fallback))

    normalized = cast(ToolArgPreviewSettingsDict, dict(fallback))
    if "enabled" in raw:
        normalized["enabled"] = _coerce_preview_bool(
            raw.get("enabled"), fallback["enabled"]
        )
    if "max_lines" in raw:
        normalized["max_lines"] = _coerce_preview_int(
            raw.get("max_lines"), fallback["max_lines"]
        )
    if "max_chars" in raw:
        normalized["max_chars"] = _coerce_preview_int(
            raw.get("max_chars"), fallback["max_chars"]
        )
    if "max_items" in raw:
        normalized["max_items"] = _coerce_preview_int(
            raw.get("max_items"), fallback["max_items"]
        )
    return normalized


class ToolArgPreviewSettings(BaseModel):
    enabled: bool = Field(default=TOOL_ARG_PREVIEW_DEFAULTS["enabled"])
    max_lines: int = Field(default=TOOL_ARG_PREVIEW_DEFAULTS["max_lines"])
    max_chars: int = Field(default=TOOL_ARG_PREVIEW_DEFAULTS["max_chars"])
    max_items: int = Field(default=TOOL_ARG_PREVIEW_DEFAULTS["max_items"])

    @field_validator("enabled", mode="before")
    @classmethod
    def validate_enabled(cls, v: Any) -> bool:
        default = cls.model_fields["enabled"].default
        return _coerce_preview_bool(v, default)

    @field_validator("max_lines", mode="before")
    @classmethod
    def validate_max_lines(cls, v: Any) -> int:
        default = cls.model_fields["max_lines"].default
        return _coerce_preview_int(v, default)

    @field_validator("max_chars", mode="before")
    @classmethod
    def validate_max_chars(cls, v: Any) -> int:
        default = cls.model_fields["max_chars"].default
        return _coerce_preview_int(v, default)

    @field_validator("max_items", mode="before")
    @classmethod
    def validate_max_items(cls, v: Any) -> int:
        default = cls.model_fields["max_items"].default
        return _coerce_preview_int(v, default)


class BashOutputOffloadSettings(BaseModel):
    enabled: bool = Field(default=True)
    threshold_bytes: int = Field(default=1024, gt=0)
    preview_bytes: int = Field(default=1024, gt=0)
    base_dir: Optional[str] = Field(
        default=None,
        description="Base directory for bash output offload files. Defaults to XDG data path.",
    )
    write_meta: bool = Field(default=True)


class ConfigModel(BaseModel):
    """Pydantic model for AI Shell configuration"""

    model_config = ConfigDict(extra="allow")

    model: str = Field(default="", description="LLM model to use")
    api_base: Optional[str] = Field(
        default=None, description="Custom API base URL (e.g., for OpenRouter)"
    )
    api_key: Optional[str] = Field(default=None, description="API key for the service")
    codex_auth_path: Optional[str] = Field(
        default=None,
        description=(
            "Path to OpenAI Codex auth.json. Defaults to $AISH_CODEX_AUTH_PATH, "
            "$CODEX_HOME/auth.json, or ~/.codex/auth.json"
        ),
    )
    temperature: float = Field(
        default=0.7, ge=0.0, le=2.0, description="Temperature for LLM responses"
    )
    max_tokens: int = Field(
        default=1000, gt=0, description="Maximum tokens for LLM responses"
    )
    prompt_style: str = Field(default="🚀", description="Prompt style character/emoji")
    prompt_theme: str = Field(
        default="compact",
        description="Prompt theme: compact, developer, powerline, minimal, or default (plain prompt)",
    )
    theme: str = Field(default="dark", description="Shell theme (dark/light)")
    auto_suggest: bool = Field(default=True, description="Enable auto-suggestions")
    history_size: int = Field(default=1000, gt=0, description="Maximum history size")
    output_language: Optional[str] = Field(
        default=None,
        description="Output language for AI responses (e.g., 'Chinese', 'English'). If not set, auto-detected from system locale",
    )
    tool_arg_preview: dict[str, ToolArgPreviewSettings] = Field(
        default_factory=lambda: {
            "default": ToolArgPreviewSettings(),
            "final_answer": ToolArgPreviewSettings(enabled=True),
        },
        description="Preview/truncation rules for tool argument display.",
    )
    enable_langfuse: bool = Field(
        default=False, description="Enable Langfuse integration for LLM observability"
    )
    approved_ai_commands: list[str] = Field(
        default_factory=list,
        description=(
            "Exact-match AI commands that are pre-approved and will not require confirmation. "
            "Only applies when sandbox is available."
        ),
    )
    max_llm_messages: int = Field(
        default=50,
        gt=0,
        description="Maximum number of LLM conversation messages to keep in context",
    )
    max_shell_messages: int = Field(
        default=20,
        gt=0,
        description="Maximum number of shell history entries to keep in context",
    )
    context_token_budget: Optional[int] = Field(
        default=None,
        description="Optional token budget limit for context (e.g., 4000). If None, only message count limits apply",
    )
    enable_token_estimation: bool = Field(
        default=True,
        description="Enable tiktoken-based token estimation for context trimming",
    )
    bash_output_offload: BashOutputOffloadSettings = Field(
        default_factory=BashOutputOffloadSettings,
        description="Settings for bash tool output offload behavior.",
    )
    pty_output_keep_bytes: int = Field(
        default=4096,
        gt=0,
        description="Maximum bytes kept in-memory for PTY stdout/stderr before offload.",
    )
    terminal_resize_mode: str = Field(
        default="full",
        description="Terminal resize handling mode: full, pty_only, or off.",
    )
    enable_scripts: bool = Field(
        default=True,
        description="Enable script system (hooks, hot-reload, custom prompts). Set to false to use legacy prompt style.",
    )
    session_db_path: str = Field(
        default_factory=get_default_session_db_path,
        description=(
            "SQLite database path for session records "
            "(default: $XDG_DATA_HOME/aish/sessions.db or ~/.local/share/aish/sessions.db)"
        ),
    )
    is_free_key: bool = Field(
        default=False,
        description="Whether the current configuration uses a free API key",
    )
    memory: MemoryConfig = Field(
        default_factory=MemoryConfig,
        description="Long-term memory configuration",
    )

    @field_validator("tool_arg_preview", mode="before")
    @classmethod
    def normalize_tool_arg_preview(cls, v: Any) -> dict[str, ToolArgPreviewSettings]:
        if not isinstance(v, dict):
            v = {}

        base = _normalize_preview_settings(v.get("default"), TOOL_ARG_PREVIEW_DEFAULTS)
        normalized: dict[str, ToolArgPreviewSettings] = {
            "default": ToolArgPreviewSettings.model_validate(base)
        }

        for key, raw_value in v.items():
            if key == "default":
                continue
            if not isinstance(key, str):
                continue
            tool_settings = _normalize_preview_settings(raw_value, base)
            normalized[key] = ToolArgPreviewSettings.model_validate(tool_settings)

        if "final_answer" not in normalized:
            final_settings = dict(base)
            final_settings["enabled"] = True
            normalized["final_answer"] = ToolArgPreviewSettings.model_validate(
                final_settings
            )

        return normalized

    @field_validator("terminal_resize_mode", mode="before")
    @classmethod
    def normalize_terminal_resize_mode(cls, v: Any) -> str:
        mode = str(v or "full").strip().lower()
        if mode in {"full", "pty_only", "off"}:
            return mode
        return "full"

    @field_validator("api_base", "api_key", mode="before")
    @classmethod
    def sanitize_optional_string_fields(cls, v: Any) -> Optional[str]:
        """Sanitize optional URL and key fields by taking first line and trimming."""
        if v is None:
            return None
        if not isinstance(v, str):
            return v
        first_line = v.split("\n")[0].split("\r")[0]
        return first_line.strip() if first_line.strip() else None

    @field_validator("model", mode="before")
    @classmethod
    def sanitize_model_field(cls, v: Any) -> str:
        """Sanitize model field by taking first line and trimming."""
        if v is None:
            return ""
        if not isinstance(v, str):
            return str(v)
        first_line = v.split("\n")[0].split("\r")[0]
        return first_line.strip()


class Config:
    """Configuration manager for AI Shell"""

    def __init__(self, config_file_path: Optional[str] = None):
        self.is_custom_config = config_file_path is not None

        if config_file_path:
            self.config_file = Path(config_file_path).expanduser().resolve()
            self.config_dir = self.config_file.parent
        else:
            aish_config_dir = os.environ.get("AISH_CONFIG_DIR")
            if aish_config_dir:
                self.config_dir = Path(aish_config_dir).expanduser()
                self.config_file = self.config_dir / "config.yaml"
            else:
                xdg_config_home = os.environ.get("XDG_CONFIG_HOME")
                if xdg_config_home:
                    base_dir = Path(xdg_config_home).expanduser()
                else:
                    is_pytest = any(
                        name == "pytest"
                        or name.startswith("pytest.")
                        or name.startswith("_pytest")
                        for name in sys.modules
                    )
                    if is_pytest:
                        base_dir = Path(tempfile.gettempdir()) / "aish-test-config"
                    else:
                        base_dir = Path.home() / ".config"

                self.config_dir = base_dir / "aish"
                self.config_file = self.config_dir / "config.yaml"

        self.history_file = self.config_dir / "history"

        if not self.is_custom_config:
            self.config_dir.mkdir(parents=True, exist_ok=True)

        self._init_skills_dir()
        self._init_scripts_dir()
        self.config_model = self._load_config()

    def _load_config(self) -> ConfigModel:
        """Load configuration from YAML file"""
        if self.config_file.exists():
            try:
                with open(self.config_file, "r") as f:
                    config_data = yaml.safe_load(f) or {}

                need_save = False
                if isinstance(config_data, dict):
                    session_db_path = config_data.get("session_db_path", "")
                    if isinstance(session_db_path, str) and session_db_path.endswith(
                        "sessions.duckdb"
                    ):
                        new_path = str(Path(session_db_path).with_name("sessions.db"))
                        config_data["session_db_path"] = new_path
                        need_save = True
                    if "verbose" in config_data:
                        config_data.pop("verbose", None)
                        need_save = True
                    if "enable_scripts" not in config_data:
                        config_data["enable_scripts"] = True
                        need_save = True
                    if "prompt_theme" not in config_data:
                        config_data["prompt_theme"] = "compact"
                        need_save = True
                    if "memory" not in config_data:
                        config_data["memory"] = {"enabled": True}
                        need_save = True

                    if need_save:
                        self._save_config_data(config_data)

                return ConfigModel.model_validate(config_data)
            except (yaml.YAMLError, Exception):
                backup_file = self.config_file.with_suffix(".yaml.backup")
                try:
                    self.config_file.rename(backup_file)
                except OSError:
                    pass
                return ConfigModel()

        if self.is_custom_config:
            raise FileNotFoundError(f"Config file not found: {self.config_file}")

        default_config = ConfigModel()
        self._save_config(default_config)
        return default_config

    def _save_config_data(self, config_data: dict) -> None:
        """Save configuration data dict to YAML file (internal use)"""
        try:
            with open(self.config_file, "w") as f:
                yaml.safe_dump(
                    config_data,
                    f,
                    default_flow_style=False,
                    sort_keys=True,
                    indent=2,
                )
        except IOError:
            pass

    def _save_config(self, config_model: ConfigModel) -> None:
        """Save configuration to YAML file"""
        try:
            with open(self.config_file, "w") as f:
                yaml.safe_dump(
                    config_model.model_dump(),
                    f,
                    default_flow_style=False,
                    sort_keys=True,
                    indent=2,
                )
        except IOError:
            pass

    def _init_skills_dir(self) -> None:
        """Initialize default skills directory from system skills."""
        if self.is_custom_config:
            return

        is_pytest = any(
            name == "pytest" or name.startswith("pytest.") or name.startswith("_pytest")
            for name in sys.modules
        )
        if is_pytest:
            return

        user_skills_dir = self.config_dir / "skills"

        if not user_skills_dir.exists():
            try:
                user_skills_dir.mkdir(parents=True, exist_ok=True)
            except (OSError, IOError):
                return

        system_skills_locations = [
            Path("/usr/share/aish/skills"),
            Path("/usr/local/share/aish/skills"),
        ]

        if getattr(sys, "frozen", False) and hasattr(sys, "_MEIPASS"):
            meipass_skills = Path(sys._MEIPASS) / "aish" / "skills"
            if meipass_skills.is_dir():
                system_skills_locations.insert(0, meipass_skills)

        source_skills_dir = None
        for location in system_skills_locations:
            if location.is_dir():
                source_skills_dir = location
                break

        if source_skills_dir is None:
            return

        for skill_entry in source_skills_dir.iterdir():
            if not skill_entry.is_dir():
                continue

            skill_name = skill_entry.name
            dest_skill_path = user_skills_dir / skill_name

            if dest_skill_path.exists():
                continue

            try:
                shutil.copytree(skill_entry, dest_skill_path, symlinks=True)
            except (OSError, IOError, shutil.Error):
                pass

    def _init_scripts_dir(self) -> None:
        """Initialize default script themes from system theme resources."""
        if self.is_custom_config:
            return

        is_pytest = any(
            name == "pytest" or name.startswith("pytest.") or name.startswith("_pytest")
            for name in sys.modules
        )
        if is_pytest:
            return

        user_scripts_dir = self.config_dir / "scripts" / "themes"

        if not user_scripts_dir.exists():
            try:
                user_scripts_dir.mkdir(parents=True, exist_ok=True)
            except (OSError, IOError):
                return

        system_theme_locations = [
            Path("/usr/share/aish/scripts/themes"),
            Path("/usr/local/share/aish/scripts/themes"),
        ]

        if getattr(sys, "frozen", False) and hasattr(sys, "_MEIPASS"):
            meipass_themes = Path(sys._MEIPASS) / "aish" / "scripts" / "themes"
            if meipass_themes.is_dir():
                system_theme_locations.insert(0, meipass_themes)

        source_themes_dir = None
        for location in system_theme_locations:
            if location.is_dir():
                source_themes_dir = location
                break

        if source_themes_dir is None:
            return

        for theme_file in source_themes_dir.iterdir():
            if not theme_file.is_file() or not theme_file.name.endswith(".aish"):
                continue

            dest_script_path = user_scripts_dir / theme_file.name

            if dest_script_path.exists():
                continue

            try:
                shutil.copy2(theme_file, dest_script_path)
            except (OSError, IOError):
                pass

    def save_config(self) -> None:
        """Save current configuration to file"""
        self._save_config(self.config_model)

    def create_example_config(self) -> None:
        """Create an example configuration file with all options"""
        example_config = ConfigModel(
            model="openai/deepseek-chat",
            api_base="https://openrouter.ai/api/v1",
            api_key="your-api-key-here",
            temperature=0.7,
            max_tokens=1000,
            prompt_style="🚀",
            theme="dark",
            auto_suggest=True,
            history_size=1000,
            output_language="English",
        )

        try:
            example_file = self.config_dir / "config.example.yaml"
            with open(example_file, "w") as f:
                yaml.safe_dump(
                    example_config.model_dump(),
                    f,
                    default_flow_style=False,
                    sort_keys=True,
                    indent=2,
                )
        except IOError:
            pass

    def get(self, key: str, default=None):
        """Get configuration value"""
        return getattr(self.config_model, key, default)

    def set(self, key: str, value) -> None:
        """Set configuration value"""
        current_data = self.config_model.model_dump()
        current_data[key] = value
        self.config_model = ConfigModel.model_validate(current_data)
        self.save_config()

    def get_model(self) -> str:
        """Get the current LLM model"""
        return self.config_model.model

    def set_model(self, model: str) -> None:
        """Set the LLM model"""
        self.config_model.model = model
        self.save_config()

    def get_history_file(self) -> Path:
        """Get the history file path"""
        return self.history_file

    def get_prompt_style(self) -> str:
        """Get the prompt style"""
        return self.config_model.prompt_style

    def set_prompt_style(self, style: str) -> None:
        """Set the prompt style"""
        self.config_model.prompt_style = style
        self.save_config()

    def get_prompt_theme(self) -> str:
        """Get the prompt theme name."""
        return self.config_model.prompt_theme

    def set_prompt_theme(self, theme: str) -> None:
        """Set the prompt theme."""
        self.config_model.prompt_theme = theme
        self.save_config()

    def get_enable_scripts(self) -> bool:
        """Get whether scripts system is enabled."""
        return self.config_model.enable_scripts

    def get_output_language(self) -> Optional[str]:
        """Get the output language"""
        return self.config_model.output_language

    def set_output_language(self, language: Optional[str]) -> None:
        """Set the output language"""
        self.config_model.output_language = language
        self.save_config()

    def get_api_base(self) -> Optional[str]:
        """Get the custom API URL"""
        return self.config_model.api_base

    def set_api_base(self, api_base: Optional[str]) -> None:
        """Set the custom API URL"""
        self.config_model.api_base = api_base
        self.save_config()

    def get_api_key(self) -> Optional[str]:
        """Get the API key"""
        return self.config_model.api_key

    def set_api_key(self, api_key: Optional[str]) -> None:
        """Set the API key"""
        self.config_model.api_key = api_key
        self.save_config()

    def is_free_key(self) -> bool:
        """Check if using free API key"""
        return self.config_model.is_free_key

    def set_is_free_key(self, is_free_key: bool) -> None:
        """Set whether using free API key"""
        self.config_model.is_free_key = is_free_key
        self.save_config()

    @property
    def model_config(self) -> ConfigModel:
        """Get the underlying Pydantic model"""
        return self.config_model


_GLOBAL_CONFIG: Optional[Config] = None


def get_global_config() -> Config:
    """Return a lazily created process-global Config."""

    global _GLOBAL_CONFIG
    if _GLOBAL_CONFIG is None:
        _GLOBAL_CONFIG = Config()
    return _GLOBAL_CONFIG


class _LazyConfigProxy:
    def __getattr__(self, name: str):
        return getattr(get_global_config(), name)


config = _LazyConfigProxy()