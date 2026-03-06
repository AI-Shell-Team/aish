"""Prompt-toolkit completion components for AI Shell."""

from __future__ import annotations

import os
import re
import shlex
import subprocess
from typing import Optional

from prompt_toolkit.application import run_in_terminal
from prompt_toolkit.completion import Completer, Completion, NestedCompleter
from prompt_toolkit.document import Document

from ..skills import SkillManager


class QuotedPathCompleter(Completer):
    """Custom path completer that automatically quotes paths with spaces or special characters."""

    def __init__(self, expanduser=True, only_dirs=False):
        self.expanduser = expanduser
        self.only_dirs = only_dirs
        # Don't use PathCompleter anymore - implement file completion logic directly

    def get_completions(self, document, complete_event, only_dirs=None):
        """Generate completions, automatically quoting paths with spaces.

        Args:
            document: The Document object containing the text to complete
            complete_event: The CompleteEvent
            only_dirs: If True, only complete directories. If None, use self.only_dirs.
        """
        # Use parameter value if provided, otherwise use instance default
        if only_dirs is None:
            only_dirs = self.only_dirs

        text = document.text_before_cursor

        # Get files and directories in current directory
        try:
            # If input is empty, list current directory
            if not text.strip():
                items = os.listdir(".")
                dir_path = "."
                file_prefix = ""
                input_prefix = ""  # No prefix in input
            else:
                # Handle tilde expansion
                expanded_path = text
                input_prefix = ""  # Track the path prefix from input

                # Check if input already contains expanded home path
                # If so, don't try to convert back to ~ format
                home_dir = os.path.expanduser("~")
                input_contains_expanded_home = (
                    text.startswith(home_dir + "/") or text == home_dir
                )

                if input_contains_expanded_home:
                    # Input already contains expanded home path (e.g., /home/xzx/.config/)
                    # Handle it as a regular path, preserve the expanded format
                    # Get directory and file parts from the expanded path
                    dir_path = os.path.dirname(text)
                    file_prefix = os.path.basename(text)

                    # Extract input prefix (everything before the last /)
                    if "/" in text:
                        last_slash = text.rfind("/")
                        input_prefix = text[: last_slash + 1]
                        file_prefix = text[last_slash + 1 :]
                    else:
                        input_prefix = ""

                    # If no directory part, use current directory
                    if not dir_path:
                        dir_path = "."
                        file_prefix = text

                    # List files
                    if os.path.isdir(dir_path):
                        items = os.listdir(dir_path)
                    else:
                        items = []

                    # Filter matching files
                    if file_prefix:
                        items = [item for item in items if item.startswith(file_prefix)]

                    # Generate completions with EXPANDED format (no ~ conversion)
                    for item in sorted(items):
                        # Build completion text preserving expanded path
                        if input_prefix:
                            completion_text = input_prefix + item
                        elif dir_path and dir_path != ".":
                            completion_text = os.path.join(dir_path, item)
                        else:
                            completion_text = item

                        # Check if directory
                        full_path = (
                            os.path.join(dir_path, item)
                            if dir_path and dir_path != "."
                            else item
                        )
                        is_dir = os.path.isdir(full_path)

                        # If only_dirs is True and this is not a directory, skip it
                        if only_dirs and not is_dir:
                            continue

                        if is_dir:
                            # Display only the filename (like bash), not the full path
                            display_text = item + "/"
                            completion_text = completion_text + "/"
                        else:
                            # Display only the filename (like bash), not the full path
                            display_text = item

                        # Calculate start_position
                        # If completion_text contains the full path (including prefix),
                        # we should replace the entire input text.
                        if input_prefix and completion_text.startswith(input_prefix):
                            # Completion already has the prefix, replace entire input
                            start_pos = -len(text) if text else 0
                        elif input_prefix:
                            # Replace only the file portion after prefix
                            start_pos = -len(file_prefix) if file_prefix else 0
                        else:
                            # No prefix, replace entire input
                            start_pos = -len(text) if text else 0

                        # Yield completion
                        if " " in completion_text or any(
                            c in completion_text for c in "\"'$`\\(){}[]?*;|&<>"
                        ):
                            quoted_text = shlex.quote(completion_text)
                            yield Completion(
                                text=quoted_text,
                                start_position=start_pos,
                                display=display_text,
                                display_meta="file",
                            )
                        else:
                            yield Completion(
                                text=completion_text,
                                start_position=start_pos,
                                display=display_text,
                                display_meta="file",
                            )

                    # Done with expanded home path case, don't continue
                    return

                # Extract the path prefix from original text (before expansion)
                # For example: "./get" -> input_prefix="./", file_prefix="get"
                # For "~/.confi" -> input_prefix="~/", file_prefix=".confi"
                if "/" in text:
                    # Find the last slash position
                    last_slash = text.rfind("/")
                    input_prefix = text[: last_slash + 1]  # Include the slash
                    file_prefix = text[last_slash + 1 :]
                else:
                    # No path prefix (current directory)
                    input_prefix = ""
                    file_prefix = text

                if text.startswith("~"):
                    # Expand tilde to user home directory
                    expanded_path = os.path.expanduser(text)
                elif input_contains_expanded_home:
                    expanded_path = text
                else:
                    expanded_path = text

                # Get directory part and filename part from expanded path
                dir_path = os.path.dirname(expanded_path)
                file_prefix_expanded = os.path.basename(expanded_path)

                # If no directory part, use current directory
                if not dir_path:
                    dir_path = "."
                    expanded_path = text

                # List files in directory
                if os.path.isdir(dir_path):
                    items = os.listdir(dir_path)
                else:
                    items = []

                # Filter matching files
                # Use file_prefix from original input (not expanded) for filtering
                if file_prefix:
                    items = [item for item in items if item.startswith(file_prefix)]

            # Generate completions
            for item in sorted(items):
                # Build the completion text
                if input_prefix:
                    # Preserve the input path prefix (e.g., "./", "~/")
                    # This is critical for ~ paths to avoid toggle
                    completion_text = input_prefix + item
                elif dir_path and dir_path != ".":
                    completion_text = os.path.join(dir_path, item)
                else:
                    completion_text = item

                # Determine full path for checking if it's a directory
                if text.startswith("~"):
                    if dir_path == os.path.expanduser("~"):
                        full_path = os.path.join(os.path.expanduser("~"), item)
                    else:
                        full_path = (
                            os.path.join(dir_path, item) if dir_path != "." else item
                        )
                else:
                    full_path = (
                        os.path.join(dir_path, item)
                        if dir_path and dir_path != "."
                        else item
                    )

                # Check if it's a directory
                is_dir = os.path.isdir(full_path)

                # If only_dirs is True and this is not a directory, skip it
                if only_dirs and not is_dir:
                    continue

                if is_dir:
                    # Display only the filename (like bash), not the full path
                    display_text = item + "/"
                    completion_text = completion_text + "/"
                else:
                    # Display only the filename (like bash), not the full path
                    display_text = item

                # Calculate start_position
                # If completion_text contains the full path (including prefix),
                # we should replace the entire input text.
                # Otherwise, only replace the file name portion.
                if input_prefix and completion_text.startswith(input_prefix):
                    # Completion already has the prefix, replace entire input
                    start_pos = -len(text) if text else 0
                elif input_prefix:
                    # Replace only the file portion after prefix
                    start_pos = -len(file_prefix) if file_prefix else 0
                else:
                    # No prefix, replace entire input
                    start_pos = -len(text) if text else 0

                # Check if quotes are needed
                if " " in completion_text or any(
                    c in completion_text for c in "\"'$`\\(){}[]?*;|&<>"
                ):
                    quoted_text = shlex.quote(completion_text)
                    yield Completion(
                        text=quoted_text,
                        start_position=start_pos,
                        display=display_text,
                        display_meta="file",
                    )
                else:
                    yield Completion(
                        text=completion_text,
                        start_position=start_pos,
                        display=display_text,
                        display_meta="file",
                    )

        except (OSError, PermissionError):
            # If error, try basic completion
            pass


