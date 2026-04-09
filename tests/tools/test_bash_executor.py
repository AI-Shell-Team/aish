"""测试统一 Bash 执行器"""

import os
import subprocess
import sys

import pytest

sys.path.insert(0, "src")

from aish.shell.environment import EnvironmentManager
from aish.tools.bash_executor import UnifiedBashExecutor


class TestUnifiedBashExecutor:
    """测试统一执行器"""

    @pytest.fixture
    def env_manager(self):
        return EnvironmentManager()

    @pytest.fixture
    def executor(self, env_manager):
        return UnifiedBashExecutor(env_manager=env_manager)

    def test_simple_command(self, executor):
        """测试简单命令"""
        success, stdout, stderr, retcode, changes = executor.execute("echo hello")

        assert success is True
        assert "hello" in stdout
        assert retcode == 0

    def test_cd_command(self, executor):
        """测试 cd 命令"""
        original_cwd = os.getcwd()

        success, stdout, stderr, retcode, changes = executor.execute("cd /tmp && pwd")

        assert success is True
        assert "/tmp" in stdout
        assert changes["cwd_changed"] is True
        assert changes["new_cwd"] == "/tmp"

        os.chdir(original_cwd)

    def test_export_command(self, executor, env_manager):
        """测试 export 命令"""
        env_manager.unset_var("TEST_VAR")
        success, stdout, stderr, retcode, changes = executor.execute(
            "export TEST_VAR=unified_executor"
        )

        assert success is True
        captured_value = changes["env_added"].get("TEST_VAR")
        if captured_value is None:
            captured_value = changes["env_modified"].get("TEST_VAR", {}).get("new")
        assert captured_value == "unified_executor"
        assert env_manager.get_var("TEST_VAR") == "unified_executor"

    def test_exit_command(self, executor):
        """测试 exit 命令（关键测试！）"""
        original_cwd = os.getcwd()

        # cd 然后 exit - 应该能捕获状态
        success, stdout, stderr, retcode, changes = executor.execute(
            "cd /tmp && exit 0"
        )

        assert retcode == 0
        assert changes["cwd_changed"] is True
        assert changes["new_cwd"] == "/tmp"

        os.chdir(original_cwd)

    def test_semicolon_with_exit(self, executor):
        """测试 ; 分隔的 exit（关键测试！）"""
        original_cwd = os.getcwd()

        success, stdout, stderr, retcode, changes = executor.execute("cd /tmp; exit 0")

        assert retcode == 0
        assert changes["cwd_changed"] is True
        assert changes["new_cwd"] == "/tmp"

        os.chdir(original_cwd)

    def test_output_is_clean(self, executor):
        """测试输出是干净的（不包含状态信息）"""
        success, stdout, stderr, retcode, changes = executor.execute(
            "echo 'test output' && cd /tmp"
        )

        # 输出不包含状态标记
        assert "PWD_AISH_MARKER" not in stdout
        assert "declare -x" not in stdout
        assert "test output" in stdout

    def test_pipe_command(self, executor):
        """测试管道命令"""
        success, stdout, stderr, retcode, changes = executor.execute(
            "echo 'hello world' | grep hello"
        )

        assert success is True
        assert "hello" in stdout

    def test_export_then_exit(self, executor, env_manager):
        """测试 export 然后 exit"""
        env_manager.unset_var("TEST_EXIT")
        success, stdout, stderr, retcode, changes = executor.execute(
            "export TEST_EXIT=789 && exit 0"
        )

        assert retcode == 0
        captured_value = changes["env_added"].get("TEST_EXIT")
        if captured_value is None:
            captured_value = changes["env_modified"].get("TEST_EXIT", {}).get("new")
        assert captured_value == "789"
        assert env_manager.get_var("TEST_EXIT") == "789"

    def test_invalid_utf8_output_does_not_crash(self, executor):
        """测试非 UTF-8 输出不会触发解码异常"""
        success, stdout, stderr, retcode, changes = executor.execute(
            "printf '\\xE8\\xFF'"
        )

        # 命令应执行成功，且输出经过容错解码
        assert retcode == 0
        assert success is True
        assert "Error: 'utf-8' codec can't decode" not in stderr
        assert stdout != ""

    def test_default_execute_does_not_pass_timeout(self, executor, monkeypatch):
        """测试默认执行不向 subprocess 传递 timeout"""
        captured = {}

        def fake_run(*args, **kwargs):
            captured.update(kwargs)
            return subprocess.CompletedProcess(
                args=kwargs.get("args") or args[0],
                returncode=0,
                stdout=b"ok\n",
                stderr=b"",
            )

        monkeypatch.setattr(subprocess, "run", fake_run)

        success, stdout, stderr, retcode, changes = executor.execute("echo ok")

        assert success is True
        assert retcode == 0
        assert stdout == "ok\n"
        assert "timeout" not in captured


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
