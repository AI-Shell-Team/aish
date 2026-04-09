"""Built-in command handlers for aish shell.

This module provides stateless command processing logic for shell built-in commands.
Handlers are designed to be used by both the interactive shell core (user commands) and BashTool (AI commands).
"""

import os
import shlex
from dataclasses import dataclass, field
from typing import List, Optional


@dataclass
class BuiltinResult:
    """Result of executing a built-in command."""

    success: bool
    output: str
    error: str = ""
    returncode: int = 0
    # State changes (to be applied by caller)
    new_cwd: Optional[str] = None
    env_vars_to_set: dict = field(default_factory=dict)
    env_vars_to_unset: List[str] = field(default_factory=list)
    directory_stack_push: Optional[str] = None
    directory_stack_pop: bool = False
    export_vars_to_remove: List[str] = field(default_factory=list)


class DirectoryStack(list):
    """Directory stack state for pushd/popd commands.

    This class extends list for full backward compatibility.
    """

    def push(self, path: str) -> None:
        """Push a directory onto the stack."""
        self.append(path)

    def pop(self) -> Optional[str]:  # type: ignore[override]
        """Pop a directory from the stack."""
        if self:
            return super().pop()
        return None

    def peek(self) -> Optional[str]:
        """Get the top directory without popping."""
        if self:
            return self[-1]
        return None

    def is_empty(self) -> bool:
        """Check if the stack is empty."""
        return len(self) == 0


