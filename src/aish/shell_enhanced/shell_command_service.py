"""Command execution orchestration extracted from AIShell."""

from __future__ import annotations

import os
import shlex
from typing import TYPE_CHECKING, Any


from ..builtin import BuiltinRegistry
from ..command import CommandDispatcher
from ..i18n import t
from ..tools.bash_executor import UnifiedBashExecutor
from .shell_types import CommandStatus

if TYPE_CHECKING:
    from ..shell import AIShell


class ShellCommandService:
    """Encapsulates command-side execution flows for process_input."""

    def __init__(self, shell: "AIShell") -> None:
        self.shell = shell

    def add_shell_context_entry(
        self,
        command_text: str,
        returncode: int,
        stdout: str | None,
        stderr: str | None,
        offload: dict[str, Any] | None = None,
    ) -> None:
        self.shell.add_to_history(
            command_text,
            returncode,
            stdout or "",
            stderr or "",
            offload=offload,
        )

    async def handle_operator_command(self, user_input: str) -> None:
        cmd_parts = user_input.split()
        if cmd_parts and BuiltinRegistry.is_state_modifying_command(cmd_parts[0]):
            if cmd_parts[0].lower() in ("pushd", "popd", "dirs"):
                self.shell.console.print(
                    f"⚠️  {cmd_parts[0]} 不能在复合命令中使用（如 pushd /etc; pushd /home）",
                    style="yellow",
                )
                self.shell.console.print("💡 请分别执行这些命令，例如：", style="cyan")
                operators = ["&&", "||", ";", "|"]
                for op in operators:
                    if op in user_input:
                        parts = user_input.split(op)
                        for i, part in enumerate(parts):
                            part = part.strip()
                            if part:
                                self.shell.console.print(
                                    f"   {i + 1}. {part}", style="cyan"
                                )
                        break
                return

            executor = UnifiedBashExecutor(
                env_manager=self.shell.env_manager,
                history_manager=None,
            )
            success, stdout, stderr, returncode, _changes = executor.execute(
                user_input,
                source="user",
            )
            self.add_shell_context_entry(user_input, returncode, stdout, stderr)
            await self.shell.history_manager.add_entry(
                command=user_input,
                source="user",
                returncode=returncode,
                stdout=stdout,
                stderr=stderr,
            )
            if stdout:
                self.shell.console.print(stdout.rstrip("\n"))
            if stderr:
                self.shell.console.print(f"❌ {stderr.rstrip(chr(10))}", style="red")
            return

        is_command = await self.shell.is_command_request(user_input, cmd_parts)
        if is_command:
            result = await self.shell.execute_command(user_input)
            returncode, stdout, stderr = result.to_tuple()
            self.add_shell_context_entry(
                user_input,
                returncode,
                stdout,
                stderr,
                result.offload,
            )
            await self.shell.history_manager.add_entry(
                command=user_input,
                source="user",
                returncode=returncode,
                stdout=stdout,
                stderr=stderr,
            )
            return

        # is_command_request() returned False - treat as AI question
        # Note: Even if _command_detection_llm_failed is True, we should still
        # try handle_ai_command rather than ignoring user input completely.
        await self.shell.handle_ai_command(user_input)

    async def handle_special_command(self, user_input: str) -> bool:
        if user_input in ["exit", "quit"]:
            self.shell.logger.info("Exit command received")
            self.shell.running = False
            self.shell._exit_shell()
            return True

        if user_input == "help":
            self.shell.print_help()
            return True

        if user_input == "clear":
            result = await self.shell.execute_command("clear")
            returncode, stdout, stderr = result.to_tuple()
            await self.shell.history_manager.add_entry(
                command=user_input,
                source="user",
                returncode=returncode,
                stdout=stdout,
                stderr=stderr,
            )
            return True

        return False

    async def handle_quick_builtin_command(self, user_input: str) -> bool:
        if user_input == "history" or user_input.startswith("history "):
            await self.shell.handle_history_command(user_input)
            return True

        if not BuiltinRegistry.is_state_modifying_command(user_input):
            return False

        result = await self.shell._execute_builtin_command(user_input)
        self.add_shell_context_entry(
            user_input,
            result.exit_code,
            result.stdout,
            result.stderr,
            result.offload,
        )

        if result.exit_code == 0:
            if result.stdout:
                self.shell.console.print(result.stdout.rstrip("\n"))
            return True

        if result.stderr:
            self.shell.console.print(f"❌ {result.stderr}", style="red")
        if result.stdout:
            self.shell.console.print(result.stdout.rstrip("\n"))
        await self.shell.handle_command_error(
            user_input,
            result.stdout or "",
            result.stderr or "",
        )
        return True

    async def handle_command_or_ai(
        self,
        user_input: str,
        *,
        cmd_parts: list[str],
        parse_error: bool,
    ) -> None:
        if parse_error:
            self.shell.console.print("🤖 parse error", style="grey50")
            await self.shell.handle_ai_command(user_input)
            return

        if cmd_parts and cmd_parts[0] in CommandDispatcher.builtin_commands():
            await self.shell.handle_ai_command(user_input)
            return

        is_command = await self.shell.is_command_request(user_input, cmd_parts)
        if is_command:
            enhanced_command = user_input
            if cmd_parts and cmd_parts[0] == "ls":
                has_color_param = any(
                    param.startswith("--color") or param == "-G"
                    for param in cmd_parts[1:]
                )
                if not has_color_param:
                    enhanced_command = user_input + " --color=always"
                    try:
                        cmd_parts = shlex.split(enhanced_command)
                    except ValueError:
                        cmd_parts = enhanced_command.split()

            with self.shell._safe_cancel_scope() as scope:
                self.shell._current_op_scope = scope
                self.shell.operation_in_progress = True
                try:
                    result = await self.shell.execute_command(enhanced_command)
                    returncode, stdout, stderr = result.to_tuple()
                    self.shell.add_to_history(
                        user_input,
                        returncode,
                        stdout,
                        stderr,
                        offload=result.offload,
                    )
                    await self.shell.history_manager.add_entry(
                        command=user_input,
                        source="user",
                        returncode=returncode,
                        stdout=stdout,
                        stderr=stderr,
                    )

                    if result.status == CommandStatus.CANCELLED:
                        self.shell.console.print(
                            f"[yellow]{t('shell.command_cancelled')}[/yellow]"
                        )
                        return
                    if result.status == CommandStatus.ERROR:
                        await self.shell.handle_command_error(
                            user_input, stdout, stderr
                        )
                        return
                    # Many normal commands output to stderr but don't fail
                finally:
                    self.shell.operation_in_progress = False
                    self.shell._current_op_scope = None
            return

        # is_command_request() returned False - treat as AI question
        await self.shell.handle_ai_command(user_input)

    async def handle_script_call(
        self,
        user_input: str,
        cmd_parts: list[str],
    ) -> None:
        """Handle script execution.

        Args:
            user_input: Full user input string.
            cmd_parts: Parsed command parts (first element is script name).
        """
        script_name = cmd_parts[0]
        script_args = cmd_parts[1:]

        script = self.shell.script_registry.get_script(script_name)
        if not script:
            # Fallback to normal command handling
            await self.handle_command_or_ai(
                user_input, cmd_parts=cmd_parts, parse_error=False
            )
            return

        # Execute the script
        result = await self.shell.script_executor.execute(
            script,
            args=script_args,
        )

        # Apply state changes
        if result.new_cwd:
            try:
                # Cache current working directory before changing
                old_pwd = os.getcwd()
                os.chdir(result.new_cwd)
                self.shell.env_manager.set_var("PWD", result.new_cwd)
                self.shell.env_manager.set_var("OLDPWD", old_pwd)
            except OSError as e:
                self.shell.console.print(
                    f"❌ Failed to change directory: {e}", style="red"
                )

        if result.env_changes:
            for key, value in result.env_changes.items():
                self.shell.env_manager.set_var(key, value)

        # Record history
        self.add_shell_context_entry(
            user_input,
            result.returncode if result.success else 1,
            result.output,
            result.error,
        )
        await self.shell.history_manager.add_entry(
            command=user_input,
            source="user",
            returncode=result.returncode if result.success else 1,
            stdout=result.output,
            stderr=result.error,
        )

        # Display output
        if result.output:
            self.shell.console.print(result.output.rstrip("\n"))
        if result.error:
            self.shell.console.print(f"❌ {result.error.rstrip(chr(10))}", style="red")