_SKILL_REF_COMPLETE_RE = re.compile(r"(?:^|\s)#([a-z0-9-]*)$", re.IGNORECASE)
_SKILL_REF_EXTRACT_RE = re.compile(
    r"#([a-z0-9](?:[a-z0-9-]*[a-z0-9])?)",
    re.IGNORECASE,
)


class SkillReferenceCompleter(Completer):
    def __init__(self, skill_manager: SkillManager):
        self.skill_manager = skill_manager

    def get_completions(self, document, complete_event):
        # Keep completions in sync with hot-reloaded skills. This stays cheap in
        # steady state (no-op unless the snapshot was invalidated).
        try:
            self.skill_manager.reload_if_dirty()
        except Exception:
            pass

        text = document.text_before_cursor
        match = _SKILL_REF_COMPLETE_RE.search(text)
        if not match:
            return

        prefix = match.group(1) or ""
        prefix_lower = prefix.lower()
        skills = sorted(
            self.skill_manager.list_skills(),
            key=lambda skill: skill.metadata.name,
        )
        for skill in skills:
            name = skill.metadata.name
            if prefix_lower and not name.startswith(prefix_lower):
                continue
            yield Completion(
                text=name,
                start_position=-len(prefix),
                display=f"#{name}",
                display_meta=skill.metadata.description,
            )


