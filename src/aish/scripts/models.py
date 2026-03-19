"""Pydantic models for script system."""

from __future__ import annotations

import re
from typing import Optional

from pydantic import BaseModel, ConfigDict, Field, field_validator

# Script name pattern: lowercase letters, numbers, hyphens, underscores
_SCRIPT_NAME_RE = re.compile(r"^[a-z0-9_][a-z0-9_-]*$")


class ScriptArgument(BaseModel):
    """Argument definition for a script."""

    name: str = Field(..., description="Argument name (used as $AISH_ARG_<name>)")
    description: Optional[str] = Field(
        default=None, description="Human-readable description of the argument"
    )
    default: Optional[str] = Field(
        default=None, description="Default value if argument not provided"
    )
    required: bool = Field(default=False, description="Whether argument is required")

    @field_validator("name")
    @classmethod
    def validate_name(cls, v: str) -> str:
        v = v.strip()
        if not v:
            raise ValueError("argument name must be non-empty")
        # Convert to valid env var name format
        if not re.match(r"^[a-zA-Z_][a-zA-Z0-9_]*$", v):
            raise ValueError(
                "argument name must be a valid identifier "
                "(letters, numbers, underscores, not starting with a number)"
            )
        return v


class ScriptMetadata(BaseModel):
    """Metadata from script YAML frontmatter."""

    name: str = Field(..., description="Script name (used as command)")
    description: str = Field(
        default="", description="Human-readable description of what the script does"
    )
    version: str = Field(default="1.0.0", description="Script version")
    arguments: list[ScriptArgument] = Field(
        default_factory=list, description="Script arguments"
    )
    type: str = Field(
        default="command", description="Script type: command, hook, or alias"
    )
    hook_event: Optional[str] = Field(
        default=None, description="Hook event name (for type=hook)"
    )

    model_config = ConfigDict(extra="allow")  # Allow extra fields for future use

    @field_validator("name")
    @classmethod
    def validate_name(cls, v: str) -> str:
        v = v.strip().lower()
        if not v:
            raise ValueError("script name must be non-empty")
        if len(v) > 64:
            raise ValueError("script name must be at most 64 characters")
        if not _SCRIPT_NAME_RE.match(v):
            raise ValueError(
                "script name must contain only lowercase letters, numbers, "
                "hyphens, and underscores, and must not start with a hyphen"
            )
        return v

    @field_validator("description")
    @classmethod
    def validate_description(cls, v: str) -> str:
        return v.strip() if v else ""


class Script(BaseModel):
    """Represents a loaded script with its metadata and content."""

    metadata: ScriptMetadata = Field(..., description="Script metadata from frontmatter")
    content: str = Field(..., description="Script body content (without frontmatter)")
    file_path: str = Field(..., description="Absolute path to the script file")
    base_dir: str = Field(..., description="Directory containing the script file")

    @property
    def name(self) -> str:
        """Convenience accessor for script name."""
        return self.metadata.name

    @property
    def is_hook(self) -> bool:
        """Check if this is a hook script."""
        return self.metadata.type == "hook"

    @property
    def hook_event(self) -> Optional[str]:
        """Get hook event name if this is a hook script."""
        return self.metadata.hook_event if self.is_hook else None
