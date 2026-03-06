import termios

from aish.shell import _build_passthrough_stdin_termios
from aish.tools.bash_executor import UnifiedBashExecutor


def _opt(name: str) -> int:
    return getattr(termios, name, 0)


def test_build_passthrough_stdin_termios_disables_translations():
    cc_len = max(termios.VMIN, termios.VTIME) + 8
    settings = [0, 0, 0, 0, 0, 0, [0] * cc_len]
    settings[0] |= (
        termios.BRKINT
        | termios.ICRNL
        | termios.INPCK
        | termios.ISTRIP
        | termios.IXON
        | _opt("INLCR")
        | _opt("IGNCR")
    )
    settings[1] |= termios.OPOST
    settings[3] |= termios.ECHO | termios.ICANON | termios.IEXTEN | termios.ISIG

    new_settings = _build_passthrough_stdin_termios(settings)

    assert (new_settings[0] & termios.ICRNL) == 0
    assert (new_settings[0] & _opt("INLCR")) == 0
    assert (new_settings[0] & _opt("IGNCR")) == 0
    assert (new_settings[1] & termios.OPOST) == 0
    assert (
        new_settings[3]
        & (termios.ECHO | termios.ICANON | termios.IEXTEN | termios.ISIG)
    ) == 0
    assert new_settings[6][termios.VMIN] == 1
    assert new_settings[6][termios.VTIME] == 0

    # Original settings must stay untouched.
    assert (settings[0] & termios.ICRNL) != 0
    assert (settings[1] & termios.OPOST) != 0
    assert settings[6][termios.VMIN] == 0


def test_bash_executor_passthrough_termios_matches_shell_policy():
    cc_len = max(termios.VMIN, termios.VTIME) + 8
    settings = [0, 0, 0, 0, 0, 0, [0] * cc_len]
    settings[0] |= termios.ICRNL | _opt("INLCR") | _opt("IGNCR")
    settings[1] |= termios.OPOST
    settings[3] |= termios.ICANON

    new_settings = UnifiedBashExecutor._build_passthrough_stdin_termios(settings)

    assert (new_settings[0] & termios.ICRNL) == 0
    assert (new_settings[0] & _opt("INLCR")) == 0
    assert (new_settings[0] & _opt("IGNCR")) == 0
    assert (new_settings[1] & termios.OPOST) == 0
    assert (new_settings[3] & termios.ICANON) == 0
