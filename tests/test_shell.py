"""
Test AI Shell functionality
"""

import getpass
import json
from contextlib import nullcontext
from unittest.mock import AsyncMock, Mock, patch

import anyio
import pytest

from aish.config import ConfigModel
from aish.context_manager import MemoryType
from aish.providers.openai_codex import OPENAI_CODEX_PROVIDER_ADAPTER
from aish.security.security_manager import SecurityDecision
from aish.security.security_policy import RiskLevel
from aish.shell import AIShell
from aish.skills import SkillManager
from aish.wizard.types import ConnectivityResult, ToolSupportResult


def make_shell(config: ConfigModel) -> AIShell:
    skill_manager = SkillManager()
    skill_manager.load_all_skills()
    return AIShell(config=config, skill_manager=skill_manager)


def make_decision(*, allow: bool, require_confirmation: bool) -> SecurityDecision:
    return SecurityDecision(
        level=RiskLevel.LOW,
        allow=allow,
        require_confirmation=require_confirmation,
        analysis={"risk_level": "LOW", "reasons": []},
    )


def shell_context_entries(shell: AIShell) -> list[str]:
    return [
        m["content"]
        for m in shell.context_manager.memories
        if m["memory_type"] == MemoryType.SHELL
    ]


def parse_offload_payload(history_entry: str) -> dict:
    lines = history_entry.splitlines()
    try:
        start = lines.index("<offload>")
        end = lines.index("</offload>")
    except ValueError as exc:
        raise AssertionError("Missing offload payload in shell history entry") from exc
    payload_text = "\n".join(lines[start + 1 : end]).strip()
    if not payload_text:
        raise AssertionError("Empty offload payload in shell history entry")
    return json.loads(payload_text)


def parse_tag_content(history_entry: str, tag: str) -> str:
    lines = history_entry.splitlines()
    start_tag = f"<{tag}>"
    end_tag = f"</{tag}>"
    try:
        start = lines.index(start_tag)
        end = lines.index(end_tag)
    except ValueError as exc:
        raise AssertionError(f"Missing {tag} tag in shell history entry") from exc
    return "\n".join(lines[start + 1 : end])


