"""Tests for builtin command classification."""

from aish.shell.commands import BuiltinRegistry


def test_quit_is_rejected_as_shell_exit_command():
    assert BuiltinRegistry.is_rejected_command("quit") is True


def test_quit_rejected_message_mentions_original_command():
    message = BuiltinRegistry.get_rejected_command_message("quit")

    assert message is not None
    assert "'quit'" in message


def test_logout_is_not_rejected_as_shell_exit_command():
    assert BuiltinRegistry.is_rejected_command("logout") is False