class ModeAwareCompleter(Completer):
    """Bash-style completer that prints candidates directly instead of showing dropdown."""

    # Threshold for showing confirmation prompt (large lists)
    COMPLETION_THRESHOLD = 100
    # Threshold below which completions are shown directly without requiring second Tab
    SMALL_COMPLETION_THRESHOLD = 30

    def __init__(
        self,
        ai_completer: Completer,
        shell_completer: Completer,
        ai_prefix_marks: set[str],
    ):
        self.ai_completer = ai_completer
        self.shell_completer = shell_completer
        self.ai_prefix_marks = ai_prefix_marks
        # Track last completion context to handle "Display all" confirmation
        self._last_completion_key: Optional[str] = None
        self._awaiting_confirmation: bool = False
        self._completion_cache: Optional[list[str]] = None
        # Track completion state for multi-tab detection
        # Format: (base_text, completion_count)
        self._completion_state: Optional[tuple[str, int]] = None
        self._last_terminal_width: Optional[int] = None

    def _get_terminal_width(self) -> int:
        import shutil

        try:
            return max(1, shutil.get_terminal_size(fallback=(80, 24)).columns)
        except OSError:
            return 80

    def _reset_state_on_terminal_resize(self) -> None:
        """Reset pending completion state when terminal width changes."""
        current_width = self._get_terminal_width()
        if self._last_terminal_width is None:
            self._last_terminal_width = current_width
            return
        if current_width != self._last_terminal_width:
            self._completion_state = None
            self._awaiting_confirmation = False
            self._completion_cache = None
            self._last_terminal_width = current_width

    def _is_ai_mode(self, document: Document) -> bool:
        text = document.text
        if not text:
            return False
        stripped = text.lstrip()
        return bool(stripped) and stripped[0] in self.ai_prefix_marks

    def _get_completion_key(self, document: Document) -> str:
        """Get a key representing the current completion context."""
        return f"{document.text_before_cursor}:{document.cursor_position}"

    def _print_columns(self, items: list[str]) -> None:
        """Print items in columns, similar to bash completion display."""
        import shutil
        import sys

        terminal_width = shutil.get_terminal_size().columns

        # Calculate max width
        max_width = max((len(item) for item in items), default=0)
        col_width = max_width + 2

        # Calculate number of columns
        num_cols = max(1, terminal_width // col_width)

        # Build output lines
        lines = []
        current_line = ""
        for i, item in enumerate(items):
            current_line += item.ljust(col_width)
            if (i + 1) % num_cols == 0:
                lines.append(current_line)
                current_line = ""

        if current_line:
            lines.append(current_line)

        # Use run_in_terminal to properly handle output without breaking prompt
        def _print():
            sys.stdout.write("\n" + "\n".join(lines) + "\n")
            sys.stdout.flush()

        try:
            run_in_terminal(_print)
        except Exception:
            # Fallback if run_in_terminal fails (e.g., in tests)
            _print()

    def get_completions(self, document, complete_event):
        self._reset_state_on_terminal_resize()
        text = document.text_before_cursor

        if self._is_ai_mode(document):
            # AI mode: use original completer behavior
            found = False
            for completion in self.ai_completer.get_completions(
                document, complete_event
            ):
                found = True
                yield completion
            if found:
                return

        # Shell mode: bash-style completion
        completions = list(
            self.shell_completer.get_completions(document, complete_event)
        )

        if not completions:
            # Reset completion state when no matches
            self._completion_state = None
            return

        # Single match: complete directly
        if len(completions) == 1:
            self._completion_state = None
            yield completions[0]
            return

        # Multiple matches: handle common prefix completion
        # Get completion texts (what would actually be inserted)
        completion_texts = [c.text for c in completions]

        # Calculate common prefix
        import os

        common_prefix = (
            os.path.commonprefix(completion_texts) if completion_texts else ""
        )

        # Get the text that would be replaced (start_position is negative, so we use abs)
        start_pos = abs(completions[0].start_position)
        current_input = text[-start_pos:] if start_pos > 0 else ""

        # Check if user is pressing Tab again (same context as last time)
        is_second_tab = False
        if self._completion_state:
            base_text, count = self._completion_state
            # Check if user is pressing Tab again in the same context:
            # 1. Text is the same (user didn't type anything new, just pressed Tab again)
            # 2. Text is an extension (via completion from previous Tab)
            # and completion count hasn't changed
            if len(completions) == count and (
                text == base_text or (text.startswith(base_text) and text != base_text)
            ):
                is_second_tab = True

        if is_second_tab:
            # User pressed Tab again, show all completion options
            self._completion_state = None
            # Extract display names
            items = []
            for c in completions:
                display = c.display
                plain_text = ""
                try:
                    for item in display:
                        if isinstance(item, tuple) and len(item) == 2:
                            plain_text += item[1]
                        else:
                            plain_text += str(item)
                except (TypeError, ValueError):
                    plain_text = str(display)
                items.append(plain_text)
            items = sorted(set(items))

            # Check if we need pager for large lists
            if len(items) > self.COMPLETION_THRESHOLD:
                self._print_with_pager(items)
            else:
                self._print_columns(items)
            return

        # Check if we can do common prefix completion
        if (
            common_prefix
            and common_prefix != current_input
            and common_prefix.startswith(current_input)
        ):
            # There's a common prefix that extends the current input
            # Complete to the common prefix and save state for next Tab
            # Save the base text (current input before completion)
            self._completion_state = (text, len(completions))
            yield Completion(
                text=common_prefix,
                start_position=completions[0].start_position,
                display=common_prefix,
            )
            return

        # No common prefix completion possible (already at max common prefix)
        # Extract display names
        items = []
        for c in completions:
            display = c.display
            plain_text = ""
            try:
                for item in display:
                    if isinstance(item, tuple) and len(item) == 2:
                        plain_text += item[1]
                    else:
                        plain_text += str(item)
            except (TypeError, ValueError):
                plain_text = str(display)
            items.append(plain_text)
        items = sorted(set(items))

        # For small numbers of completions, show directly without requiring second Tab
        if len(items) <= self.SMALL_COMPLETION_THRESHOLD:
            # Reset completion state since we're showing directly
            self._completion_state = None
            self._print_columns(items)
            return

        # For larger numbers, save state and require another Tab press
        self._completion_state = (text, len(completions))

        # Check if we need pager for large lists
        if len(items) > self.COMPLETION_THRESHOLD:
            self._print_prompt_message(len(items))
        else:
            # For medium-sized lists, show a hint that another Tab will show all
            import sys

            def _print():
                sys.stdout.write(
                    f"\n{len(items)} completions (Press Tab again to show)\n"
                )
                sys.stdout.flush()

            try:
                run_in_terminal(_print)
            except Exception:
                _print()
        # Don't return any Completion, require another Tab press

    def _print_prompt_message(self, count: int) -> None:
        """Print a message indicating there are many possibilities."""
        import sys

        def _print():
            # Print message with newline
            sys.stdout.write(
                f"\nDisplay all {count} possibilities? (Press Tab again to show)\n"
            )
            sys.stdout.flush()

        try:
            run_in_terminal(_print)
        except Exception:
            _print()

    def _print_with_pager(self, items: list[str]) -> None:
        """Print items using a pager (like more) for large lists."""
        import subprocess
        import sys
        import tempfile

        def _print():
            # Build columnated output
            import shutil

            terminal_width = shutil.get_terminal_size().columns
            max_width = max((len(item) for item in items), default=0)
            col_width = max_width + 2
            num_cols = max(1, terminal_width // col_width)

            lines = []
            current_line = ""
            for i, item in enumerate(items):
                current_line += item.ljust(col_width)
                if (i + 1) % num_cols == 0:
                    lines.append(current_line)
                    current_line = ""

            if current_line:
                lines.append(current_line)

            output = "\n".join(lines)

            # Try to use pager (more or less)
            pager = os.environ.get("PAGER", "more")
            try:
                # Use subprocess to run pager
                process = subprocess.Popen(
                    [pager],
                    stdin=subprocess.PIPE,
                    stdout=sys.stdout,
                    stderr=sys.stderr,
                    text=True,
                )
                process.communicate(input=output)
                process.wait()
            except Exception:
                # Fallback: print directly
                sys.stdout.write("\n" + output + "\n")
                sys.stdout.flush()

        try:
            run_in_terminal(_print)
        except Exception:
            _print()


def make_shell_completer():
    path_comp = QuotedPathCompleter(expanduser=True)

    # Create command completer that provides PATH executable file completion
    class CommandCompleter(Completer):
        def __init__(self):
            self._cached_commands = None
            self._last_cache_time = 0

        def _get_commands(self):
            """Get all executable commands from PATH"""
            import time

            current_time = time.time()

            # Cache for 5 seconds to avoid frequent PATH scanning
            if (
                self._cached_commands is None
                or current_time - self._last_cache_time > 5
            ):

                try:
                    path_dirs = os.getenv("PATH", "").split(os.pathsep)
                    commands = set()

                    for path_dir in path_dirs:
                        if not os.path.isdir(path_dir):
                            continue

                        try:
                            if os.access(path_dir, os.R_OK):
                                for entry in os.listdir(path_dir):
                                    entry_path = os.path.join(path_dir, entry)
                                    if os.path.isfile(entry_path) and os.access(
                                        entry_path, os.X_OK
                                    ):
                                        commands.add(entry)
                        except (OSError, PermissionError):
                            continue

                    self._cached_commands = sorted(commands)
                    self._last_cache_time = current_time
                except Exception:
                    self._cached_commands = set()

            return self._cached_commands

        def get_completions(self, document, complete_event):
            text = document.text_before_cursor.strip()

            # If input contains space, it means it's a parameter, don't provide command completion
            if " " in text:
                return

            # Get all commands
            commands = self._get_commands()

            # Filter matching commands
            for command in commands:
                if command.startswith(text):
                    yield Completion(
                        text=command,
                        start_position=-len(text),
                        display=command,
                        display_meta="executable",
                    )

    # Create smart command argument completer that can extract command arguments and provide file completion
    class SmartArgumentCompleter(Completer):
        # Commands that only accept directories
        DIR_ONLY_COMMANDS = {"cd", "pushd", "rmdir"}

        def __init__(self, path_completer, command_completer=None):
            self.path_completer = path_completer
            self.command_completer = command_completer

        def _is_dir_only_command(self, command):
            """Check if the command only accepts directory arguments."""
            cmd_base = command.split("/")[-1]  # Handle paths like '/usr/bin/git'
            return cmd_base in self.DIR_ONLY_COMMANDS

        def _strip_sudo_prefix(self, text, parts):
            """Strip sudo prefix from text and parts. Returns (stripped_text, stripped_parts, sudo_count)."""
            if not parts:
                return text, parts, 0

            sudo_count = 0
            original_parts = list(parts)
            while parts and parts[0] == "sudo":
                sudo_count += 1
                parts = parts[1:]

            if sudo_count == 0:
                return text, original_parts, 0

            # Calculate the stripped text by removing sudo prefixes
            # Find the position after all sudo prefixes and their following spaces
            import shlex

            try:
                # Use shlex.split to find the end position of each sudo token
                i = 0
                tokens_removed = 0

                # Skip leading whitespace
                while i < len(text) and text[i].isspace():
                    i += 1

                # Remove sudo tokens
                while tokens_removed < sudo_count and i < len(text):
                    # Check if current position starts with 'sudo'
                    if text[i : i + 4] == "sudo":
                        i += 4  # Skip 'sudo'
                        tokens_removed += 1
                        # Skip spaces after sudo
                        while i < len(text) and text[i].isspace():
                            i += 1
                    else:
                        # Token doesn't match sudo, stop
                        break

                stripped_text = text[i:]
                return stripped_text, parts, sudo_count
            except Exception:
                # Fallback: return original
                return text, original_parts, sudo_count

        def get_completions(self, document, complete_event):
            text = document.text_before_cursor

            # If input is empty, provide file completion (show all files)
            # This is used when completing after a complete command (e.g., "vim" + Tab)
            if not text.strip():
                # Get command name from full document text
                full_text = document.text
                import shlex

                try:
                    cmd_parts = shlex.split(full_text)
                    command_name = cmd_parts[0] if cmd_parts else ""
                    only_dirs = self._is_dir_only_command(command_name)
                except Exception:
                    only_dirs = False

                # List all files in current directory
                import os

                try:
                    items = sorted(os.listdir("."))
                    for item in items:
                        full_path = os.path.join(".", item)
                        is_dir = os.path.isdir(full_path)

                        # If only_dirs is True and this is not a directory, skip it
                        if only_dirs and not is_dir:
                            continue

                        if is_dir:
                            display_text = item + "/"
                            completion_text = item + "/"
                        else:
                            display_text = item
                            completion_text = item

                        # Check if quotes are needed
                        if " " in completion_text or any(
                            c in completion_text for c in "\"'$`\\(){}[]?*;|&<>"
                        ):
                            import shlex

                            quoted_text = shlex.quote(completion_text)
                            yield Completion(
                                text=quoted_text,
                                start_position=0,
                                display=display_text,
                                display_meta="file",
                            )
                        else:
                            yield Completion(
                                text=completion_text,
                                start_position=0,
                                display=display_text,
                                display_meta="file",
                            )
                except (OSError, PermissionError):
                    pass
                return

            # Parse command and arguments, properly handle options
            import shlex

            try:
                parts = shlex.split(text)
            except ValueError:
                # If quotes don't match, use simple split
                parts = text.split()

            if not parts:
                return

            # Strip sudo prefix for completion
            stripped_text, stripped_parts, sudo_count = self._strip_sudo_prefix(
                text, list(parts)
            )

            # If sudo was stripped, use stripped parts for completion logic
            if sudo_count > 0:
                parts = stripped_parts
                text = stripped_text

            # Check if we're at first argument position (command name)
            if len(parts) == 1 and not text.endswith(" "):
                # If sudo was stripped, provide command completion for the actual command
                if sudo_count > 0 and self.command_completer:
                    arg_document = Document(text=text, cursor_position=len(text))
                    for completion in self.command_completer.get_completions(
                        arg_document, complete_event
                    ):
                        yield Completion(
                            text=completion.text,
                            start_position=-len(text),
                            display=completion.display,
                            display_meta=completion.display_meta,
                        )
                    return

                # Otherwise, provide file completion
                arg_document = Document(text=text, cursor_position=len(text))
                only_dirs = self._is_dir_only_command(parts[0]) if parts else False
                for completion in self.path_completer.get_completions(
                    arg_document, complete_event, only_dirs=only_dirs
                ):
                    yield Completion(
                        text=completion.text,
                        start_position=-len(text),
                        display=completion.display,
                        display_meta=completion.display_meta,
                    )
                return

            # For multiple arguments case, find current argument position to complete
            # Use shlex split to properly handle quotes
            try:
                # Re-parse, keeping original text structure
                parsed_parts = []
                current_part = ""
                in_quotes = False
                quote_char = None

                i = 0
                while i < len(text):
                    char = text[i]

                    if char in ('"', "'") and not in_quotes:
                        in_quotes = True
                        quote_char = char
                        current_part += char
                    elif char == quote_char and in_quotes:
                        in_quotes = False
                        quote_char = None
                        current_part += char
                    elif char == " " and not in_quotes:
                        if current_part:
                            parsed_parts.append(current_part)
                            current_part = ""
                        # Skip spaces
                        while i + 1 < len(text) and text[i + 1] == " ":
                            i += 1
                    else:
                        current_part += char
                    i += 1

                if current_part:
                    parsed_parts.append(current_part)

                # If text ends with space, add empty parameter to indicate new parameter start
                if text.endswith(" "):
                    parsed_parts.append("")

                # Find current argument to complete (last non-empty argument)
                current_arg = ""
                arg_start_pos = 0

                if parsed_parts:
                    current_arg = parsed_parts[-1]
                    # Calculate start position of current argument in original text
                    if len(parsed_parts) > 1:
                        # Find start position of last argument
                        arg_start_pos = len(text) - len(current_arg)

                    # If current argument is empty (like "rm -f " case), use current directory
                    if not current_arg.strip():
                        arg_document = Document(text=".", cursor_position=1)
                    else:
                        arg_document = Document(
                            text=current_arg, cursor_position=len(current_arg)
                        )

                    # Use path completer to provide completions for current argument
                    only_dirs = self._is_dir_only_command(parts[0]) if parts else False
                    for completion in self.path_completer.get_completions(
                        arg_document, complete_event, only_dirs=only_dirs
                    ):
                        # Calculate correct start_position
                        if len(parsed_parts) == 1:
                            # Only one argument, replace entire input
                            start_pos = -len(text)
                        else:
                            # Multiple arguments, only replace current argument
                            start_pos = -len(current_arg)

                        yield Completion(
                            text=completion.text,
                            start_position=start_pos,
                            display=completion.display,
                            display_meta=completion.display_meta,
                        )
            except Exception:
                # If parsing fails, fallback to simple method
                first_space_pos = text.find(" ")
                if first_space_pos == -1:
                    arg_document = Document(text=text, cursor_position=len(text))
                    start_pos = -len(text)
                else:
                    arg_text = text[first_space_pos + 1 :]
                    if not arg_text.strip():
                        arg_document = Document(text=".", cursor_position=1)
                    else:
                        arg_document = Document(
                            text=arg_text, cursor_position=len(arg_text)
                        )
                    start_pos = -(len(text) - first_space_pos - 1)

                # Get command name for only_dirs check
                command_name = text[:first_space_pos] if first_space_pos != -1 else text
                only_dirs = self._is_dir_only_command(command_name)
                for completion in self.path_completer.get_completions(
                    arg_document, complete_event, only_dirs=only_dirs
                ):
                    yield Completion(
                        text=completion.text,
                        start_position=start_pos,
                        display=completion.display,
                        display_meta=completion.display_meta,
                    )

    # Create directory-only completer for cd, pushd, rmdir
    dir_only_comp = QuotedPathCompleter(expanduser=True, only_dirs=True)

    # Create nested completer for specific built-in commands
    nested = NestedCompleter.from_nested_dict(
        {
            "cd": dir_only_comp,  # cd should only complete directories
            "pushd": dir_only_comp,  # pushd should only complete directories
            "rmdir": dir_only_comp,  # rmdir should only complete directories
            "ls": path_comp,
            "popd": None,
            "dirs": None,
            "help": None,
            "exit": None,
            "history": None,
            "export": None,
            "unset": None,
            "pwd": None,
        }
    )

    # Create smart merged completer that supports command completion and argument completion
    class SmartMergedCompleter(Completer):
        def __init__(self, nested_completer, argument_completer, command_completer):
            self.nested_completer = nested_completer
            self.argument_completer = argument_completer
            self.command_completer = command_completer

        def get_completions(self, document, complete_event):
            text = document.text_before_cursor
            text_stripped = text.strip()

            # If input doesn't contain space, check if it's a complete command
            if " " not in text_stripped:
                # First try built-in command completion
                nested_completions = list(
                    self.nested_completer.get_completions(document, complete_event)
                )

                # If there are built-in command completions, use them
                if nested_completions:
                    for completion in nested_completions:
                        yield completion
                    # Built-in commands matched, don't provide file completion
                    return

                # Check if the input exactly matches a command in PATH
                # If so, only provide file completion (for command arguments)
                command_completions = list(
                    self.command_completer.get_completions(document, complete_event)
                )

                # Check if input text exactly matches any command
                exact_command_match = any(
                    c.text == text_stripped or c.text == text_stripped + "/"
                    for c in command_completions
                )

                if exact_command_match:
                    # Input is a complete command, provide file completion only
                    # Use empty document to show ALL files in current directory (like bash)
                    empty_document = Document(text="", cursor_position=0)
                    for completion in self.argument_completer.get_completions(
                        empty_document, complete_event
                    ):
                        yield completion
                else:
                    # Input is incomplete, provide command completion
                    for completion in command_completions:
                        yield completion
                    # Also provide file completion for partial command names
                    for completion in self.argument_completer.get_completions(
                        document, complete_event
                    ):
                        yield completion
            else:
                # If contains space, try nested completer
                nested_completions = list(
                    self.nested_completer.get_completions(document, complete_event)
                )

                # If nested completer has results, use them first
                if nested_completions:
                    for completion in nested_completions:
                        yield completion
                else:
                    # Otherwise use smart argument completer
                    for completion in self.argument_completer.get_completions(
                        document, complete_event
                    ):
                        yield completion

    # Create various completers
    command_completer = CommandCompleter()
    argument_completer = SmartArgumentCompleter(path_comp, command_completer)
    smart_completer = SmartMergedCompleter(
        nested, argument_completer, command_completer
    )
    return smart_completer
