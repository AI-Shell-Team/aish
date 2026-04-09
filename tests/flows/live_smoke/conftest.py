from __future__ import annotations

import io
import os
import re
import subprocess
import sys
import time
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any, Iterator

import pytest
import yaml


@dataclass
class LiveSmokeProviderConfig:
    model: str
    api_key: str
    api_base: str | None


@dataclass
class LiveSmokePaths:
    root: Path
    home: Path
    xdg_config_home: Path
    xdg_data_home: Path
    workspace: Path
    diagnostics_dir: Path


@dataclass
class LiveSmokeCommandResult:
    argv: list[str]
    cwd: str
    returncode: int
    stdout: str
    stderr: str
    duration_seconds: float

    @property
    def combined_output(self) -> str:
        return f"{self.stdout}\n{self.stderr}".strip()


@dataclass
class LiveSmokeChatResult:
    argv: list[str]
    cwd: str
    transcript: str
    exitstatus: int | None
    signalstatus: int | None
    duration_seconds: float
    expected_token_seen: bool = False


_SHELL_PROMPT_PATTERNS = (
    re.compile(r"\x1b\[[0-9;?]*m>\x1b\[[0-9;?]*[A-Za-z]"),
    re.compile(r">\x1b\[[0-9;?]*[A-Za-z]"),
    re.compile(r"> "),
)

_ANSI_ESCAPE_RE = re.compile(r"\x1b\[[0-9;?]*[ -/]*[@-~]")
_CONFIRMATION_PROMPTS = (
    "Your choice (default: n):",
    "你的选择（默认：n）：",
)


def _expect_shell_prompt(child: Any) -> None:
    child.expect(list(_SHELL_PROMPT_PATTERNS))


def _strip_terminal_control(text: str) -> str:
    stripped = _ANSI_ESCAPE_RE.sub("", text)
    return stripped.replace("\r", "")


def _workspace_prompt_text(workspace: Path) -> str:
    return f"{workspace} > "


def _transcript_ends_with_prompt(text: str, prompt_text: str) -> bool:
    return text.rstrip("\n").endswith(prompt_text.rstrip())


def _write_config_file(
    *,
    config_file: Path,
    model: str,
    api_key: str,
    api_base: str | None,
) -> Path:
    config_data = {
        "api_key": api_key,
        "enable_scripts": False,
        "max_tokens": 32,
        "model": model,
        "prompt_theme": "default",
        "temperature": 0,
    }
    if api_base:
        config_data["api_base"] = api_base

    config_file.write_text(yaml.safe_dump(config_data, sort_keys=True), encoding="utf-8")
    return config_file


def _close_shell(child: Any, timeout: float = 5.0) -> None:
    pexpect = pytest.importorskip("pexpect")
    try:
        child.sendcontrol("d")
        child.expect(pexpect.EOF, timeout=timeout)
    except pexpect.TIMEOUT:
        child.close(force=True)


def _env_summary(env: dict[str, str]) -> dict[str, str]:
    keys = (
        "HOME",
        "PATH",
        "PYTHONPATH",
        "TERM",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "AISH_LIVE_SMOKE_MODEL",
        "AISH_LIVE_SMOKE_API_BASE",
    )
    summary: dict[str, str] = {}
    for key in keys:
        value = env.get(key)
        if value:
            summary[key] = value
    return summary


@pytest.hookimpl(hookwrapper=True)
def pytest_runtest_makereport(item: pytest.Item, call: pytest.CallInfo[Any]):
    outcome = yield
    report = outcome.get_result()
    setattr(item, f"rep_{report.when}", report)


@pytest.fixture
def live_smoke_paths(tmp_path: Path) -> LiveSmokePaths:
    root = tmp_path / "live-smoke"
    home = root / "home"
    xdg_config_home = root / "xdg-config"
    xdg_data_home = root / "xdg-data"
    workspace = root / "workspace"
    diagnostics_dir = root / "diagnostics"

    for path in (home, xdg_config_home, xdg_data_home, workspace, diagnostics_dir):
        path.mkdir(parents=True, exist_ok=True)

    return LiveSmokePaths(
        root=root,
        home=home,
        xdg_config_home=xdg_config_home,
        xdg_data_home=xdg_data_home,
        workspace=workspace,
        diagnostics_dir=diagnostics_dir,
    )