class TestAIShell:
    """Test class for AIShell"""

    def test_init(self):
        """Test shell initialization"""
        config = ConfigModel(model="test-model")
        shell = make_shell(config)
        assert shell.running is True
        # Context manager manages command history, not a direct list
        assert shell.context_manager is not None

    def test_init_with_custom_model(self):
        """Test shell initialization with custom model"""
        config = ConfigModel(model="gpt-4")
        shell = make_shell(config)
        assert shell.llm_session.model == "gpt-4"

    @pytest.mark.asyncio
    async def test_model_command_show_current(self):
        """/model should show current model"""
        config = ConfigModel(model="test-model")
        shell = make_shell(config)

        with patch.object(shell.console, "print") as mock_print:
            await shell.handle_model_command("/model")

        assert any(
            "test-model" in str(call.args[0]) for call in mock_print.call_args_list
        )

    @pytest.mark.asyncio
    async def test_model_command_switch_success(self):
        """/model should switch model after validation"""
        config = ConfigModel(model="test-model", api_key="test-key")
        skill_manager = SkillManager()
        skill_manager.load_all_skills()
        config_manager = Mock()
        shell = AIShell(
            config=config,
            skill_manager=skill_manager,
            config_manager=config_manager,
        )

        connectivity = ConnectivityResult(ok=True)
        tool_support = ToolSupportResult(supports=True)

        with patch(
            "aish.wizard.verification.run_verification_async",
            new_callable=AsyncMock,
            return_value=(connectivity, tool_support),
        ):
            await shell.handle_model_command("/model new-model")

        assert shell.llm_session.model == "new-model"
        assert shell.context_manager.model == "new-model"
        assert shell.config.model == "new-model"
        config_manager.set_model.assert_called_with("new-model")

    @pytest.mark.asyncio
    async def test_model_command_switch_rejected(self):
        """/model should not switch when validation fails"""
        config = ConfigModel(model="test-model", api_key="test-key")
        shell = make_shell(config)

        connectivity = ConnectivityResult(ok=True)
        tool_support = ToolSupportResult(supports=False, error="no tool support")

        with patch(
            "aish.wizard.verification.run_verification_async",
            new_callable=AsyncMock,
            return_value=(connectivity, tool_support),
        ):
            await shell.handle_model_command("/model blocked-model")

        assert shell.llm_session.model == "test-model"

    @pytest.mark.asyncio
    async def test_model_command_switches_to_openai_codex_without_verification(self):
        """/model should allow OpenAI Codex after local auth validation."""
        config = ConfigModel(
            model="test-model",
            api_key="test-key",
            codex_auth_path="/tmp/codex-auth.json",
        )
        shell = make_shell(config)

        with (
            patch(
                "aish.shell.get_provider_for_model",
                return_value=OPENAI_CODEX_PROVIDER_ADAPTER,
            ),
            patch("aish.providers.openai_codex.load_openai_codex_auth", return_value=Mock()),
            patch(
                "aish.wizard.verification.run_verification_async",
                new_callable=AsyncMock,
            ) as mock_verify,
        ):
            await shell.handle_model_command("/model openai-codex/gpt-5.4")

        assert shell.llm_session.model == "openai-codex/gpt-5.4"
        assert shell.context_manager.model == "openai-codex/gpt-5.4"
        assert shell.config.model == "openai-codex/gpt-5.4"
        mock_verify.assert_not_called()

    @pytest.mark.asyncio
    async def test_model_command_uses_provider_contract_for_validation(self):
        config = ConfigModel(model="test-model", api_key="test-key")
        shell = make_shell(config)
        provider = Mock()
        provider.validate_model_switch = AsyncMock(return_value=None)

        with patch("aish.shell.get_provider_for_model", return_value=provider):
            await shell.handle_model_command("/model custom-provider/model-x")

        provider.validate_model_switch.assert_awaited_once_with(
            model="custom-provider/model-x",
            config=shell.config,
        )
        assert shell.llm_session.model == "custom-provider/model-x"

    def test_init_creates_session_record(self, tmp_path):
        """Each shell start should create a new persisted session record."""
        db_path = tmp_path / "sessions.db"
        config = ConfigModel(model="test-model", session_db_path=str(db_path))
        shell = make_shell(config)

        assert shell.session_record is not None
        assert shell.session_record.model == "test-model"

        from aish.session_store import SessionStore

        store = SessionStore(db_path)
        try:
            record = store.get_session(shell.session_record.session_uuid)
            assert record is not None
            assert record.model == "test-model"
            assert record.run_user == getpass.getuser()
        finally:
            store.close()

    @pytest.mark.asyncio
    async def test_execute_command_success(self):
        """Test successful command execution"""
        config = ConfigModel(model="test-model")
        shell = make_shell(config)

        # Import CommandResult and CommandStatus for the test
        from aish.shell import CommandResult, CommandStatus

        with (
            patch.object(shell, "execute_command_with_pty") as mock_exec,
            patch.object(
                shell.security_manager,
                "decide",
                return_value=make_decision(allow=True, require_confirmation=False),
            ),
        ):
            mock_exec.return_value = CommandResult(
                CommandStatus.SUCCESS, 0, "test output", ""
            )
            result = await shell.execute_command_with_security("echo test")

            assert result.exit_code == 0
            assert result.stdout == "test output"
            assert result.stderr == ""
            assert result.status == CommandStatus.SUCCESS

    @pytest.mark.asyncio
    async def test_execute_command_cancelled_by_user(self):
        """Test command cancelled by user"""
        config = ConfigModel(model="test-model")
        shell = make_shell(config)

        from aish.shell import CommandStatus, LLMCallbackResult

        with (
            patch.object(
                shell.security_manager,
                "decide",
                return_value=make_decision(allow=True, require_confirmation=True),
            ),
            patch(
                "aish.shell.to_thread.run_sync",
                new_callable=AsyncMock,
                return_value=LLMCallbackResult.CANCEL,
            ),
        ):
            result = await shell.execute_command_with_security("echo test")

            assert result.status == CommandStatus.CANCELLED
            assert "User cancelled" in result.stderr

    @pytest.mark.asyncio
    async def test_execute_command_error(self):
        """Test failed command execution handling"""
        config = ConfigModel(model="test-model")
        shell = make_shell(config)

        from aish.shell import CommandResult, CommandStatus

        with (
            patch.object(shell, "execute_command_with_pty") as mock_exec,
            patch.object(
                shell.security_manager,
                "decide",
                return_value=make_decision(allow=True, require_confirmation=False),
            ),
        ):
            mock_exec.return_value = CommandResult(
                CommandStatus.ERROR, 1, "", "command not found"
            )
            result = await shell.execute_command_with_security("invalid_command")

            assert result.exit_code == 1
            assert result.stdout == ""
            assert result.stderr == "command not found"
            assert result.status == CommandStatus.ERROR

    @pytest.mark.asyncio
    async def test_asyncio_cancelled_error_handling(self):
        """Test handling of asyncio.CancelledError"""
        config = ConfigModel(model="test-model")
        shell = make_shell(config)

        from aish.shell import CommandStatus

        cancelled_exc = anyio.get_cancelled_exc_class()
        with (
            patch.object(shell, "execute_command", side_effect=cancelled_exc),
            patch.object(
                shell.security_manager,
                "decide",
                return_value=make_decision(allow=True, require_confirmation=False),
            ),
        ):
            result = await shell.execute_command_with_security("echo test")

            assert result.status == CommandStatus.ERROR
            assert "Execution cancelled by system" in result.stderr

    @pytest.mark.asyncio
    async def test_execute_command_failure(self):
        """Test failed command execution"""
        config = ConfigModel(model="test-model")
        shell = make_shell(config)

        # Import CommandResult and CommandStatus for the test
        from aish.shell import CommandResult, CommandStatus

        with patch.object(shell, "execute_command_with_pty") as mock_exec:
            mock_exec.return_value = CommandResult(
                CommandStatus.ERROR, 1, "", "command not found"
            )
            result = await shell.execute_command("invalid_command")

            assert result.exit_code == 1
            assert result.stdout == ""
            assert result.stderr == "command not found"
            assert result.status == CommandStatus.ERROR

    @pytest.mark.asyncio
    async def test_ask_llm_mock(self):
        """Test LLM interaction with mocking"""
        config = ConfigModel(model="test-model")
        shell = make_shell(config)

        with patch.object(
            shell.llm_session, "process_input", new_callable=AsyncMock
        ) as mock_llm:
            mock_llm.return_value = "Test response"

            response = await shell.ask_oracle("test question")

            assert response == "Test response"
            mock_llm.assert_called_once()

    @pytest.mark.asyncio
    async def test_ask_llm_error(self):
        """Test LLM interaction error handling"""
        config = ConfigModel(model="test-model")
        shell = make_shell(config)

        with patch.object(
            shell.llm_session, "process_input", new_callable=AsyncMock
        ) as mock_llm:
            mock_llm.side_effect = Exception("LLM error")

            # Should handle the error gracefully
            try:
                await shell.ask_oracle("test question")
            except Exception as e:
                assert str(e) == "LLM error"

    @pytest.mark.asyncio
    async def test_process_input_exit(self):
        """Test exit command processing"""
        config = ConfigModel(model="test-model")
        shell = make_shell(config)

        await shell.process_input("exit")
        assert shell.running is False

    @pytest.mark.asyncio
    async def test_process_input_help(self):
        """Test help command processing"""
        config = ConfigModel(model="test-model")
        shell = make_shell(config)

        with patch.object(shell, "print_help") as mock_help:
            await shell.process_input("help")
            mock_help.assert_called_once()

    @pytest.mark.asyncio
    async def test_process_input_ai_command(self):
        """Test AI command processing"""
        config = ConfigModel(model="test-model")
        shell = make_shell(config)

        with patch.object(
            shell, "handle_ai_command", new_callable=AsyncMock
        ) as mock_ai:
            await shell.process_input("; what is python?")
            mock_ai.assert_called_once()

    @pytest.mark.asyncio
    async def test_process_input_explain_command(self):
        """Test explain command processing"""
        config = ConfigModel(model="test-model")
        shell = make_shell(config)

        with patch.object(
            shell, "handle_ai_command", new_callable=AsyncMock
        ) as mock_ai:
            await shell.process_input("; explain ls -la")
            mock_ai.assert_called_once()

    @pytest.mark.asyncio
    async def test_process_input_suggest_command(self):
        """Test suggest command processing"""
        config = ConfigModel(model="test-model")
        shell = make_shell(config)

        with patch.object(
            shell, "handle_ai_command", new_callable=AsyncMock
        ) as mock_ai:
            await shell.process_input("; suggest find large files")
            mock_ai.assert_called_once()

    @pytest.mark.asyncio
    async def test_process_input_model_command(self):
        """Process input should route /model to model handler."""
        config = ConfigModel(model="test-model")
        shell = make_shell(config)

        with patch.object(
            shell, "handle_model_command", new_callable=AsyncMock
        ) as mock_model:
            await shell.process_input("/model")
            mock_model.assert_awaited_once_with("/model")

    @pytest.mark.asyncio
    async def test_process_input_regular_command(self):
        """Test regular command processing"""
        config = ConfigModel(model="test-model")
        shell = make_shell(config)
        shell.config.bash_output_offload.preview_bytes = 8

        # Import CommandResult and CommandStatus for the test
        from aish.shell import CommandResult, CommandStatus

        with patch.object(
            shell, "is_command_request", new_callable=AsyncMock
        ) as mock_is_cmd:
            with patch.object(
                shell, "execute_command", new_callable=AsyncMock
            ) as mock_exec:
                with patch.object(
                    shell.history_manager, "add_entry", new_callable=AsyncMock
                ) as mock_history_add:
                    with patch.object(
                        shell, "handle_error_detect", new_callable=AsyncMock
                    ) as mock_error_detect:
                        with patch(
                            "aish.shell.anyio.CancelScope", return_value=nullcontext()
                        ):
                            baseline_shell_count = len(shell_context_entries(shell))

                            mock_is_cmd.return_value = True
                            mock_exec.return_value = CommandResult(
                                CommandStatus.SUCCESS,
                                0,
                                "abcdefghijk",
                                "stderr-too-long",
                                offload={
                                    "status": "offloaded",
                                    "stdout_path": "/tmp/stdout.txt",
                                    "stdout_clean_path": "/tmp/stdout.clean.txt",
                                    "stderr_path": "/tmp/stderr.txt",
                                    "stderr_clean_path": "/tmp/stderr.clean.txt",
                                    "meta_path": "/tmp/meta.json",
                                    "keep_bytes": 4096,
                                    "hint": "showing last 4096 bytes",
                                },
                            )

                            await shell.process_input("ls")

                            mock_is_cmd.assert_called_once()
                            mock_exec.assert_called_once_with("ls --color=always")
                            mock_history_add.assert_awaited_once()
                            mock_error_detect.assert_not_awaited()

                            shell_entries = shell_context_entries(shell)
                            assert len(shell_entries) == baseline_shell_count + 1
                            entry = shell_entries[-1]
                            assert "$ ls → ✓ (exit 0)" in entry
                            assert "<stdout>\nabcdefgh" in entry
                            assert "... [stdout preview truncated to 8 bytes]" in entry
                            assert "<stderr>\nstderr-t" in entry
                            assert "... [stderr preview truncated to 8 bytes]" in entry
                            assert "<return_code>\n0\n</return_code>" in entry
                            assert "<offload>" in entry
                            offload_payload = parse_offload_payload(entry)
                            assert offload_payload["status"] == "offloaded"
                            assert offload_payload["stdout_path"] == "/tmp/stdout.txt"
                            assert (
                                offload_payload["stdout_clean_path"]
                                == "/tmp/stdout.clean.txt"
                            )
                            assert offload_payload["stderr_path"] == "/tmp/stderr.txt"
                            assert (
                                offload_payload["stderr_clean_path"]
                                == "/tmp/stderr.clean.txt"
                            )
                            assert offload_payload["meta_path"] == "/tmp/meta.json"

    @pytest.mark.asyncio
    async def test_process_input_regular_command_success_with_stderr_no_error_detect(
        self,
    ):
        """Successful command with stderr should NOT trigger error detection.
        Many commands (dd, grep -v, etc.) output to stderr for progress, not errors."""
        config = ConfigModel(model="test-model")
        shell = make_shell(config)

        from aish.shell import CommandResult, CommandStatus

        with patch.object(
            shell, "is_command_request", new_callable=AsyncMock
        ) as mock_is_cmd:
            with patch.object(
                shell, "execute_command", new_callable=AsyncMock
            ) as mock_exec:
                with patch.object(
                    shell.history_manager, "add_entry", new_callable=AsyncMock
                ) as mock_history_add:
                    with patch.object(
                        shell, "handle_error_detect", new_callable=AsyncMock
                    ) as mock_error_detect:
                        with patch(
                            "aish.shell.anyio.CancelScope", return_value=nullcontext()
                        ):
                            mock_is_cmd.return_value = True
                            mock_exec.return_value = CommandResult(
                                CommandStatus.SUCCESS,
                                0,
                                "ok",
                                "one two three four",  # stderr with >3 words
                                offload={
                                    "status": "inline",
                                    "reason": "below_threshold",
                                },
                            )

                            await shell.process_input("ls")

                            mock_is_cmd.assert_called_once()
                            mock_exec.assert_called_once_with("ls --color=always")
                            mock_history_add.assert_awaited_once()
                            # SUCCESS: should NOT trigger error detection
                            mock_error_detect.assert_not_awaited()

    @pytest.mark.asyncio
    async def test_process_input_command_detection_llm_failed_still_calls_ai(self):
        """When command detection LLM fails, user input should still be processed as AI question."""
        config = ConfigModel(model="test-model")
        shell = make_shell(config)
        shell._command_detection_llm_failed = (
            True  # Simulate LLM failure during command detection
        )

        with patch.object(
            shell, "is_command_request", new_callable=AsyncMock
        ) as mock_is_cmd:
            with patch.object(
                shell, "handle_ai_command", new_callable=AsyncMock
            ) as mock_ai:
                mock_is_cmd.return_value = False  # Not recognized as command

                await shell.process_input("some unknown text")

                mock_is_cmd.assert_called_once()
                # Even though _command_detection_llm_failed=True, handle_ai_command should be called
                mock_ai.assert_awaited_once_with("some unknown text")

    def test_shell_pty_preexec_setup_has_fcntl_scope(self):
        """Regression: preexec_fn setup must be callable without NameError on fcntl."""
        import os
        import pty
        import signal
        import subprocess
        import termios

        from aish.shell_enhanced import shell_pty_executor

        master_fd = None
        slave_fd = None
        process = None
        try:
            master_fd, slave_fd = pty.openpty()

            def preexec_setup() -> None:
                os.setsid()
                shell_pty_executor.fcntl.ioctl(slave_fd, termios.TIOCSCTTY, 0)

            process = subprocess.Popen(
                "true",
                shell=True,
                executable="/bin/bash",
                stdin=slave_fd,
                stdout=slave_fd,
                stderr=slave_fd,
                preexec_fn=preexec_setup,
            )
            os.close(slave_fd)
            slave_fd = None

            returncode = process.wait(timeout=2)
            assert returncode == 0
        finally:
            if master_fd is not None:
                try:
                    os.close(master_fd)
                except OSError:
                    pass
            if slave_fd is not None:
                try:
                    os.close(slave_fd)
                except OSError:
                    pass
            if process and process.poll() is None:
                try:
                    os.killpg(process.pid, signal.SIGTERM)
                    process.wait(timeout=1)
                except Exception:
                    pass

    @pytest.mark.asyncio
    async def test_process_input_compound_state_modifying_command_adds_shell_context(
        self,
    ):
        """Compound commands in UnifiedBashExecutor path should be recorded to SHELL context."""
        config = ConfigModel(model="test-model")
        shell = make_shell(config)

        baseline_shell_count = len(shell_context_entries(shell))

        with patch(
            "aish.tools.bash_executor.UnifiedBashExecutor.execute",
            return_value=(True, "/tmp\n", "", 0, []),
        ) as mock_unified_exec:
            with patch.object(
                shell.history_manager, "add_entry", new_callable=AsyncMock
            ) as mock_history_add:
                await shell.process_input("cd /tmp && pwd")

                mock_unified_exec.assert_called_once_with(
                    "cd /tmp && pwd", source="user"
                )
                mock_history_add.assert_awaited_once()

        shell_entries = shell_context_entries(shell)
        assert len(shell_entries) == baseline_shell_count + 1
        entry = shell_entries[-1]
        assert "$ cd /tmp && pwd → ✓ (exit 0)" in entry
        assert "<stdout>\n/tmp" in entry
        assert parse_tag_content(entry, "stderr") == ""
        assert "<return_code>\n0\n</return_code>" in entry
        assert "<offload>" in entry
        offload_payload = parse_offload_payload(entry)
        assert offload_payload["status"] == "inline"
        assert offload_payload["reason"] == "not_offloaded"

    @pytest.mark.asyncio
    async def test_process_input_compound_regular_command_adds_shell_context(self):
        """Compound commands in execute_command path should be recorded to SHELL context."""
        config = ConfigModel(model="test-model")
        shell = make_shell(config)

        from aish.shell import CommandResult, CommandStatus

        baseline_shell_count = len(shell_context_entries(shell))

        with patch.object(
            shell, "is_command_request", new_callable=AsyncMock
        ) as mock_is_cmd:
            with patch.object(
                shell, "execute_command", new_callable=AsyncMock
            ) as mock_exec:
                with patch.object(
                    shell.history_manager, "add_entry", new_callable=AsyncMock
                ) as mock_history_add:
                    mock_is_cmd.return_value = True
                    mock_exec.return_value = CommandResult(
                        CommandStatus.SUCCESS,
                        0,
                        "done",
                        "",
                        offload={"status": "inline", "reason": "below_threshold"},
                    )
                    await shell.process_input("echo hi | cat")

                    mock_is_cmd.assert_called_once()
                    mock_exec.assert_called_once_with("echo hi | cat")
                    mock_history_add.assert_awaited_once()

        shell_entries = shell_context_entries(shell)
        assert len(shell_entries) == baseline_shell_count + 1
        entry = shell_entries[-1]
        assert "$ echo hi | cat → ✓ (exit 0)" in entry
        assert "<stdout>\ndone" in entry
        assert parse_tag_content(entry, "stderr") == ""
        assert "<return_code>\n0\n</return_code>" in entry
        assert "<offload>" in entry
        offload_payload = parse_offload_payload(entry)
        assert offload_payload["status"] == "inline"
        assert offload_payload["reason"] == "below_threshold"

    @pytest.mark.asyncio
    @pytest.mark.parametrize(
        ("exit_code", "stdout", "stderr"),
        [
            (0, "/tmp\n", ""),
            (1, "", "cd: no such file or directory"),
        ],
    )
    async def test_process_input_builtin_cd_adds_shell_context(
        self, exit_code: int, stdout: str, stderr: str
    ):
        """Builtin quick-path commands should always be recorded to SHELL context."""
        config = ConfigModel(model="test-model")
        shell = make_shell(config)

        from aish.shell import CommandResult, CommandStatus

        baseline_shell_count = len(shell_context_entries(shell))

        status = CommandStatus.SUCCESS if exit_code == 0 else CommandStatus.ERROR
        with patch.object(
            shell, "_execute_builtin_command", new_callable=AsyncMock
        ) as mock_builtin_exec:
            with patch.object(
                shell, "handle_command_error", new_callable=AsyncMock
            ) as mock_handle_error:
                mock_builtin_exec.return_value = CommandResult(
                    status,
                    exit_code,
                    stdout,
                    stderr,
                    offload={"status": "inline", "reason": "builtin_command"},
                )
                await shell.process_input("cd /tmp")

                mock_builtin_exec.assert_awaited_once_with("cd /tmp")
                if exit_code == 0:
                    mock_handle_error.assert_not_awaited()
                else:
                    mock_handle_error.assert_awaited_once()

        shell_entries = shell_context_entries(shell)
        assert len(shell_entries) == baseline_shell_count + 1
        entry = shell_entries[-1]
        if exit_code == 0:
            assert "$ cd /tmp → ✓ (exit 0)" in entry
            assert "<stdout>\n/tmp" in entry
            assert parse_tag_content(entry, "stderr") == ""
            assert "<return_code>\n0\n</return_code>" in entry
        else:
            assert "$ cd /tmp → ✗ (exit 1)" in entry
            assert parse_tag_content(entry, "stdout") == ""
            assert "<stderr>\ncd: no such file or directory" in entry
            assert "<return_code>\n1\n</return_code>" in entry
        assert "<offload>" in entry
        offload_payload = parse_offload_payload(entry)
        assert offload_payload["status"] == "inline"
        assert offload_payload["reason"] == "builtin_command"

    def test_print_welcome(self):
        """Test welcome message printing"""
        config = ConfigModel(model="test-model")
        shell = make_shell(config)

        with patch.object(shell.console, "print") as mock_print:
            shell.print_welcome()
            mock_print.assert_called_once()

    def test_print_help(self):
        """Test help message printing"""
        config = ConfigModel(model="test-model")
        shell = make_shell(config)

        with patch.object(shell.console, "print") as mock_print:
            shell.print_help()
            mock_print.assert_called_once()

    @pytest.mark.asyncio
    async def test_handle_ai_command_empty(self):
        """Test AI command with empty question"""
        config = ConfigModel(model="test-model")
        shell = make_shell(config)

        # Should return early for empty question
        result = await shell.handle_ai_command("")
        assert result is None

    @pytest.mark.asyncio
    async def test_handle_explain_command_empty(self):
        """Test explain command with empty command"""
        config = ConfigModel(model="test-model")
        shell = make_shell(config)

        # Should return early for empty question
        result = await shell.handle_ai_command("")
        assert result is None

    @pytest.mark.asyncio
    async def test_handle_suggest_command_empty(self):
        """Test suggest command with empty task"""
        config = ConfigModel(model="test-model")
        shell = make_shell(config)

        # Should return early for empty question
        result = await shell.handle_ai_command("")
        assert result is None

    def test_get_heredoc_delimiter(self):
        """Test heredoc delimiter detection"""
        config = ConfigModel(model="test-model")
        shell = make_shell(config)

        # Basic heredoc
        assert shell._get_heredoc_delimiter("cat << EOF") == "EOF"

        # Heredoc with quoted delimiter
        assert shell._get_heredoc_delimiter("cat << 'EOF'") == "EOF"
        assert shell._get_heredoc_delimiter('cat << "EOF"') == "EOF"

        # Heredoc with escaped delimiter
        assert shell._get_heredoc_delimiter("cat << \\EOF") == "EOF"

        # No heredoc
        assert shell._get_heredoc_delimiter("cat file.txt") is None
        assert shell._get_heredoc_delimiter("echo hello") is None

        # Heredoc with command before
        assert shell._get_heredoc_delimiter("wc -w << EOF") == "EOF"

        # Different delimiter names
        assert shell._get_heredoc_delimiter("cat << END") == "END"
        assert shell._get_heredoc_delimiter("cat << MY_DELIMITER") == "MY_DELIMITER"
        assert shell._get_heredoc_delimiter("cat << delim123") == "delim123"
