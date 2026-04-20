"""Script executor for running .aish scripts."""

from __future__ import annotations

import logging
import os
import re
import subprocess
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any, Optional

from ..shell.environment import sanitize_subprocess_loader_env

if TYPE_CHECKING:
    from ..llm import LLMSession
    from ..shell.environment import EnvironmentManager
    from .models import Script

logger = logging.getLogger("aish.scripts.executor")


@dataclass
class ScriptExecutionResult:
    """Result of script execution."""

    success: bool
    output: str = ""
    error: str = ""
    new_cwd: Optional[str] = None
    env_changes: dict[str, str] = field(default_factory=dict)
    returncode: int = 0


class ScriptExecutor:
    """Execute .aish scripts with shell and AI integration."""

    # Pattern to match ai "prompt" calls
    AI_CALL_PATTERN = re.compile(r'^\s*ai\s+"([^"]+)"\s*$')
    # Pattern to match ai 'prompt' calls (single quotes)
    AI_CALL_PATTERN_SINGLE = re.compile(r"^\s*ai\s+'([^']+)'\s*$")
    # Pattern to match return statement
    RETURN_PATTERN = re.compile(r'^\s*return\s+(.+)\s*$')
    # Pattern to match cd command
    CD_PATTERN = re.compile(r'^\s*cd\s+(.+)$')
    # Pattern to match export command
    EXPORT_PATTERN = re.compile(r'^\s*export\s+([^=]+)=(.*)$')
    # Pattern to match ask "prompt" calls
    ASK_PATTERN = re.compile(r'^\s*ask\s+"([^"]+)"\s*$')

    def __init__(
        self,
        llm_session: Optional["LLMSession"] = None,
        env_manager: Optional["EnvironmentManager"] = None,
    ):
        """Initialize the script executor.

        Args:
            llm_session: LLMSession for AI calls.
            env_manager: EnvironmentManager for env variable access.
        """
        self.llm_session = llm_session
        self.env_manager = env_manager

    async def execute(
        self,
        script: "Script",
        args: Optional[list[str]] = None,
        stdin_input: Optional[str] = None,
    ) -> ScriptExecutionResult:
        """Execute a script with given arguments.

        Args:
            script: Script object to execute.
            args: List of string arguments.
            stdin_input: Optional input for interactive ask() calls.

        Returns:
            ScriptExecutionResult with output and state changes.
        """
        args = args or []

        # Build runtime environment
        try:
            runtime_env = self._build_runtime_env(script, args)
        except ValueError as e:
            return ScriptExecutionResult(success=False, error=str(e))

        # Parse and execute script body
        try:
            result = await self._execute_body(
                script.content, runtime_env, stdin_input
            )
            return result
        except Exception as e:
            logger.exception("Script execution failed: %s", e)
            return ScriptExecutionResult(success=False, error=str(e))

    def execute_sync(
        self,
        script: "Script",
        args: Optional[list[str]] = None,
        env: Optional[dict[str, str]] = None,
        timeout: int = 5,
    ) -> ScriptExecutionResult:
        """Execute a script synchronously (for hooks and simple scripts).

        This method runs the script as a simple bash command without
        AI calls or interactive features. Used for hook scripts that
        need to be fast.

        Args:
            script: Script object to execute.
            args: List of string arguments.
            env: Optional extra environment variables to merge.
            timeout: Execution timeout in seconds.

        Returns:
            ScriptExecutionResult with output.
        """
        args = args or []

        # Build runtime environment
        try:
            runtime_env = self._build_runtime_env(script, args)
        except ValueError as e:
            return ScriptExecutionResult(success=False, error=str(e))

        # Merge extra environment variables
        if env:
            runtime_env.update(env)

        # Execute as simple bash script
        try:
            result = subprocess.run(
                script.content,
                shell=True,
                executable="/bin/bash",
                capture_output=True,
                text=True,
                env=runtime_env,
                cwd=runtime_env.get("AISH_CWD", os.getcwd()),
                timeout=timeout,
            )

            output = result.stdout.rstrip("\r\n")
            error = result.stderr.strip() if result.returncode != 0 else ""

            # Parse state changes from output
            new_cwd = None
            env_changes: dict[str, str] = {}

            # Check for cd output pattern
            cd_match = re.search(r"\[AISH:CWD:(.+?)\]", output)
            if cd_match:
                new_cwd = cd_match.group(1)
                output = re.sub(r"\[AISH:CWD:.+?\]", "", output).strip()

            return ScriptExecutionResult(
                success=result.returncode == 0,
                output=output,
                error=error,
                new_cwd=new_cwd,
                env_changes=env_changes,
                returncode=result.returncode,
            )

        except subprocess.TimeoutExpired:
            return ScriptExecutionResult(
                success=False, error=f"Script execution timed out after {timeout}s"
            )
        except Exception as e:
            return ScriptExecutionResult(success=False, error=str(e))

    def _build_runtime_env(
        self, script: "Script", args: list[str]
    ) -> dict[str, str]:
        """Build runtime environment with AISH_ARG_* variables.

        Args:
            script: Script object.
            args: List of string arguments.

        Returns:
            Environment dict with AISH_* variables set.

        Raises:
            ValueError: If required argument is missing.
        """
        env = sanitize_subprocess_loader_env(os.environ)
        env["AISH_SCRIPT_DIR"] = script.base_dir
        env["AISH_CWD"] = os.getcwd()
        env["AISH_SCRIPT_NAME"] = script.name

        # Map arguments to AISH_ARG_<name>
        for i, arg_def in enumerate(script.metadata.arguments):
            arg_name_upper = arg_def.name.upper()
            if i < len(args):
                env[f"AISH_ARG_{arg_name_upper}"] = args[i]
            elif arg_def.default is not None:
                env[f"AISH_ARG_{arg_name_upper}"] = arg_def.default
            elif arg_def.required:
                raise ValueError(
                    f"Missing required argument: {arg_def.name} "
                    f"(use: {script.name} <{arg_def.name}>)"
                )

        # Add positional args as AISH_ARG_0, AISH_ARG_1, etc.
        for i, arg in enumerate(args):
            env[f"AISH_ARG_{i}"] = arg

        # Add exported variables from env_manager
        if self.env_manager:
            env.update(self.env_manager.get_exported_vars())

        return env

    async def _execute_body(
        self,
        content: str,
        env: dict[str, str],
        stdin_input: Optional[str] = None,
    ) -> ScriptExecutionResult:
        """Execute script body line by line.

        Args:
            content: Script content.
            env: Runtime environment.
            stdin_input: Optional stdin for interactive prompts.

        Returns:
            ScriptExecutionResult.
        """
        output_lines: list[str] = []
        new_cwd: Optional[str] = None
        env_changes: dict[str, str] = {}
        current_cwd = env.get("AISH_CWD", os.getcwd())

        lines = content.split("\n")
        i = 0

        while i < len(lines):
            line = lines[i].strip()

            # Skip empty lines and comments
            if not line or line.startswith("#"):
                i += 1
                continue

            # Check for return statement
            if self._match_return(line):
                break

            # Check for ai "prompt" call
            ai_result = self._match_ai_call(line)
            if ai_result:
                try:
                    response = await self._execute_ai_call(ai_result)
                    output_lines.append(response)
                    env["AISH_LAST_OUTPUT"] = response
                except Exception as e:
                    return ScriptExecutionResult(
                        success=False, error=f"AI call failed: {e}"
                    )
                i += 1
                continue

            # Check for ask "prompt" call
            ask_match = self.ASK_PATTERN.match(line)
            if ask_match:
                prompt_text = ask_match.group(1)
                if stdin_input:
                    output_lines.append(stdin_input)
                    env["AISH_LAST_OUTPUT"] = stdin_input
                else:
                    # Simple fallback - in real usage this would prompt user
                    output_lines.append(f"[Interactive prompt: {prompt_text}]")
                i += 1
                continue

            # Check for cd command (capture state change)
            cd_match = self.CD_PATTERN.match(line)
            if cd_match:
                target = cd_match.group(1).strip()
                target = self._expand_path(target, current_cwd, env)

                if os.path.isdir(target):
                    new_cwd = os.path.abspath(target)
                    current_cwd = new_cwd
                    env["AISH_CWD"] = new_cwd
                    env["PWD"] = new_cwd
                    output_lines.append(f"📁 {os.path.basename(new_cwd)}")
                else:
                    return ScriptExecutionResult(
                        success=False, error=f"cd: no such directory: {target}"
                    )
                i += 1
                continue

            # Check for export command
            export_match = self.EXPORT_PATTERN.match(line)
            if export_match:
                key = export_match.group(1).strip()
                value = export_match.group(2).strip().strip("\"'")
                env_changes[key] = value
                env[key] = value
                output_lines.append(f"✅ {key}={value}")
                i += 1
                continue

            # Handle multi-line constructs (if, for, while, etc.)
            if line.endswith(";") or line.startswith(("if ", "for ", "while ", "case ")):
                block_lines = [line]
                i += 1
                # Collect until we see 'fi', 'done', 'esac'
                while i < len(lines):
                    block_line = lines[i]
                    block_lines.append(block_line)
                    if (
                        block_line.strip().startswith("fi")
                        or block_line.strip().startswith("done")
                        or block_line.strip().startswith("esac")
                    ):
                        i += 1
                        break
                    i += 1

                # Execute the block
                result = self._execute_bash("\n".join(block_lines), env, current_cwd)
                if result["output"]:
                    output_lines.append(result["output"])
                if result["error"]:
                    return ScriptExecutionResult(
                        success=False,
                        error=result["error"],
                        output="\n".join(output_lines),
                    )
                continue

            # Execute as bash command
            result = self._execute_bash(line, env, current_cwd)
            if result["output"]:
                output_lines.append(result["output"])
            if result["error"]:
                output_lines.append(f"Error: {result['error']}")

            i += 1

        return ScriptExecutionResult(
            success=True,
            output="\n".join(output_lines),
            new_cwd=new_cwd,
            env_changes=env_changes,
        )

    def _match_return(self, line: str) -> bool:
        """Check if line is a return statement."""
        return bool(self.RETURN_PATTERN.match(line))

    def _match_ai_call(self, line: str) -> Optional[str]:
        """Match ai "prompt" or ai 'prompt' call and return prompt string."""
        match = self.AI_CALL_PATTERN.match(line)
        if match:
            return match.group(1)
        match = self.AI_CALL_PATTERN_SINGLE.match(line)
        if match:
            return match.group(1)
        return None

    async def _execute_ai_call(self, prompt: str) -> str:
        """Execute AI call via LLMSession.

        Args:
            prompt: Prompt string to send to AI.

        Returns:
            AI response text.
        """
        if not self.llm_session:
            return "[AI not available - no LLM session]"

        try:
            response = await self.llm_session.completion(
                prompt=prompt,
                stream=False,
            )
            return response
        except Exception as e:
            logger.error("AI call failed: %s", e)
            raise

    def _execute_bash(
        self, command: str, env: dict[str, str], cwd: str
    ) -> dict[str, Any]:
        """Execute bash command.

        Args:
            command: Bash command string.
            env: Environment dict.
            cwd: Working directory.

        Returns:
            Dict with 'output', 'error', 'returncode'.
        """
        try:
            result = subprocess.run(
                command,
                shell=True,
                executable="/bin/bash",
                capture_output=True,
                text=True,
                env=env,
                cwd=cwd,
            )
            return {
                "output": result.stdout.strip(),
                "error": result.stderr.strip() if result.returncode != 0 else "",
                "returncode": result.returncode,
            }
        except Exception as e:
            return {"output": "", "error": str(e), "returncode": -1}

    def _expand_path(
        self, path: str, current_cwd: str, env: dict[str, str]
    ) -> str:
        """Expand path with ~ and environment variables.

        Args:
            path: Path string to expand.
            current_cwd: Current working directory.
            env: Environment dict.

        Returns:
            Expanded absolute path.
        """
        # Expand ~ to home directory
        path = os.path.expanduser(path)

        # Expand environment variables
        path = os.path.expandvars(path)

        # Make absolute
        if not os.path.isabs(path):
            path = os.path.join(current_cwd, path)

        return os.path.abspath(path)
