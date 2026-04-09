import json
from pathlib import Path
from unittest.mock import patch

from aish.offload.pty_output_offload import PtyOutputOffload


def test_pty_output_offload_reconstructs_full_stdout(tmp_path: Path):
    writer = PtyOutputOffload(
        command="echo test",
        session_uuid="session-1",
        cwd=str(tmp_path),
        keep_len=4096,
        base_dir=str(tmp_path / "offload"),
    )

    writer.append_overflow(stream_name="stdout", overflow=b"ab")
    result = writer.finalize(stdout_tail=b"cdef", stderr_tail=b"", return_code=0)

    assert result.stdout.status == "offloaded"
    assert result.stderr.status == "inline"
    assert result.stdout.path
    assert result.stdout.clean_path
    assert Path(result.stdout.path).read_bytes() == b"abcdef"
    assert Path(result.stdout.clean_path).read_text(encoding="utf-8") == "abcdef"
    assert result.meta_path
    meta_path = Path(result.meta_path)
    assert meta_path.exists()
    meta = json.loads(meta_path.read_text(encoding="utf-8"))
    assert meta["stdout"]["clean_path"] == result.stdout.clean_path
    assert meta["stdout"]["clean_error"] == ""


def test_pty_output_offload_reconstructs_both_streams(tmp_path: Path):
    writer = PtyOutputOffload(
        command="echo test",
        session_uuid="session-2",
        cwd=str(tmp_path),
        keep_len=4096,
        base_dir=str(tmp_path / "offload"),
    )

    writer.append_overflow(stream_name="stdout", overflow=b"out-")
    writer.append_overflow(stream_name="stderr", overflow=b"err-")
    result = writer.finalize(stdout_tail=b"tail", stderr_tail=b"tail", return_code=1)

    assert result.stdout.status == "offloaded"
    assert result.stderr.status == "offloaded"
    assert Path(result.stdout.path).read_bytes() == b"out-tail"
    assert Path(result.stderr.path).read_bytes() == b"err-tail"
    assert Path(result.stdout.clean_path).read_text(encoding="utf-8") == "out-tail"
    assert Path(result.stderr.clean_path).read_text(encoding="utf-8") == "err-tail"


def test_pty_output_offload_fails_gracefully_when_base_invalid(tmp_path: Path):
    bad_base = tmp_path / "not-a-directory"
    bad_base.write_text("x", encoding="utf-8")

    writer = PtyOutputOffload(
        command="echo test",
        session_uuid="session-3",
        cwd=str(tmp_path),
        keep_len=4096,
        base_dir=str(bad_base),
    )

    writer.append_overflow(stream_name="stdout", overflow=b"overflow")
    result = writer.finalize(stdout_tail=b"tail", stderr_tail=b"", return_code=0)

    assert result.stdout.status == "failed"
    assert result.stdout.error


def test_pty_output_offload_no_truncation_keeps_inline(tmp_path: Path):
    writer = PtyOutputOffload(
        command="echo test",
        session_uuid="session-4",
        cwd=str(tmp_path),
        keep_len=4096,
        base_dir=str(tmp_path / "offload"),
    )

    result = writer.finalize(stdout_tail=b"short", stderr_tail=b"", return_code=0)

    assert result.stdout.status == "inline"
    assert result.stderr.status == "inline"
    assert result.meta_path == ""


def test_pty_output_offload_clean_handles_invalid_utf8_bytes(tmp_path: Path):
    writer = PtyOutputOffload(
        command="echo test",
        session_uuid="session-5",
        cwd=str(tmp_path),
        keep_len=4096,
        base_dir=str(tmp_path / "offload"),
    )

    writer.append_overflow(stream_name="stdout", overflow=b"bad-\xe8(")
    result = writer.finalize(stdout_tail=b"-tail", stderr_tail=b"", return_code=0)

    assert result.stdout.status == "offloaded"
    clean_text = Path(result.stdout.clean_path).read_text(encoding="utf-8")
    assert "bad-" in clean_text
    assert "\ufffd" in clean_text
    assert "-tail" in clean_text


def test_pty_output_offload_clean_strips_control_sequences(tmp_path: Path):
    writer = PtyOutputOffload(
        command="echo test",
        session_uuid="session-6",
        cwd=str(tmp_path),
        keep_len=4096,
        base_dir=str(tmp_path / "offload"),
    )

    ansi_and_controls = (
        b"hello \x1b[31mred\x1b[0m\rHELLO\n"
        b"abc\bX\n"
        b"title\x1b]0;my-title\x07done\n"
    )
    writer.append_overflow(stream_name="stdout", overflow=ansi_and_controls)
    result = writer.finalize(stdout_tail=b"", stderr_tail=b"", return_code=0)

    clean_text = Path(result.stdout.clean_path).read_text(encoding="utf-8")
    assert "\x1b" not in clean_text
    assert "HELLO" in clean_text
    assert "abX" in clean_text
    assert "my-title" not in clean_text
    assert "done" in clean_text


def test_pty_output_offload_clean_failure_keeps_raw_files(tmp_path: Path):
    writer = PtyOutputOffload(
        command="echo test",
        session_uuid="session-7",
        cwd=str(tmp_path),
        keep_len=4096,
        base_dir=str(tmp_path / "offload"),
    )

    writer.append_overflow(stream_name="stdout", overflow=b"overflow")
    with patch(
        "aish.offload.pty_output_offload._write_text_utf8",
        side_effect=OSError("clean write failed"),
    ):
        result = writer.finalize(stdout_tail=b"tail", stderr_tail=b"", return_code=0)

    assert result.stdout.status == "offloaded"
    assert Path(result.stdout.path).exists()
    assert result.stdout.clean_path == ""
    assert "clean write failed" in result.stdout.clean_error
    meta = json.loads(Path(result.meta_path).read_text(encoding="utf-8"))
    assert meta["stdout"]["clean_path"] == ""
    assert "clean write failed" in meta["stdout"]["clean_error"]
