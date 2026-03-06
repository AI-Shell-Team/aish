from __future__ import annotations

from aish.shell import (AIShell, CommandResult, CommandStatus,
                        ModeAwareCompleter, QuotedPathCompleter,
                        make_shell_completer)


def test_shell_exports_stable_symbols():
    assert AIShell is not None
    assert QuotedPathCompleter is not None
    assert ModeAwareCompleter is not None
    assert callable(make_shell_completer)
    assert CommandStatus.SUCCESS.value == "success"

    result = CommandResult(
        status=CommandStatus.SUCCESS, exit_code=0, stdout="", stderr=""
    )
    assert result.to_tuple() == (0, "", "")
