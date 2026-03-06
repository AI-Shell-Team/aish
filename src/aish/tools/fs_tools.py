from pathlib import Path
from typing import ClassVar

from aish.tools.base import ToolBase
from aish.tools.result import ToolResult


# TODO: support images
class ReadFileTool(ToolBase):
    MAX_READ_BYTES: ClassVar[int] = 32 * 1024

    def __init__(self):
        super().__init__(
            name="read_file",
            description=(
                "Read file contents with line numbers. "
                "Maximum returned content per call is 32KiB (32768 bytes). "
                "If the requested content exceeds this limit, the tool returns an error "
                "and no partial content is returned. This means you should find another efficent way to inspect the content of the file."
                "If a single line exceeds 32KiB, the tool also returns an error and no content is returned."
            ),
            parameters={
                "type": "object",
                "properties": {
                    "file_path": {
                        "type": "string",
                        "description": "The absolute path to the file to read.",
                    },
                    "offset": {
                        "type": "integer",
                        "description": "Starting line number to read from (1-based)",
                        "default": 1,
                    },
                    "limit": {
                        "type": "integer",
                        "description": "maximum number of lines to read",
                        "default": 200,
                    },
                },
                "required": ["file_path"],
            },
        )

    def __call__(self, file_path: str, offset: int = 1, limit: int = 200) -> ToolResult:
        try:
            result_lines = []
            total_lines = 0
            max_bytes = self.MAX_READ_BYTES

            # First pass: count total lines
            with open(file_path, "r", encoding="utf-8") as file:
                for _ in file:
                    total_lines += 1

            # Second pass: read the requested content
            with open(file_path, "r", encoding="utf-8") as file:
                # Skip lines before offset
                current_line = 1
                while current_line < offset:
                    line = file.readline()
                    if not line:  # End of file reached before offset
                        return ToolResult(
                            ok=False,
                            output=(
                                f"Error: File has only {total_lines} lines, but offset "
                                f"{offset} was requested"
                            ),
                        )
                    current_line += 1

                lines_read = 0
                accumulated_bytes = 0
                while lines_read < limit:
                    line = file.readline()
                    if not line:  # End of file
                        break
                    line_no = offset + lines_read
                    line_bytes = len(line.encode("utf-8"))
                    if line_bytes > max_bytes:
                        return ToolResult(
                            ok=False,
                            output=(
                                f"Error: Line {line_no} is {line_bytes} bytes, "
                                f"exceeding read_file max of {max_bytes} bytes; no content returned"
                            ),
                            meta={
                                "reason": "single_line_too_long",
                                "max_bytes": max_bytes,
                                "line_number": line_no,
                                "line_bytes": line_bytes,
                            },
                        )
                    would_be = accumulated_bytes + line_bytes
                    if would_be > max_bytes:
                        return ToolResult(
                            ok=False,
                            output=(
                                "Error: Requested content exceeds read_file max of "
                                f"{max_bytes} bytes ({would_be} bytes needed); no content returned"
                            ),
                            meta={
                                "reason": "max_bytes_exceeded",
                                "max_bytes": max_bytes,
                                "requested_bytes": would_be,
                            },
                        )
                    accumulated_bytes = would_be
                    result_lines.append(line.rstrip("\n\r"))
                    lines_read += 1

            if not result_lines:
                return ToolResult(
                    ok=True,
                    output=(
                        "No content found in the specified range "
                        f"(file has {total_lines} total lines)"
                    ),
                    meta={"empty": True},
                )

            numbered_lines = [
                f"{offset + i:4d}: {line}" for i, line in enumerate(result_lines)
            ]

            # Calculate range info
            end_line = offset + len(result_lines) - 1
            range_info = f"Lines {offset}-{end_line} of {total_lines} total"

            output = f"File({file_path}) - {range_info}:\n"
            output += "\n".join(numbered_lines)
            return ToolResult(ok=True, output=output)

        except FileNotFoundError:
            return ToolResult(ok=False, output=f"Error: File '{file_path}' not found")
        except PermissionError:
            return ToolResult(
                ok=False,
                output=f"Error: Permission denied to read file '{file_path}'",
            )
        except UnicodeDecodeError:
            return ToolResult(
                ok=False,
                output=(
                    f"Error: File '{file_path}' contains binary data or unsupported encoding"
                ),
            )
        except Exception as e:
            return ToolResult(
                ok=False,
                output=f"Error: {str(e)}",
                meta={"exception_type": type(e).__name__},
            )


class WriteFileTool(ToolBase):
    def __init__(self):
        super().__init__(
            name="write_file",
            description="Write a file to the filesystem.",
            parameters={
                "type": "object",
                "properties": {
                    "file_path": {
                        "type": "string",
                        "description": "The path to the file to write.",
                    },
                    "content": {
                        "type": "string",
                        "description": "The content to write to the file.",
                    },
                },
                "required": ["file_path", "content"],
            },
        )

    def __call__(self, file_path: str, content: str) -> ToolResult:
        try:
            with open(file_path, "w") as f:
                f.write(content)
            return ToolResult(ok=True, output=f"File {file_path} written successfully.")
        except Exception as e:
            return ToolResult(
                ok=False,
                output=f"Error: {str(e)}",
                meta={"exception_type": type(e).__name__},
            )

    def need_confirm_before_exec(self, content: str) -> bool:
        return True

    def get_confirmation_info(self, content: str) -> dict:
        # For write_file tool, we need to get the file_path from the tool_args
        # Since we only receive the content string here, we'll return what we can
        return {
            "content_preview": content[:100] + "..." if len(content) > 100 else content,
            "content_length": len(content),
        }


