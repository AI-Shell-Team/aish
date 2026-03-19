from __future__ import annotations

import re
from enum import Enum
from typing import Any

from pydantic import (AliasChoices, BaseModel, ConfigDict, Field,
                      field_validator)

_SKILL_NAME_RE = re.compile(
    r"^[a-z0-9](?:[a-z0-9]|-(?=[a-z0-9])){0,62}[a-z0-9]$|^[a-z0-9]$"
)


class SkillMetadata(BaseModel):
    """Agent Skills SKILL.md frontmatter metadata.

    Specification: https://agentskills.io/specification
    """

    name: str = Field(
        ..., description="Skill identifier (lowercase letters/numbers/hyphens; max 64)"
    )
    description: str = Field(
        ..., description="Describes what the skill does and when to use it (max 1024)"
    )
    license: str | None = Field(
        default=None, description="License name or reference to a bundled license file"
    )
    compatibility: str | None = Field(
        default=None,
        description=(
            "Indicates environment requirements (intended product, system packages, "
            "network access, etc.; max 500)"
        ),
    )
    metadata: dict[str, Any] | None = Field(
        default=None, description="Arbitrary key-value mapping for additional metadata"
    )
    allowed_tools: list[str] | None = Field(
        default=None,
        validation_alias=AliasChoices("allowed-tools", "allowed_tools"),
        description=(
            "Space-delimited list of pre-approved tools the skill may use (experimental)"
        ),
    )

    @field_validator("name")
    @classmethod
    def validate_name(cls, v: str) -> str:
        v = v.strip()
        if len(v) > 64:
            raise ValueError("name must be at most 64 characters")
        if not _SKILL_NAME_RE.match(v):
            raise ValueError(
                "name must match: lowercase letters, numbers, and hyphens only; "
                "must not start or end with a hyphen"
            )
        return v

    @field_validator("description")
    @classmethod
    def validate_description(cls, v: str) -> str:
        v = v.strip()
        if not v:
            raise ValueError("description must be non-empty")
        if len(v) > 1024:
            raise ValueError("description must be at most 1024 characters")
        return v

    @field_validator("license")
    @classmethod
    def validate_license(cls, v: str | None) -> str | None:
        if v is None:
            return None
        v = v.strip()
        return v or None

    @field_validator("compatibility")
    @classmethod
    def validate_compatibility(cls, v: str | None) -> str | None:
        if v is None:
            return None
        v = v.strip()
        if not v:
            return None
        if len(v) > 500:
            raise ValueError("compatibility must be at most 500 characters")
        return v

    @field_validator("allowed_tools", mode="before")
    @classmethod
    def parse_allowed_tools(cls, v: Any) -> list[str] | None:
        if v is None:
            return None
        if isinstance(v, str):
            items = [x for x in v.split() if x]
            return items or None
        if isinstance(v, (list, tuple)):
            items: list[str] = []
            for item in v:
                if item is None:
                    continue
                if not isinstance(item, str):
                    raise TypeError("allowed-tools items must be strings")
                s = item.strip()
                if s:
                    items.append(s)
            return items or None
        raise TypeError("allowed-tools must be a string or a list of strings")

    model_config = ConfigDict(extra="allow")  # Forward compatibility


class SkillSource(Enum):
    LOCAL = "local"
    """Deprecated: local skill source from current directory is no longer loaded"""
    USER = "user"
    """User skill source, default in $AISH_CONFIG_DIR/skills"""
    CLAUDE = "claude"
    """Claude skill source, default in $HOME/.claude/skills"""
    BUILTIN = "builtin"
    """Built-in skills shipped with aish"""


class SkillMetadataInfo(SkillMetadata):
    """Skill metadata info used for proto serialization and caching."""

    source: str = Field(..., description="Source location of the skill")
    file_path: str = Field(..., description="Absolute path to the skill file")


class Skill(BaseModel):
    """Represents a loaded skill with its metadata and content"""

    metadata: SkillMetadata = Field(..., description="Skill metadata from frontmatter")
    content: str = Field(..., description="Skill prompt content (without frontmatter)")
    source: SkillSource = Field(..., description="Source location of the skill")
    file_path: str = Field(..., description="Absolute path to the skill file")
    base_dir: str = Field(..., description="Base directory of the skill")


class SkillList(BaseModel):
    """List of skills from one directory"""

    source: SkillSource = Field(..., description="Source of these skills")
    skills: list[Skill] = Field(default_factory=list, description="List of skills")
    root_path: str = Field(..., description="Root directory path")
