import signal
from unittest.mock import MagicMock, patch

from prompt_toolkit.completion import Completer, Completion

from aish.config import ConfigModel
from aish.shell import AIShell, ModeAwareCompleter
from aish.skills import SkillManager


def make_shell(config: ConfigModel) -> AIShell:
    skill_manager = SkillManager()
    skill_manager.load_all_skills()
    return AIShell(config=config, skill_manager=skill_manager)


class _FakeProcess:
    def __init__(self, pid: int, returncode: int | None):
        self.pid = pid
        self._returncode = returncode

    def poll(self):
        return self._returncode


class _StaticCompleter(Completer):
    def __init__(self, values: list[str]):
        self.values = values

    def get_completions(self, document, complete_event):
        for value in self.values:
            yield Completion(
                text=value, start_position=-len(document.text_before_cursor)
            )


def test_sync_pty_resize_no_change_skips_ioctl_and_sigwinch():
    shell = make_shell(ConfigModel(model="test-model", terminal_resize_mode="full"))
    process = _FakeProcess(pid=1234, returncode=None)

    with (
        patch.object(shell, "_read_terminal_size", return_value=(24, 80)),
        patch.object(shell, "_set_pty_winsize", return_value=True) as mock_set,
        patch("aish.shell.os.killpg") as mock_killpg,
    ):
        size = shell._sync_pty_resize(9, process, (24, 80))

    assert size == (24, 80)
    mock_set.assert_not_called()
    mock_killpg.assert_not_called()


def test_sync_pty_resize_size_change_updates_winsize_and_sends_sigwinch():
    shell = make_shell(ConfigModel(model="test-model", terminal_resize_mode="full"))
    process = _FakeProcess(pid=1234, returncode=None)

    with (
        patch.object(shell, "_read_terminal_size", return_value=(32, 120)),
        patch.object(shell, "_set_pty_winsize", return_value=True) as mock_set,
        patch("aish.shell.os.killpg") as mock_killpg,
    ):
        size = shell._sync_pty_resize(9, process, (24, 80))

    assert size == (32, 120)
    mock_set.assert_called_once_with(9, 32, 120)
    mock_killpg.assert_called_once_with(1234, signal.SIGWINCH)


def test_sync_pty_resize_skips_sigwinch_when_process_exited():
    shell = make_shell(ConfigModel(model="test-model", terminal_resize_mode="full"))
    process = _FakeProcess(pid=1234, returncode=0)

    with (
        patch.object(shell, "_read_terminal_size", return_value=(32, 120)),
        patch.object(shell, "_set_pty_winsize", return_value=True) as mock_set,
        patch("aish.shell.os.killpg") as mock_killpg,
    ):
        size = shell._sync_pty_resize(9, process, (24, 80))

    assert size == (32, 120)
    mock_set.assert_called_once_with(9, 32, 120)
    mock_killpg.assert_not_called()


def test_sync_pty_resize_off_mode_skips_all_resize_work():
    shell = make_shell(ConfigModel(model="test-model", terminal_resize_mode="off"))
    process = _FakeProcess(pid=1234, returncode=None)

    with (
        patch.object(shell, "_read_terminal_size") as mock_read,
        patch.object(shell, "_set_pty_winsize") as mock_set,
    ):
        size = shell._sync_pty_resize(9, process, (24, 80))

    assert size == (24, 80)
    mock_read.assert_not_called()
    mock_set.assert_not_called()


def test_sync_pty_resize_pty_only_mode_updates_winsize():
    shell = make_shell(ConfigModel(model="test-model", terminal_resize_mode="pty_only"))
    process = _FakeProcess(pid=1234, returncode=None)

    with (
        patch.object(shell, "_read_terminal_size", return_value=(28, 100)),
        patch.object(shell, "_set_pty_winsize", return_value=True) as mock_set,
    ):
        size = shell._sync_pty_resize(9, process, (24, 80))

    assert size == (28, 100)
    mock_set.assert_called_once_with(9, 28, 100)


def test_sync_pty_resize_failed_winsize_keeps_last_size_for_retry():
    shell = make_shell(ConfigModel(model="test-model", terminal_resize_mode="full"))
    process = _FakeProcess(pid=1234, returncode=None)

    with (
        patch.object(shell, "_read_terminal_size", return_value=(28, 100)),
        patch.object(shell, "_set_pty_winsize", return_value=False) as mock_set,
        patch("aish.shell.os.killpg") as mock_killpg,
    ):
        size = shell._sync_pty_resize(9, process, (24, 80))

    assert size == (24, 80)
    mock_set.assert_called_once_with(9, 28, 100)
    mock_killpg.assert_not_called()


def test_compute_ask_user_max_visible_has_floor_and_upper_bound():
    assert AIShell._compute_ask_user_max_visible(10, 4, False) == 3
    assert AIShell._compute_ask_user_max_visible(5, 40, False) == 5
    assert AIShell._compute_ask_user_max_visible(20, 12, True) >= 3
    assert AIShell._compute_ask_user_max_visible(100, 80, False) == 12


def test_refresh_live_for_resize_skips_ui_updates_when_not_full_mode():
    shell = make_shell(ConfigModel(model="test-model", terminal_resize_mode="pty_only"))
    shell.current_live = MagicMock()
    shell._last_streaming_accumulated = "stream"
    shell._last_reasoning_render_lines = ["line"]

    with (
        patch.object(shell, "_update_reasoning_live") as mock_reasoning,
        patch.object(shell, "_render_streaming_chunk") as mock_streaming,
    ):
        shell._refresh_live_for_resize()

    mock_reasoning.assert_not_called()
    mock_streaming.assert_not_called()


def test_refresh_live_for_resize_updates_streaming_when_full_mode():
    shell = make_shell(ConfigModel(model="test-model", terminal_resize_mode="full"))
    shell.current_live = MagicMock()
    shell._last_streaming_accumulated = "stream"

    with patch.object(shell, "_render_streaming_chunk") as mock_streaming:
        shell._refresh_live_for_resize()

    mock_streaming.assert_called_once_with("stream")


def test_mode_aware_completer_resets_state_when_terminal_width_changes():
    values = [f"x{i}" for i in range(40)]
    completer = ModeAwareCompleter(
        ai_completer=_StaticCompleter([]),
        shell_completer=_StaticCompleter(values),
        ai_prefix_marks={";", "；"},
    )
    completer._completion_state = ("x", 40)
    completer._awaiting_confirmation = True
    completer._completion_cache = ["x1"]
    completer._last_terminal_width = 80

    with patch.object(completer, "_get_terminal_width", return_value=100):
        completer._reset_state_on_terminal_resize()

    assert completer._completion_state is None
    assert completer._awaiting_confirmation is False
    assert completer._completion_cache is None
    assert completer._last_terminal_width == 100
