from aish.tools.fs_tools import ReadFileTool


def test_read_file_small_file_success(tmp_path):
    file_path = tmp_path / "sample.txt"
    file_path.write_text("alpha\nbeta\ngamma\n", encoding="utf-8")

    tool = ReadFileTool()
    result = tool(file_path=str(file_path), offset=1, limit=2)

    assert result.ok is True
    assert "Lines 1-2 of 3 total" in result.output
    assert "   1: alpha" in result.output
    assert "   2: beta" in result.output


def test_read_file_total_bytes_exceeded_returns_error_without_partial_content(tmp_path):
    file_path = tmp_path / "big.txt"
    content = ("a" * 20000) + "\n" + ("b" * 20000) + "\n"
    file_path.write_text(content, encoding="utf-8")

    tool = ReadFileTool()
    result = tool(file_path=str(file_path), offset=1, limit=2)

    assert result.ok is False
    assert (
        result.output
        == "Error: Requested content exceeds read_file max of 32768 bytes "
        "(40002 bytes needed); no content returned"
    )
    assert "   1:" not in result.output
    assert "   2:" not in result.output
    assert result.meta == {
        "reason": "max_bytes_exceeded",
        "max_bytes": 32768,
        "requested_bytes": 40002,
    }


def test_read_file_single_line_exceeds_limit_returns_error_without_content(tmp_path):
    file_path = tmp_path / "huge-line.txt"
    file_path.write_text("x" * 32769, encoding="utf-8")

    tool = ReadFileTool()
    result = tool(file_path=str(file_path), offset=1, limit=1)

    assert result.ok is False
    assert (
        result.output
        == "Error: Line 1 is 32769 bytes, exceeding read_file max of 32768 bytes; no content returned"
    )
    assert "   1:" not in result.output
    assert result.meta == {
        "reason": "single_line_too_long",
        "max_bytes": 32768,
        "line_number": 1,
        "line_bytes": 32769,
    }


def test_read_file_boundary_exactly_32768_bytes_succeeds(tmp_path):
    file_path = tmp_path / "boundary.txt"
    file_path.write_text("z" * 32768, encoding="utf-8")

    tool = ReadFileTool()
    result = tool(file_path=str(file_path), offset=1, limit=1)

    assert result.ok is True
    assert "Lines 1-1 of 1 total" in result.output
    assert "   1: " in result.output


def test_read_file_description_mentions_32kib_and_no_partial_content():
    description = ReadFileTool().description

    assert "32KiB" in description
    assert "no partial content is returned" in description
