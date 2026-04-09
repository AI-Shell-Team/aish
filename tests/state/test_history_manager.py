from __future__ import annotations

from aish.state import HistoryManager


def test_search_prefix_sync_defaults_to_user_source(tmp_path):
    manager = HistoryManager(tmp_path / "history.db", session_uuid="session-1")
    try:
        manager._add_entry_sync("ls", "user", 0, "", "")
        manager._add_entry_sync("ls --color=auto", "ai", 0, "", "")

        assert manager.search_prefix_sync("ls") == "ls"
        assert manager.search_prefix_sync("ls", source=None) == "ls --color=auto"
    finally:
        manager.close()


def test_get_recent_commands_sync_excludes_ai_by_default(tmp_path):
    manager = HistoryManager(tmp_path / "history.db", session_uuid="session-1")
    try:
        manager._add_entry_sync("pwd", "user", 0, "", "")
        manager._add_entry_sync("ls --color=auto", "ai", 0, "", "")
        manager._conn.execute(
            """
            INSERT INTO command_history
            (session_uuid, command, timestamp, source, returncode, stdout, stderr)
            VALUES (?, ?, ?, ?, ?, ?, ?)
            """,
            ("session-1", "echo legacy", "2026-04-02T00:00:00", None, 0, "", ""),
        )
        manager._conn.commit()

        assert manager.get_recent_commands_sync() == ["pwd", "echo legacy"]
        assert manager.get_recent_commands_sync(source=None) == ["pwd", "ls --color=auto", "echo legacy"]
    finally:
        manager.close()