class EditFileTool(ToolBase):
    def __init__(self):
        super().__init__(
            name="edit_file",
            description="""Performs exact string replacements in files.

Usage:
- Do read file content first before editting.
- ALWAYS prefer editing existing files. NEVER write new files unless explicitly required.
- Take care of the encoding of the file to be edited.
- The edit will FAIL if old_string is not unique in the file. Either provide a larger string with more surrounding context to make it unique or use replace_all to change every instance of old_string.
- Use replace_all for replacing and renaming strings across the file. This parameter is useful if you want to rename a variable for instance.
""",
            parameters={
                "type": "object",
                "properties": {
                    "file_path": {
                        "type": "string",
                        "description": "The absolute path to the file to modify",
                    },
                    "old_string": {
                        "type": "string",
                        "description": "The text to replace",
                    },
                    "new_string": {
                        "type": "string",
                        "description": "The text to replace it with (must be different from old_string)",
                    },
                    "replace_all": {
                        "type": "boolean",
                        "description": "Replace all occurences of old_string (default false)",
                        "default": False,
                    },
                },
                "required": ["file_path", "old_string", "new_string"],
            },
        )

    @staticmethod
    def _decode_bytes(raw: bytes) -> tuple[str | None, str | None]:
        # Prefer round-trippable decodes to preserve file encoding.
        candidates = [
            "utf-8-sig",
            "utf-8",
            "utf-16",
            "utf-16-le",
            "utf-16-be",
            "gb18030",
        ]
        for encoding in candidates:
            try:
                text = raw.decode(encoding)
                if text.encode(encoding) == raw:
                    return encoding, text
            except (UnicodeDecodeError, UnicodeEncodeError):
                continue
        return None, None

    def __call__(
        self,
        file_path: str,
        old_string: str,
        new_string: str,
        replace_all: bool = False,
    ) -> ToolResult:
        try:
            if old_string == "":
                return ToolResult(
                    ok=False,
                    output="Error: old_string must not be empty",
                )
            if new_string == old_string:
                return ToolResult(
                    ok=False,
                    output=("Error: new_string must be different from old_string"),
                )

            path = Path(file_path)
            if not path.exists():
                return ToolResult(
                    ok=False, output=f"Error: File '{file_path}' not found"
                )
            if not path.is_file():
                return ToolResult(
                    ok=False, output=f"Error: '{file_path}' is not a file"
                )

            raw = path.read_bytes()
            encoding, content = self._decode_bytes(raw)
            if encoding is None or content is None:
                return ToolResult(
                    ok=False,
                    output=(
                        f"Error: File '{file_path}' contains binary data or unsupported encoding"
                    ),
                )

            occurrences = content.count(old_string)
            if occurrences == 0:
                return ToolResult(
                    ok=False,
                    output=(
                        "Error: old_string not found in file. Provide a larger "
                        "context string or verify the file contents first."
                    ),
                )

            if not replace_all and occurrences != 1:
                return ToolResult(
                    ok=False,
                    output=(
                        f"Error: old_string occurs {occurrences} times in the file, "
                        "but replace_all is false. Provide a larger unique old_string "
                        "or set replace_all=true."
                    ),
                    meta={"occurrences": occurrences},
                )

            if replace_all:
                updated = content.replace(old_string, new_string)
                replaced = occurrences
            else:
                updated = content.replace(old_string, new_string, 1)
                replaced = 1

            path.write_bytes(updated.encode(encoding))
            return ToolResult(
                ok=True,
                output=(
                    f"Successfully replaced {replaced} occurrence(s) in '{file_path}'."
                ),
                meta={"replacements": replaced, "encoding": encoding},
            )

        except PermissionError:
            return ToolResult(
                ok=False,
                output=f"Error: Permission denied to edit file '{file_path}'",
            )
        except Exception as e:
            return ToolResult(
                ok=False,
                output=f"Error: {str(e)}",
                meta={"exception_type": type(e).__name__},
            )

    def need_confirm_before_exec(self, tool_args: dict | None = None) -> bool:
        return True

    def get_confirmation_info(self, tool_args: dict | None = None) -> dict:
        if not isinstance(tool_args, dict):
            return {}

        def _preview(value: object, limit: int = 100) -> str:
            text = str(value) if value is not None else ""
            return text[:limit] + "..." if len(text) > limit else text

        replace_all = bool(tool_args.get("replace_all", False))
        old_string = tool_args.get("old_string", "")
        new_string = tool_args.get("new_string", "")
        mode = "Replace all" if replace_all else "Replace"
        return {
            "content_preview": f"{mode}: {_preview(old_string)} -> {_preview(new_string)}"
        }