class BuiltinHandlers:
    """Stateless built-in command handlers.

    These handlers perform command logic and return a BuiltinResult
    describing what should be done. The caller is responsible for:
    - Applying state changes (new_cwd, env_vars, directory_stack)
    - Displaying output to the user
    - Recording history
    """

    @staticmethod
    def handle_cd(
        command: str, cwd: str, directory_stack: DirectoryStack
    ) -> BuiltinResult:
        """Handle cd command to change working directory.

        Supports options:
        -L: Use logical path (follow symlinks, default behavior)
        -P: Use physical path (resolve symlinks)
        -e: When -P is specified and current directory cannot be determined, exit with non-zero status
        # -@: Extended attributes support (ignored on Linux, temporarily hidden)
        """
        try:
            parts = shlex.split(command)
        except ValueError:
            # Handle unmatched quotes by treating the whole thing after 'cd' as a path
            parts = command.split(None, 1)
            if len(parts) > 1:
                parts = ["cd", parts[1]]

        original_dir = cwd

        # Parse options (following handle_export pattern)
        physical_mode = False
        strict_check = False  # -e flag
        disable_options = False
        paths = []

        i = 1
        while i < len(parts):
            arg = parts[i]

            if arg == "--":
                disable_options = True
                i += 1
                break
            elif arg == "-L":
                physical_mode = False
                i += 1
            elif arg == "-P":
                physical_mode = True
                i += 1
            elif arg == "-e":
                strict_check = True
                i += 1
            elif arg == "-@":
                # Extended attributes - ignored on Linux
                i += 1
            elif arg.startswith("-") and not disable_options:
                # Check for combined options like -LP, -Le, etc.
                if arg.startswith("-") and len(arg) > 1:
                    opt_chars = arg[1:]
                    invalid_char = None
                    for char in opt_chars:
                        if char == "L":
                            physical_mode = False
                        elif char == "P":
                            physical_mode = True
                        elif char == "e":
                            strict_check = True
                        elif char == "@":
                            pass  # Ignored
                        else:
                            invalid_char = char
                            break

                    if invalid_char:
                        return BuiltinResult(
                            success=False,
                            output="",
                            error=f"cd: invalid option -- '{invalid_char}'",
                            returncode=1,
                        )
                i += 1
            else:
                paths.append(arg)
                i += 1

        # Handle remaining arguments (after --)
        while i < len(parts):
            paths.append(parts[i])
            i += 1

        # Determine target directory
        if not paths:
            # cd with no arguments - go to home directory
            target_dir = os.path.expanduser("~")
        elif len(paths) == 1:
            # Handle special cases
            arg = paths[0]
            if arg == "-":
                # cd - (go to previous directory)
                prev_dir = os.getenv("OLDPWD")
                if prev_dir is None:
                    return BuiltinResult(
                        success=False,
                        output="",
                        error="cd: OLDPWD not set",
                        returncode=1,
                    )
                target_dir = prev_dir
            else:
                target_dir = os.path.expanduser(arg)
        else:
            # Too many arguments - might be an unquoted path with spaces
            # Try to combine arguments into a single path
            potential_path = " ".join(paths)
            expanded_path = os.path.expanduser(potential_path)
            if os.path.exists(expanded_path):
                target_dir = expanded_path
                # Resolve according to mode
                if physical_mode:
                    resolved_dir = os.path.realpath(target_dir)
                else:
                    resolved_dir = os.path.abspath(target_dir)
                return BuiltinResult(
                    success=True,
                    output=f'💡 [yellow]Tip: Use quotes for paths with spaces: cd "{potential_path}"[/yellow]',
                    new_cwd=resolved_dir,
                    env_vars_to_set={"OLDPWD": original_dir, "PWD": resolved_dir},
                )
            else:
                return BuiltinResult(
                    success=False,
                    output="",
                    error=f'cd: too many arguments. Use quotes for paths with spaces: cd "{" ".join(paths)}"',
                    returncode=1,
                )

        # Resolve the target path according to mode
        try:
            if physical_mode:
                target_dir = os.path.realpath(target_dir)
            else:
                target_dir = os.path.abspath(target_dir)
        except Exception as e:
            return BuiltinResult(
                success=False,
                output="",
                error=f"cd: invalid path '{paths[0] if paths else '~'}': {e}",
                returncode=1,
            )

        # Check if target exists
        if not os.path.exists(target_dir):
            return BuiltinResult(
                success=False,
                output="",
                error=f"cd: no such file or directory: {target_dir}",
                returncode=1,
            )

        # Check if target is a directory
        if not os.path.isdir(target_dir):
            return BuiltinResult(
                success=False,
                output="",
                error=f"cd: not a directory: {target_dir}",
                returncode=1,
            )

        # For -e flag in -P mode, verify we can get current directory
        if strict_check and physical_mode:
            try:
                # Try to verify the path is accessible
                if not os.path.exists(target_dir):
                    return BuiltinResult(
                        success=False,
                        output="",
                        error="cd: cannot determine current directory",
                        returncode=1,
                    )
            except Exception:
                return BuiltinResult(
                    success=False,
                    output="",
                    error="cd: cannot determine current directory",
                    returncode=1,
                )

        # Prepare output message
        # For cd -, just show the target path (like bash)
        if len(paths) == 1 and paths[0] == "-":
            output = target_dir
        elif target_dir == os.path.expanduser("~"):
            output = "📁 ~ (Home directory)"
        elif original_dir == target_dir:
            output = f"📁 Already in {os.path.basename(target_dir)}"
        else:
            output = f"📁 {os.path.basename(target_dir)} ({target_dir})"

        # Set PWD according to mode
        pwd_value = target_dir

        return BuiltinResult(
            success=True,
            output=output,
            returncode=0,
            new_cwd=target_dir,
            env_vars_to_set={"OLDPWD": original_dir, "PWD": pwd_value},
        )

    @staticmethod
    def handle_pushd(
        command: str, cwd: str, directory_stack: DirectoryStack
    ) -> BuiltinResult:
        """Handle pushd command to push directory onto stack."""
        try:
            parts = shlex.split(command)
        except ValueError:
            # Handle unmatched quotes
            parts = command.split(None, 1)
            if len(parts) > 1:
                parts = ["pushd", parts[1]]

        current_dir = cwd

        if len(parts) == 1:
            # pushd with no arguments - swap top two directories on stack
            if directory_stack.is_empty():
                return BuiltinResult(
                    success=False,
                    output="",
                    error="pushd: no other directory",
                    returncode=1,
                )

            # Swap current dir with top of stack
            top_dir = directory_stack.pop()
            directory_stack.push(current_dir)

            return BuiltinResult(
                success=True,
                output=f"📁 {os.path.basename(top_dir)} ({top_dir})",
                returncode=0,
                new_cwd=top_dir,
                env_vars_to_set={"PWD": top_dir},
            )

        elif len(parts) == 2:
            # pushd with directory argument
            target_dir = os.path.expanduser(parts[1])
        else:
            # Too many arguments - might be an unquoted path with spaces
            # Try to combine arguments 1+ into a single path
            potential_path = " ".join(parts[1:])
            expanded_path = os.path.expanduser(potential_path)
            if os.path.exists(expanded_path):
                target_dir = expanded_path
                return BuiltinResult(
                    success=True,
                    output=f'💡 [yellow]Tip: Use quotes for paths with spaces: pushd "{potential_path}"[/yellow]',
                    returncode=0,
                    new_cwd=os.path.abspath(target_dir),
                    env_vars_to_set={"PWD": os.path.abspath(target_dir)},
                    directory_stack_push=current_dir,
                )
            else:
                return BuiltinResult(
                    success=False,
                    output="",
                    error=f'pushd: too many arguments. Use quotes for paths with spaces: pushd "{" ".join(parts[1:])}"',
                    returncode=1,
                )

        if not os.path.isabs(target_dir):
            target_dir = os.path.join(current_dir, target_dir)

        try:
            target_dir = os.path.abspath(target_dir)

            if os.path.exists(target_dir) and not os.path.isdir(target_dir):
                return BuiltinResult(
                    success=False,
                    output="",
                    error=f"pushd: not a directory: {target_dir}",
                    returncode=1,
                )

            # Push current directory onto stack
            directory_stack.push(current_dir)

            return BuiltinResult(
                success=True,
                output=f"📁 {os.path.basename(target_dir)} ({target_dir})",
                returncode=0,
                new_cwd=target_dir,
                env_vars_to_set={"PWD": target_dir},
            )

        except FileNotFoundError:
            return BuiltinResult(
                success=False,
                output="",
                error=f"pushd: no such file or directory: {target_dir}",
                returncode=1,
            )
        except Exception as e:
            return BuiltinResult(
                success=False,
                output="",
                error=f"pushd: error changing directory: {e}",
                returncode=1,
            )

    @staticmethod
    def handle_popd(
        command: str, cwd: str, directory_stack: DirectoryStack
    ) -> BuiltinResult:
        """Handle popd command to pop directory from stack."""
        if directory_stack.is_empty():
            return BuiltinResult(
                success=False,
                output="",
                error="popd: directory stack empty",
                returncode=1,
            )

        target_dir = directory_stack.pop()

        try:
            target_dir = os.path.abspath(target_dir)
            return BuiltinResult(
                success=True,
                output=f"📁 {os.path.basename(target_dir)} ({target_dir})",
                returncode=0,
                new_cwd=target_dir,
                env_vars_to_set={"PWD": target_dir},
                directory_stack_pop=True,  # Signal that pop was successful
            )

        except Exception as e:
            # Restore the directory to stack if chdir fails
            directory_stack.push(target_dir)
            return BuiltinResult(
                success=False,
                output="",
                error=f"popd: cannot access {target_dir}: {e}",
                returncode=1,
            )

    @staticmethod
    def handle_export(
        command: str, get_exported_vars_func, set_var_func, remove_export_func
    ) -> BuiltinResult:
        """Handle export command.

        Args:
            command: The export command string
            get_exported_vars_func: Function to get exported vars (returns dict)
            set_var_func: Function to set a var (set_var(key, value, export=True))
            remove_export_func: Function to remove export (remove_export(key))
        """
        try:
            parts = shlex.split(command)

            # Parse options
            show_all = False
            remove_export = False
            export_function = False
            var_assignments = []
            disable_options = False

            i = 1
            while i < len(parts):
                arg = parts[i]

                if arg == "--":
                    disable_options = True
                    i += 1
                    break
                elif arg == "-p":
                    show_all = True
                    i += 1
                elif arg == "-n":
                    remove_export = True
                    i += 1
                elif arg == "-f":
                    export_function = True
                    i += 1
                elif arg.startswith("-") and not disable_options:
                    return BuiltinResult(
                        success=False,
                        output="",
                        error=f"export: invalid option -- '{arg[1:]}'",
                        returncode=1,
                    )
                else:
                    var_assignments.append(arg)
                    i += 1

            # Handle remaining arguments (after --)
            while i < len(parts):
                var_assignments.append(parts[i])
                i += 1

            # Handle function export (not supported)
            if export_function:
                return BuiltinResult(
                    success=True,
                    output="⚠️  Function export is not supported in AI Shell",
                    returncode=0,
                )

            # Display exported variables
            if show_all or (len(parts) == 1):
                exported_vars = get_exported_vars_func()
                if not exported_vars:
                    return BuiltinResult(
                        success=True,
                        output="No exported environment variables",
                        returncode=0,
                    )

                lines = ["Exported environment variables:"]
                for key, value in sorted(exported_vars.items()):
                    display_value = value
                    if len(value) > 100:
                        display_value = value[:100] + "..."
                    lines.append(f'declare -x {key}="{display_value}"')

                return BuiltinResult(
                    success=True,
                    output="\n".join(lines),
                    returncode=0,
                )

            # Handle removing export attribute
            if remove_export:
                if not var_assignments:
                    return BuiltinResult(
                        success=False,
                        output="",
                        error="export: -n: option requires an argument",
                        returncode=1,
                    )

                success_count = 0
                for var_name in var_assignments:
                    if remove_export_func(var_name):
                        success_count += 1

                return BuiltinResult(
                    success=True,
                    output=f"✅ Removed export from {success_count} variables",
                    returncode=0,
                    export_vars_to_remove=var_assignments,
                )

            # Handle variable assignments
            if not var_assignments:
                # No arguments, display exported variables
                exported_vars = get_exported_vars_func()
                if not exported_vars:
                    return BuiltinResult(
                        success=True,
                        output="No exported environment variables",
                        returncode=0,
                    )

                lines = ["Exported environment variables:"]
                for key, value in sorted(exported_vars.items()):
                    display_value = value
                    if len(value) > 100:
                        display_value = value[:100] + "..."
                    lines.append(f'declare -x {key}="{display_value}"')

                return BuiltinResult(
                    success=True,
                    output="\n".join(lines),
                    returncode=0,
                )

            # Parse variable assignments
            env_vars_to_set = {}
            success_count = 0

            for assignment in var_assignments:
                if "=" in assignment:
                    # VAR=value format
                    key, value = assignment.split("=", 1)
                    key = key.strip()
                    value = value.strip()

                    # Remove possible quotes
                    if (value.startswith('"') and value.endswith('"')) or (
                        value.startswith("'") and value.endswith("'")
                    ):
                        value = value[1:-1]

                    env_vars_to_set[key] = value
                    success_count += 1
                else:
                    # Only variable name, mark as exported
                    env_vars_to_set[assignment] = ""
                    success_count += 1

            return BuiltinResult(
                success=True,
                output=f"✅ Processed {success_count} variable assignments",
                returncode=0,
                env_vars_to_set=env_vars_to_set,
            )

        except Exception as e:
            return BuiltinResult(
                success=False,
                output="",
                error=f"export: error: {e}",
                returncode=1,
            )

    @staticmethod
    def handle_unset(command: str, unset_var_func) -> BuiltinResult:
        """Handle unset command.

        Args:
            command: The unset command string
            unset_var_func: Function to unset a var (unset_var(key) -> bool)
        """
        try:
            parts = shlex.split(command)

            # Parse options
            unset_func = False
            unset_ref = False
            var_names = []
            disable_options = False

            i = 1
            while i < len(parts):
                arg = parts[i]

                if arg == "--":
                    disable_options = True
                    i += 1
                    break
                elif arg == "-v":
                    i += 1
                elif arg == "-f":
                    unset_func = True
                    i += 1
                elif arg == "-n":
                    unset_ref = True
                    i += 1
                elif arg.startswith("-") and not disable_options:
                    return BuiltinResult(
                        success=False,
                        output="",
                        error=f"unset: invalid option -- '{arg[1:]}'",
                        returncode=1,
                    )
                else:
                    var_names.append(arg)
                    i += 1

            # Handle remaining arguments (after --)
            while i < len(parts):
                var_names.append(parts[i])
                i += 1

            # Check for variable names
            if not var_names:
                return BuiltinResult(
                    success=False,
                    output="",
                    error="unset: usage: unset [-v] [-f] [-n] [name ...]",
                    returncode=1,
                )

            # Handle function unset (not supported)
            if unset_func:
                return BuiltinResult(
                    success=True,
                    output="⚠️  Function unset is not supported in AI Shell",
                    returncode=0,
                )

            # Handle name reference unset (not supported)
            if unset_ref:
                return BuiltinResult(
                    success=True,
                    output="⚠️  Name reference unset is not supported in AI Shell",
                    returncode=0,
                )

            # Unset environment variables
            vars_to_unset = []
            success_count = 0
            not_found_count = 0

            for var_name in var_names:
                if unset_var_func(var_name):
                    success_count += 1
                    vars_to_unset.append(var_name)
                else:
                    not_found_count += 1

            summary = f"Unset {success_count} variables"
            if not_found_count > 0:
                summary += f", {not_found_count} not found"

            return BuiltinResult(
                success=True,
                output=summary,
                returncode=0,
                env_vars_to_unset=vars_to_unset,
            )

        except Exception as e:
            return BuiltinResult(
                success=False,
                output="",
                error=f"unset: error: {e}",
                returncode=1,
            )

    @staticmethod
    def handle_dirs(
        command: str, cwd: str, directory_stack: DirectoryStack
    ) -> BuiltinResult:
        """Handle dirs command to show directory stack."""
        parts = shlex.split(command)

        # Check for options
        clear_stack = False
        disable_options = False

        for part in parts[1:]:
            if part == "--":
                disable_options = True
            elif part == "-c" and not disable_options:
                clear_stack = True
            elif part.startswith("-") and not disable_options:
                return BuiltinResult(
                    success=False,
                    output="",
                    error=f"dirs: invalid option -- '{part[1:]}'",
                    returncode=1,
                )

        if clear_stack:
            # Clear the directory stack
            directory_stack.clear()
            return BuiltinResult(
                success=True,
                output="🗂️  Directory stack cleared",
                returncode=0,
            )

        # Display the directory stack
        current = os.path.basename(cwd)
        stack_display = [f"📁 {current} (current)"]

        for i, dir_path in enumerate(reversed(directory_stack)):
            stack_display.append(
                f"  {len(directory_stack) - i}: {os.path.basename(dir_path)}"
            )

        if directory_stack.is_empty():
            output = "🗂️  Directory stack: only current directory"
        else:
            output = "🗂️  Directory stack:\n" + "\n".join(stack_display)

        return BuiltinResult(
            success=True,
            output=output,
            returncode=0,
        )

    @staticmethod
    def handle_pwd(command: str, cwd: str) -> BuiltinResult:
        """Handle pwd command to print working directory.

        Supports:
        -L: Use logical path (follow symlinks, default behavior)
        -P: Use physical path (resolve symlinks)
        """
        import os as os_module

        parts = shlex.split(command)

        # Parse options
        logical_mode = True  # Default is logical mode (-L)
        disable_options = False

        i = 1
        while i < len(parts):
            arg = parts[i]

            if arg == "--":
                disable_options = True
                i += 1
                break
            elif arg == "-L":
                logical_mode = True
                i += 1
            elif arg == "-P":
                logical_mode = False
                i += 1
            elif arg.startswith("-") and not disable_options:
                return BuiltinResult(
                    success=False,
                    output="",
                    error=f"pwd: invalid option -- '{arg[1:]}'",
                    returncode=1,
                )
            else:
                # pwd doesn't accept arguments
                return BuiltinResult(
                    success=False,
                    output="",
                    error="pwd: too many arguments",
                    returncode=1,
                )

        # Handle remaining arguments (after --)
        if i < len(parts):
            return BuiltinResult(
                success=False,
                output="",
                error="pwd: too many arguments",
                returncode=1,
            )

        # Determine path based on mode
        if logical_mode:
            # Logical mode: prefer PWD environment variable, fallback to cwd parameter
            path = os_module.environ.get("PWD", cwd)
        else:
            # Physical mode: resolve all symlinks in cwd
            path = os_module.path.realpath(cwd)

        return BuiltinResult(
            success=True,
            output=path,
            returncode=0,
        )
