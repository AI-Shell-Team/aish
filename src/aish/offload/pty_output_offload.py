from __future__ import annotations

import datetime as dt
import hashlib
import json
import os
import threading
from dataclasses import dataclass
from pathlib import Path

from aish.config import get_default_aish_data_dir

_EXEC_SEQ_LOCK = threading.Lock()
_EXEC_SEQ = 0


def _next_exec_seq() -> int:
    global _EXEC_SEQ
    with _EXEC_SEQ_LOCK:
        _EXEC_SEQ += 1
        return _EXEC_SEQ


def _safe_session_dir_name(session_uuid: str) -> str:
    value = "".join(
        ch if (ch.isalnum() or ch in {"-", "_", "."}) else "_"
        for ch in str(session_uuid or "unknown-session")
    ).strip("_")
    return value or "unknown-session"


def _build_exec_id(now: dt.datetime, pid: int, seq: int) -> str:
    return f"{now.strftime('%Y%m%dT%H%M%S')}.{now.microsecond // 1000:03d}_{pid}_{seq}"


def _build_uid(command: str, now: dt.datetime, pid: int, seq: int) -> str:
    payload = f"{now.timestamp()}:{pid}:{seq}:{command}"
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()[:16]


def _mkdir_700(path: Path) -> None:
    path.mkdir(parents=True, exist_ok=True)
    os.chmod(path, 0o700)


def _open_binary_append(path: Path):
    flags = os.O_WRONLY | os.O_CREAT | os.O_APPEND
    fd = os.open(path, flags, 0o600)
    return os.fdopen(fd, "ab")


