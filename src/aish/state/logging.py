from __future__ import annotations

import logging
import os
import sys
from logging.handlers import RotatingFileHandler
from pathlib import Path
from typing import Optional

from ..config import ConfigModel

_DEFAULT_LOG_DIR = Path.home() / ".config" / "aish" / "logs"
_DEFAULT_LOG_FILE = "aish.log"
_DEFAULT_MAX_BYTES = 5 * 1024 * 1024
_DEFAULT_BACKUP_COUNT = 3

_LOGGING_INITIALIZED = False


class _ContextFilter(logging.Filter):
    def __init__(self) -> None:
        super().__init__()
        self.session_uuid: Optional[str] = None

    def set_session_uuid(self, session_uuid: Optional[str]) -> None:
        self.session_uuid = session_uuid

    def filter(self, record: logging.LogRecord) -> bool:
        record.session_uuid = self.session_uuid or "-"
        record.pid = os.getpid()
        return True


_CONTEXT_FILTER = _ContextFilter()


def set_session_uuid(session_uuid: Optional[str]) -> None:
    _CONTEXT_FILTER.set_session_uuid(session_uuid)


def add_context_filter(handler: logging.Handler) -> None:
    handler.addFilter(_CONTEXT_FILTER)


def _add_handler(logger: logging.Logger, handler: logging.Handler) -> None:
    add_context_filter(handler)
    logger.addHandler(handler)


def build_log_formatter() -> logging.Formatter:
    return logging.Formatter(
        "%(asctime)s %(levelname)s %(name)s [pid=%(pid)s session=%(session_uuid)s] %(message)s"
    )


class _SandboxdFormatter(logging.Formatter):
    def format(self, record: logging.LogRecord) -> str:  # pragma: no cover
        level = getattr(record, "levelname", "")
        short = {
            "WARNING": "WARN",
            "ERROR": "ERROR",
            "CRITICAL": "CRIT",
            "INFO": "INFO",
            "DEBUG": "DEBUG",
        }.get(str(level), str(level))
        record.levelname_short = short  # type: ignore[attr-defined]
        return super().format(record)


def build_sandboxd_log_formatter() -> logging.Formatter:
    # Match: 2026-01-28 16:21:15.879 [WARN] ...
    return _SandboxdFormatter(
        "%(asctime)s.%(msecs)03d [%(levelname_short)s] %(name)s [pid=%(pid)s session=%(session_uuid)s] %(message)s",
        datefmt="%Y-%m-%d %H:%M:%S",
    )


def build_sandboxd_rotating_file_handler(log_path: Path, level: int) -> logging.Handler:
    handler = RotatingFileHandler(
        log_path,
        maxBytes=_DEFAULT_MAX_BYTES,
        backupCount=_DEFAULT_BACKUP_COUNT,
        encoding="utf-8",
    )
    handler.setLevel(level)
    handler.setFormatter(build_sandboxd_log_formatter())
    return handler


def build_rotating_file_handler(log_path: Path, level: int) -> logging.Handler:
    handler = RotatingFileHandler(
        log_path,
        maxBytes=_DEFAULT_MAX_BYTES,
        backupCount=_DEFAULT_BACKUP_COUNT,
        encoding="utf-8",
    )
    handler.setLevel(level)
    handler.setFormatter(build_log_formatter())
    return handler


def build_stream_handler(level: int, *, stream) -> logging.Handler:
    handler = logging.StreamHandler(stream)
    handler.setLevel(level)
    handler.setFormatter(build_log_formatter())
    return handler


def _build_file_handler(log_path: Path, level: int) -> logging.Handler:
    return build_rotating_file_handler(log_path, level)


def init_logging(config: ConfigModel) -> logging.Logger:
    global _LOGGING_INITIALIZED

    logger = logging.getLogger("aish")
    if _LOGGING_INITIALIZED:
        return logger

    logger.setLevel(logging.DEBUG)
    logger.propagate = False

    log_dir = Path(getattr(config, "log_dir", _DEFAULT_LOG_DIR)).expanduser()
    log_file = getattr(config, "log_file", _DEFAULT_LOG_FILE)
    log_path = log_dir / log_file

    handlers_added = False

    try:
        log_dir.mkdir(parents=True, exist_ok=True)
        file_handler = _build_file_handler(log_path, logging.DEBUG)
        _add_handler(logger, file_handler)
        handlers_added = True
    except Exception:
        pass

    if not handlers_added:
        logger.addHandler(logging.NullHandler())

    _LOGGING_INITIALIZED = True
    return logger


def init_sandboxd_logging(
    log_path: Optional[Path],
    *,
    level: int = logging.INFO,
    also_stderr: bool = True,
) -> logging.Logger:
    logger = logging.getLogger("aish.sandboxd")
    if logger.handlers:
        return logger

    logger.setLevel(level)
    logger.propagate = False

    if log_path is not None:
        try:
            log_path.parent.mkdir(parents=True, exist_ok=True)
            file_handler = build_sandboxd_rotating_file_handler(log_path, level)
            _add_handler(logger, file_handler)
        except Exception:
            logger.addHandler(logging.NullHandler())

    if also_stderr:
        stream_handler = logging.StreamHandler(sys.stderr)
        stream_handler.setLevel(level)
        stream_handler.setFormatter(build_sandboxd_log_formatter())
        _add_handler(logger, stream_handler)

    return logger