@pytest.fixture
def live_smoke_env(live_smoke_paths: LiveSmokePaths) -> dict[str, str]:
    env = os.environ.copy()
    existing_pythonpath = env.get("PYTHONPATH", "")
    repo_root = Path(__file__).resolve().parents[2]
    src_path = str(repo_root / "src")
    pythonpath_parts = [src_path]
    if existing_pythonpath:
        pythonpath_parts.append(existing_pythonpath)

    env.update(
        {
            "HOME": str(live_smoke_paths.home),
            "LANG": "en_US.UTF-8",
            "LC_ALL": "en_US.UTF-8",
            "PYTHONPATH": os.pathsep.join(pythonpath_parts),
            "PYTHONUNBUFFERED": "1",
            "TERM": env.get("TERM", "xterm-256color"),
            "XDG_CONFIG_HOME": str(live_smoke_paths.xdg_config_home),
            "XDG_DATA_HOME": str(live_smoke_paths.xdg_data_home),
        }
    )
    env.pop("AISH_CONFIG_DIR", None)
    return env


@pytest.fixture
def live_smoke_provider_config() -> LiveSmokeProviderConfig:
    model = os.environ.get("AISH_LIVE_SMOKE_MODEL", "").strip()
    api_key = os.environ.get("AISH_LIVE_SMOKE_API_KEY", "").strip()
    api_base = os.environ.get("AISH_LIVE_SMOKE_API_BASE", "").strip() or None

    if not model:
        pytest.skip("missing AISH_LIVE_SMOKE_MODEL for live smoke test")
    if not api_key:
        pytest.skip("missing AISH_LIVE_SMOKE_API_KEY for live smoke test")
    if api_base and "/" not in model:
        pytest.fail(
            "AISH_LIVE_SMOKE_MODEL must include a provider prefix when AISH_LIVE_SMOKE_API_BASE is set, "
            f"for example openai/{model}"
        )

    return LiveSmokeProviderConfig(model=model, api_key=api_key, api_base=api_base)


@pytest.fixture
def live_smoke_config_file(
    live_smoke_paths: LiveSmokePaths,
    live_smoke_provider_config: LiveSmokeProviderConfig,
) -> Path:
    config_dir = live_smoke_paths.xdg_config_home / "aish"
    config_dir.mkdir(parents=True, exist_ok=True)
    config_file = config_dir / "config.yaml"
    return _write_config_file(
        config_file=config_file,
        model=live_smoke_provider_config.model,
        api_key=live_smoke_provider_config.api_key,
        api_base=live_smoke_provider_config.api_base,
    )


@pytest.fixture
def live_smoke_artifacts(
    request: pytest.FixtureRequest, live_smoke_paths: LiveSmokePaths
) -> Iterator[list[dict[str, Any]]]:
    artifacts: list[dict[str, Any]] = []
    yield artifacts

    report = getattr(request.node, "rep_call", None)
    if report is None or not report.failed:
        return

    node_name = request.node.name.replace(os.sep, "_")
    artifact_dir = live_smoke_paths.diagnostics_dir / node_name
    artifact_dir.mkdir(parents=True, exist_ok=True)

    for index, artifact in enumerate(artifacts, start=1):
        artifact_path = artifact_dir / f"artifact-{index}.yaml"
        artifact_path.write_text(
            yaml.safe_dump(artifact, allow_unicode=False, sort_keys=False),
            encoding="utf-8",
        )


@pytest.fixture
def live_smoke_runner(
    live_smoke_env: dict[str, str],
    live_smoke_paths: LiveSmokePaths,
    live_smoke_artifacts: list[dict[str, Any]],
):
    def _run(*args: str, timeout: float = 30.0) -> LiveSmokeCommandResult:
        argv = [sys.executable, "-m", "aish", *args]
        start = time.monotonic()
        completed = subprocess.run(
            argv,
            capture_output=True,
            cwd=live_smoke_paths.workspace,
            env=live_smoke_env,
            text=True,
            timeout=timeout,
        )
        duration_seconds = time.monotonic() - start
        result = LiveSmokeCommandResult(
            argv=argv,
            cwd=str(live_smoke_paths.workspace),
            returncode=completed.returncode,
            stdout=completed.stdout,
            stderr=completed.stderr,
            duration_seconds=duration_seconds,
        )
        live_smoke_artifacts.append(
            {
                "type": "command",
                "argv": argv,
                "cwd": str(live_smoke_paths.workspace),
                "duration_seconds": duration_seconds,
                "env_summary": _env_summary(live_smoke_env),
                "result": asdict(result),
            }
        )
        return result

    return _run


