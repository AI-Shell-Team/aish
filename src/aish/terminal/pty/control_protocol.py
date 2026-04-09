"""Backend control protocol helpers for the persistent bash PTY."""

from __future__ import annotations

import json
from dataclasses import dataclass
from typing import Any


class ControlProtocolError(ValueError):
    """Raised when a backend control event cannot be decoded."""


@dataclass(slots=True)
class BackendControlEvent:
    """A decoded event emitted by the bash backend control channel."""

    version: int
    type: str
    ts: int | float | None
    payload: dict[str, Any]

    @classmethod
    def from_mapping(cls, value: dict[str, Any]) -> "BackendControlEvent":
        event_type = value.get("type")
        if not isinstance(event_type, str) or not event_type:
            raise ControlProtocolError("control event missing valid 'type'")

        version = value.get("version", 1)
        if not isinstance(version, int):
            raise ControlProtocolError("control event has non-integer 'version'")

        ts = value.get("ts")
        if ts is not None and not isinstance(ts, (int, float)):
            raise ControlProtocolError("control event has invalid 'ts'")

        payload = {
            key: item
            for key, item in value.items()
            if key not in {"version", "type", "ts"}
        }
        return cls(version=version, type=event_type, ts=ts, payload=payload)


def parse_control_event_line(line: bytes | str) -> BackendControlEvent:
    """Decode a single newline-delimited JSON control event."""
    if isinstance(line, bytes):
        try:
            text = line.decode("utf-8")
        except UnicodeDecodeError as error:
            raise ControlProtocolError("control event is not valid UTF-8") from error
    else:
        text = line

    text = text.strip()
    if not text:
        raise ControlProtocolError("control event line is empty")

    try:
        value = json.loads(text)
    except json.JSONDecodeError as error:
        raise ControlProtocolError(f"invalid control event JSON: {error.msg}") from error

    if not isinstance(value, dict):
        raise ControlProtocolError("control event must decode to an object")

    return BackendControlEvent.from_mapping(value)


def decode_control_chunk(
    buffer: bytes,
    chunk: bytes,
) -> tuple[list[BackendControlEvent], bytes, list[str]]:
    """Decode as many NDJSON control events as possible from a byte chunk."""
    combined = buffer + chunk
    if not combined:
        return [], b"", []

    parts = combined.split(b"\n")
    remainder = parts.pop()
    events: list[BackendControlEvent] = []
    errors: list[str] = []

    for raw_line in parts:
        line = raw_line.rstrip(b"\r")
        if not line:
            continue
        try:
            events.append(parse_control_event_line(line))
        except ControlProtocolError as error:
            errors.append(str(error))

    return events, remainder, errors