def _write_text_utf8(path: Path, content: str) -> None:
    flags = os.O_WRONLY | os.O_CREAT | os.O_TRUNC
    fd = os.open(path, flags, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as handle:
        handle.write(content)


@dataclass(slots=True)
class PtyStreamOffloadResult:
    status: str = "inline"
    path: str = ""
    clean_path: str = ""
    error: str = ""
    clean_error: str = ""
    bytes_written: int = 0
    truncated: bool = False


@dataclass(slots=True)
class PtyOutputOffloadResult:
    stdout: PtyStreamOffloadResult
    stderr: PtyStreamOffloadResult
    meta_path: str = ""


class PtyOutputOffload:
    def __init__(
        self,
        *,
        command: str,
        session_uuid: str,
        cwd: str,
        keep_len: int,
        base_dir: str | None = None,
    ) -> None:
        self.command = command
        self.session_uuid = session_uuid
        self.cwd = cwd
        self.keep_len = keep_len
        self.base_dir = (
            Path(base_dir).expanduser()
            if base_dir
            else (get_default_aish_data_dir() / "offload")
        )

        self._now = dt.datetime.now()
        self._pid = os.getpid()
        self._seq = _next_exec_seq()
        self._exec_id = _build_exec_id(self._now, self._pid, self._seq)
        self._uid = _build_uid(command, self._now, self._pid, self._seq)

        self._layout_ready = False
        self._layout_error = ""
        self._exec_dir: Path | None = None
        self._stdout_handle = None
        self._stderr_handle = None
        self._meta_path: Path | None = None

        self.stdout = PtyStreamOffloadResult()
        self.stderr = PtyStreamOffloadResult()

    def _ensure_layout(self) -> bool:
        if self._layout_ready:
            return True
        if self._layout_error:
            return False

        try:
            session_dir = self.base_dir / _safe_session_dir_name(self.session_uuid)
            exec_dir = session_dir / self._exec_id
            _mkdir_700(self.base_dir)
            _mkdir_700(session_dir)
            _mkdir_700(exec_dir)
            self._exec_dir = exec_dir
            self._meta_path = exec_dir / f"result_{self._uid}.pty.json"
            self._layout_ready = True
            return True
        except Exception as exc:
            self._layout_error = str(exc)
            return False

    def _ensure_handle(self, stream_name: str):
        if not self._ensure_layout():
            return None

        handle = self._stdout_handle if stream_name == "stdout" else self._stderr_handle
        if handle is not None:
            return handle

        result = self.stdout if stream_name == "stdout" else self.stderr
        assert self._exec_dir is not None
        file_name = f"{stream_name}.pty.txt"
        file_path = self._exec_dir / file_name
        try:
            handle = _open_binary_append(file_path)
            if stream_name == "stdout":
                self._stdout_handle = handle
            else:
                self._stderr_handle = handle
            result.path = str(file_path.resolve())
            result.status = "offloaded"
            return handle
        except Exception as exc:
            result.status = "failed"
            result.error = str(exc)
            return None

    def _mark_failed_by_layout(self, result: PtyStreamOffloadResult) -> None:
        if result.status == "failed":
            return
        result.status = "failed"
        result.error = self._layout_error or "offload layout unavailable"

    def append_overflow(self, *, stream_name: str, overflow: bytes) -> None:
        if not overflow:
            return
        result = self.stdout if stream_name == "stdout" else self.stderr
        result.truncated = True
        handle = self._ensure_handle(stream_name)
        if handle is None:
            if self._layout_error:
                self._mark_failed_by_layout(result)
            return
        try:
            handle.write(overflow)
            result.bytes_written += len(overflow)
        except Exception as exc:
            result.status = "failed"
            result.error = str(exc)

    def _append_tail(self, *, stream_name: str, tail: bytes) -> None:
        result = self.stdout if stream_name == "stdout" else self.stderr
        if not result.truncated:
            return
        if result.status == "failed":
            return
        handle = self._ensure_handle(stream_name)
        if handle is None:
            if self._layout_error:
                self._mark_failed_by_layout(result)
            return
        try:
            handle.write(tail)
            result.bytes_written += len(tail)
        except Exception as exc:
            result.status = "failed"
            result.error = str(exc)

    def _close_handles(self) -> None:
        for handle in (self._stdout_handle, self._stderr_handle):
            if handle is not None:
                try:
                    handle.close()
                except Exception:
                    pass
        self._stdout_handle = None
        self._stderr_handle = None

    @staticmethod
    def _consume_escape_sequence(text: str, start: int) -> int:
        n = len(text)
        i = start + 1
        if i >= n:
            return n

        leader = text[i]
        if leader == "[":
            i += 1
            while i < n:
                code = ord(text[i])
                if 0x40 <= code <= 0x7E:
                    return i + 1
                i += 1
            return n

        if leader == "]":
            i += 1
            while i < n:
                ch = text[i]
                if ch == "\x07":
                    return i + 1
                if ch == "\x1b" and i + 1 < n and text[i + 1] == "\\":
                    return i + 2
                i += 1
            return n

        if leader in ("P", "X", "^", "_"):
            i += 1
            while i < n:
                if text[i] == "\x1b" and i + 1 < n and text[i + 1] == "\\":
                    return i + 2
                i += 1
            return n

        return min(i + 1, n)

    @classmethod
    def _sanitize_terminal_text(cls, text: str) -> str:
        lines: list[str] = []
        current_line: list[str] = []
        cursor = 0
        i = 0
        n = len(text)

        while i < n:
            ch = text[i]

            if ch == "\x1b":
                i = cls._consume_escape_sequence(text, i)
                continue

            if ch == "\r":
                cursor = 0
                i += 1
                continue

            if ch == "\n":
                lines.append("".join(current_line))
                current_line = []
                cursor = 0
                i += 1
                continue

            if ch == "\b":
                if cursor > 0:
                    cursor -= 1
                    if cursor < len(current_line):
                        current_line.pop(cursor)
                i += 1
                continue

            code = ord(ch)
            if ch != "\t" and (code < 0x20 or code == 0x7F):
                i += 1
                continue

            if cursor == len(current_line):
                current_line.append(ch)
            elif cursor < len(current_line):
                current_line[cursor] = ch
            else:
                current_line.extend(" " * (cursor - len(current_line)))
                current_line.append(ch)
            cursor += 1
            i += 1

        lines.append("".join(current_line))
        return "\n".join(lines)

    @staticmethod
    def _build_clean_path(raw_path: Path) -> Path:
        return raw_path.with_suffix(".clean.txt")

    def _write_clean_copy(self, stream_name: str) -> None:
        result = self.stdout if stream_name == "stdout" else self.stderr
        if result.status != "offloaded" or not result.path:
            return

        try:
            raw_path = Path(result.path)
            raw_bytes = raw_path.read_bytes()
            decoded = raw_bytes.decode("utf-8", errors="replace")
            clean_text = self._sanitize_terminal_text(decoded)
            clean_path = self._build_clean_path(raw_path)
            _write_text_utf8(clean_path, clean_text)
            result.clean_path = str(clean_path.resolve())
            result.clean_error = ""
        except Exception as exc:
            result.clean_error = str(exc)

    def _write_meta(self, return_code: int) -> None:
        if not self.stdout.truncated and not self.stderr.truncated:
            return
        if not self._ensure_layout():
            return
        if self._meta_path is None:
            return

        payload = {
            "version": 1,
            "kind": "pty_command_output",
            "uid": self._uid,
            "session_uuid": self.session_uuid,
            "exec_id": self._exec_id,
            "timestamp_utc": self._now.astimezone(dt.timezone.utc).isoformat(),
            "cwd": self.cwd,
            "command_sha256": hashlib.sha256(self.command.encode("utf-8")).hexdigest(),
            "keep_len": self.keep_len,
            "return_code": return_code,
            "stdout": {
                "status": self.stdout.status,
                "path": self.stdout.path,
                "clean_path": self.stdout.clean_path,
                "error": self.stdout.error,
                "clean_error": self.stdout.clean_error,
                "bytes_written": self.stdout.bytes_written,
                "truncated": self.stdout.truncated,
            },
            "stderr": {
                "status": self.stderr.status,
                "path": self.stderr.path,
                "clean_path": self.stderr.clean_path,
                "error": self.stderr.error,
                "clean_error": self.stderr.clean_error,
                "bytes_written": self.stderr.bytes_written,
                "truncated": self.stderr.truncated,
            },
        }

        try:
            flags = os.O_WRONLY | os.O_CREAT | os.O_TRUNC
            fd = os.open(self._meta_path, flags, 0o600)
            with os.fdopen(fd, "w", encoding="utf-8") as handle:
                json.dump(payload, handle, ensure_ascii=False, indent=2, sort_keys=True)
        except Exception:
            return

    def finalize(
        self, *, stdout_tail: bytes, stderr_tail: bytes, return_code: int
    ) -> PtyOutputOffloadResult:
        self._append_tail(stream_name="stdout", tail=stdout_tail)
        self._append_tail(stream_name="stderr", tail=stderr_tail)
        self._close_handles()
        self._write_clean_copy("stdout")
        self._write_clean_copy("stderr")
        self._write_meta(return_code)
        return PtyOutputOffloadResult(
            stdout=self.stdout,
            stderr=self.stderr,
            meta_path=str(self._meta_path.resolve()) if self._meta_path else "",
        )