@pytest.fixture
def live_smoke_chat_runner(
    live_smoke_config_file: Path,
    live_smoke_env: dict[str, str],
    live_smoke_paths: LiveSmokePaths,
    live_smoke_artifacts: list[dict[str, Any]],
):
    pexpect = pytest.importorskip("pexpect")

    def _run(
        *,
        prompt: str,
        expected_token: str | None = None,
        timeout: float = 90.0,
        auto_approve: bool = False,
        expected_file: Path | None = None,
    ) -> LiveSmokeChatResult:
        argv = [
            sys.executable,
            "-m",
            "aish",
            "run",
            "--config",
            str(live_smoke_config_file),
        ]
        transcript = io.StringIO()
        start = time.monotonic()
        child = pexpect.spawn(
            sys.executable,
            argv[1:],
            cwd=str(live_smoke_paths.workspace),
            env=live_smoke_env,
            encoding="utf-8",
            timeout=timeout,
        )
        child.logfile = transcript
        expected_token_seen = False
        failure_message: str | None = None
        prompt_text = _workspace_prompt_text(live_smoke_paths.workspace)
        result: LiveSmokeChatResult | None = None

        try:
            _expect_shell_prompt(child)
            child.sendline(f";{prompt}")

            deadline = time.monotonic() + timeout
            approvals_sent = 0
            prompt_echo_len = len(_strip_terminal_control(transcript.getvalue()))

            while True:
                remaining = max(deadline - time.monotonic(), 0.1)
                match_index = child.expect(
                    [pexpect.EOF, pexpect.TIMEOUT],
                    timeout=min(remaining, 0.5),
                )
                eof_reached = match_index == 0

                normalized = _strip_terminal_control(transcript.getvalue())
                post_submit = normalized[prompt_echo_len:]

                if expected_token and expected_token in post_submit:
                    expected_token_seen = True

                confirmation_count = sum(
                    post_submit.count(prompt_text)
                    for prompt_text in _CONFIRMATION_PROMPTS
                )
                if confirmation_count > approvals_sent:
                    if not auto_approve:
                        raise AssertionError(
                            "encountered command confirmation prompt but auto_approve is disabled"
                        )
                    child.sendline("y")
                    approvals_sent = confirmation_count
                    continue

                token_ready = expected_token is None or expected_token_seen
                file_ready = expected_file is None or expected_file.exists()
                if token_ready and file_ready:
                    break

                prompt_returned = _transcript_ends_with_prompt(post_submit, prompt_text)
                if prompt_returned or eof_reached:
                    raise AssertionError(
                        "shell returned to prompt before expected task outcome was observed"
                    )

                if time.monotonic() >= deadline:
                    raise pexpect.TIMEOUT("Timeout exceeded.")

        except BaseException as exc:
            failure_message = f"{type(exc).__name__}: {exc}"
            raise
        finally:
            if child.isalive():
                try:
                    _close_shell(child)
                except Exception:
                    child.close(force=True)
            child.close(force=True)

            duration_seconds = time.monotonic() - start
            result = LiveSmokeChatResult(
                argv=argv,
                cwd=str(live_smoke_paths.workspace),
                transcript=transcript.getvalue(),
                exitstatus=child.exitstatus,
                signalstatus=child.signalstatus,
                duration_seconds=duration_seconds,
                expected_token_seen=expected_token_seen,
            )
            live_smoke_artifacts.append(
                {
                    "type": "chat",
                    "argv": argv,
                    "cwd": str(live_smoke_paths.workspace),
                    "duration_seconds": duration_seconds,
                    "env_summary": _env_summary(live_smoke_env),
                    "config_file": str(live_smoke_config_file),
                    "transcript": result.transcript,
                    "exitstatus": result.exitstatus,
                    "signalstatus": result.signalstatus,
                    "expected_token_seen": result.expected_token_seen,
                    "failure_message": failure_message,
                }
            )

        assert result is not None
        return result

    return _run