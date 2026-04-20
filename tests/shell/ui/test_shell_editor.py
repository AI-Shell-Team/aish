from __future__ import annotations

from types import SimpleNamespace
from unittest.mock import Mock

from prompt_toolkit.completion import CompleteEvent
from prompt_toolkit.document import Document
from prompt_toolkit.history import FileHistory

from aish.shell.ui.completion import ShellCompleter
from aish.shell.ui.editor import ShellPromptController


def test_shell_prompt_controller_uses_file_history_and_remembers_commands():
    history_manager = Mock()
    controller = ShellPromptController(history_manager=history_manager)

    controller.remember_command("git status")

    assert isinstance(controller._history, FileHistory)
    history_manager.get_recent_commands_sync.assert_not_called()


def test_shell_prompt_controller_uses_cwd_provider_for_prompt_text():
    controller = ShellPromptController(cwd_provider=lambda: "/tmp/project")

    assert controller._get_prompt_text() == "/tmp/project"


def test_shell_prompt_controller_forwards_custom_prompt_message():
    controller = ShellPromptController()
    controller._session.prompt = Mock(return_value="echo hi")

    result = controller.prompt("... ")

    assert result == "echo hi"
    controller._session.prompt.assert_called_once()
    assert controller._session.prompt.call_args.args[0] == "... "
    assert "bottom_toolbar" not in controller._session.prompt.call_args.kwargs
    assert "style" not in controller._session.prompt.call_args.kwargs


def test_shell_prompt_controller_render_theme_preserves_trailing_space(monkeypatch):
    controller = ShellPromptController(prompt_theme="minimal")

    monkeypatch.setattr(controller, "_find_theme_script", lambda _theme: "/tmp/theme.aish")
    monkeypatch.setattr(
        "aish.shell.ui.editor.subprocess.run",
        lambda *args, **kwargs: SimpleNamespace(
            returncode=0,
            stdout="\x1b[32m❯\x1b[0m \n",
        ),
    )

    assert controller._render_theme() == "\x1b[32m❯\x1b[0m "


def test_shell_prompt_controller_render_theme_preserves_multiline_prompt_suffix(monkeypatch):
    controller = ShellPromptController(prompt_theme="developer")

    monkeypatch.setattr(controller, "_find_theme_script", lambda _theme: "/tmp/theme.aish")
    monkeypatch.setattr(
        "aish.shell.ui.editor.subprocess.run",
        lambda *args, **kwargs: SimpleNamespace(
            returncode=0,
            stdout="line1\n\x1b[32m❯\x1b[0m \n",
        ),
    )

    assert controller._render_theme() == "line1\n\x1b[32m❯\x1b[0m "


def test_shell_completer_suggests_builtin_and_special_commands():
    completer = ShellCompleter(command_provider=lambda: ["ls", "logout", "pwd"])

    completions = list(
        completer.get_completions(Document(text="/m", cursor_position=2), CompleteEvent(completion_requested=True))
    )
    assert [item.text for item in completions] == ["/model"]

    completions = list(
        completer.get_completions(Document(text="pw", cursor_position=2), CompleteEvent(completion_requested=True))
    )
    assert [item.text for item in completions] == ["pwd"]


def test_shell_completer_completes_directories_for_cd(tmp_path):
    target_dir = tmp_path / "project"
    target_dir.mkdir()
    (tmp_path / "plain.txt").write_text("data", encoding="utf-8")

    completer = ShellCompleter(
        cwd_provider=lambda: str(tmp_path),
        command_provider=lambda: ["cd"],
    )

    completions = list(
        completer.get_completions(
            Document(text="cd pr", cursor_position=5),
            CompleteEvent(completion_requested=True),
        )
    )

    assert [item.display_text for item in completions] == ["project/"]


def test_shell_completer_completes_path_like_first_token(tmp_path):
    scripts_dir = tmp_path / "scripts"
    scripts_dir.mkdir()

    completer = ShellCompleter(
        cwd_provider=lambda: str(tmp_path),
        command_provider=lambda: ["ls", "pwd"],
    )

    completions = list(
        completer.get_completions(
            Document(text="./scr", cursor_position=5),
            CompleteEvent(completion_requested=True),
        )
    )

    assert [item.display_text for item in completions] == ["scripts/"]


def test_shell_completer_skips_ai_prefixed_input():
    completer = ShellCompleter(command_provider=lambda: ["pwd"])

    completions = list(
        completer.get_completions(
            Document(text=";pw", cursor_position=3),
            CompleteEvent(completion_requested=True),
        )
    )

    assert completions == []