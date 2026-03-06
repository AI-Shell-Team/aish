from __future__ import annotations

from dataclasses import dataclass
from typing import Any

from pydantic import ValidationError

from .models import SkillMetadata

_ALLOWED_FRONTMATTER_KEYS = {
    "name",
    "description",
    "license",
    "compatibility",
    "metadata",
    "allowed_tools",
    "allowed-tools",
}


@dataclass(frozen=True)
class SkillValidationResult:
    metadata: SkillMetadata | None
    errors: list[str]
    warnings: list[str]


class SkillValidationError(Exception):
    def __init__(self, errors: list[str]):
        self.errors = errors
        super().__init__("\n".join(errors))


def validate_frontmatter(frontmatter: Any) -> SkillValidationResult:
    errors: list[str] = []
    warnings: list[str] = []

    if not isinstance(frontmatter, dict):
        errors.append("frontmatter must be a YAML mapping/object")
        return SkillValidationResult(metadata=None, errors=errors, warnings=warnings)

    unknown_keys = sorted(
        key for key in frontmatter.keys() if key not in _ALLOWED_FRONTMATTER_KEYS
    )
    if unknown_keys:
        warnings.append(
            "unknown frontmatter keys: "
            + ", ".join(str(key) for key in unknown_keys)
            + " (put custom data under 'metadata')"
        )

    try:
        metadata = SkillMetadata(**frontmatter)
    except ValidationError as exc:
        errors.extend(_format_pydantic_errors(exc))
        return SkillValidationResult(metadata=None, errors=errors, warnings=warnings)

    return SkillValidationResult(metadata=metadata, errors=errors, warnings=warnings)


def _format_pydantic_errors(exc: ValidationError) -> list[str]:
    errors: list[str] = []
    for err in exc.errors():
        loc = ".".join(str(item) for item in err.get("loc", [])) or "frontmatter"
        msg = err.get("msg", "invalid value")
        errors.append(f"{loc}: {msg}")
    return errors
