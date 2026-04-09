"""History management for AI Shell - SQLite-backed command history with WAL mode."""

from __future__ import annotations

import datetime as dt
import sqlite3
from dataclasses import dataclass
from pathlib import Path
from typing import Any, List, Optional

import anyio


@dataclass(slots=True)
class HistoryEntry:
    """Represents a single history entry"""

    command: str
    timestamp: dt.datetime
    source: str  # "user" or "ai"
    session_uuid: str
    returncode: Optional[int] = None
    stdout: Optional[str] = None
    stderr: Optional[str] = None

    def to_display_string(self) -> str:
        """Convert to display string for history command"""
        time_str = self.timestamp.strftime("%Y-%m-%d %H:%M:%S")
        source_indicator = "🤖" if self.source == "ai" else "👤"
        return f"{time_str} {source_indicator} {self.command}"


class HistoryManager:
    """
    Manages command execution history using SQLite with WAL mode.

    Features:
    - WAL (Write-Ahead Logging) mode for concurrent read/write access
    - Session-based history organization
    - Cross-session history queries
    - ACID guarantees for data integrity
    - Multi-process safe writes
    """

    def __init__(self, db_path: Path, session_uuid: str):
        """
        Initialize history manager.

        Args:
            db_path: Path to SQLite database file
            session_uuid: Current session UUID (must be provided)
        """
        self.db_path = Path(db_path).expanduser()
        if not session_uuid:
            raise ValueError("session_uuid must be provided for HistoryManager")
        self.session_uuid = session_uuid
        self.db_path.parent.mkdir(parents=True, exist_ok=True)
        self._conn = sqlite3.connect(str(self.db_path), check_same_thread=False)
        self._init_db()

    def close(self) -> None:
        """Close database connection."""
        self._conn.close()

    def _init_db(self) -> None:
        """Initialize database schema and enable WAL mode."""
        # Enable WAL mode for concurrent access
        self._conn.execute("PRAGMA journal_mode=WAL;")
        self._conn.execute("PRAGMA synchronous=NORMAL;")
        self._conn.execute("PRAGMA busy_timeout=5000;")  # 5 second timeout

        # Create table with auto-increment id
        self._conn.execute(
            """
            CREATE TABLE IF NOT EXISTS command_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_uuid TEXT NOT NULL,
                command TEXT NOT NULL,
                timestamp TIMESTAMP NOT NULL,
                source TEXT,
                returncode INTEGER,
                stdout TEXT,
                stderr TEXT
            );
            """
        )

        # Create indexes for efficient queries
        self._conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_history_session_time "
            "ON command_history(session_uuid, timestamp DESC);"
        )
        self._conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_history_timestamp "
            "ON command_history(timestamp DESC);"
        )

        self._conn.commit()

    @staticmethod
    def _parse_timestamp(value: Any) -> dt.datetime:
        """Parse timestamp from database value."""
        if isinstance(value, dt.datetime):
            return value
        if isinstance(value, str):
            try:
                return dt.datetime.fromisoformat(value)
            except ValueError:
                pass
        return dt.datetime.now()

    @staticmethod
    def _row_to_entry(row: tuple) -> HistoryEntry:
        """Convert database row to HistoryEntry."""
        return HistoryEntry(
            command=row[0],
            timestamp=HistoryManager._parse_timestamp(row[1]),
            source=row[2] or "user",
            session_uuid=row[3],
            returncode=row[4],
            stdout=row[5],
            stderr=row[6],
        )

    async def add_entry(
        self,
        command: str,
        source: str = "user",
        returncode: Optional[int] = None,
        stdout: Optional[str] = None,
        stderr: Optional[str] = None,
    ) -> bool:
        """
        Add a new history entry.

        Thread-safe: SQLite with WAL mode handles concurrent writes.
        """
        try:
            await anyio.to_thread.run_sync(
                self._add_entry_sync,
                command,
                source,
                returncode,
                stdout,
                stderr,
            )
            return True
        except Exception as e:
            # Log error for debugging (minimal output)
            import sys

            print(f"[HistoryManager] Failed to add entry: {e}", file=sys.stderr)
            return False

    def _add_entry_sync(
        self,
        command: str,
        source: str,
        returncode: Optional[int],
        stdout: Optional[str],
        stderr: Optional[str],
    ) -> None:
        """Synchronous add entry for use with anyio.to_thread."""
        self._conn.execute(
            """
            INSERT INTO command_history
            (session_uuid, command, timestamp, source, returncode, stdout, stderr)
            VALUES (?, ?, ?, ?, ?, ?, ?)
            """,
            (
                self.session_uuid,
                command,
                dt.datetime.now().isoformat(),
                source,
                returncode,
                stdout,
                stderr,
            ),
        )
        self._conn.commit()

    async def get_history(
        self,
        limit: Optional[int] = None,
        session_uuid: Optional[str] = None,
    ) -> List[HistoryEntry]:
        """
        Get command history.

        Args:
            limit: Maximum number of entries to return
            session_uuid: Filter by session (None = all sessions)

        Returns:
            List of history entries, most recent first
        """
        try:
            return await anyio.to_thread.run_sync(
                self._get_history_sync,
                limit,
                session_uuid,
            )
        except Exception:
            return []

    def _get_history_sync(
        self,
        limit: Optional[int],
        session_uuid: Optional[str],
    ) -> List[HistoryEntry]:
        """Synchronous get history for use with anyio.to_thread."""
        params: List[Any] = []
        if session_uuid:
            query = """
                SELECT command, timestamp, source, session_uuid, returncode, stdout, stderr
                FROM command_history
                WHERE session_uuid = ?
                ORDER BY timestamp DESC
            """
            params = [session_uuid]
        else:
            query = """
                SELECT command, timestamp, source, session_uuid, returncode, stdout, stderr
                FROM command_history
                ORDER BY timestamp DESC
            """

        if limit is not None and limit > 0:
            query += " LIMIT ?"
            params.append(limit)

        cursor = self._conn.execute(query, tuple(params) if params else ())
        rows = cursor.fetchall()
        return [self._row_to_entry(row) for row in rows]

    async def get_sessions(self) -> List[str]:
        """Get list of session UUIDs with history."""
        try:
            return await anyio.to_thread.run_sync(self._get_sessions_sync)
        except Exception:
            return []

    def _get_sessions_sync(self) -> List[str]:
        """Synchronous get sessions for use with anyio.to_thread."""
        cursor = self._conn.execute(
            """
            SELECT DISTINCT session_uuid
            FROM command_history
            ORDER BY session_uuid DESC
            """
        )
        return [row[0] for row in cursor.fetchall()]

    async def delete_entry(self, history_id: int) -> bool:
        """Delete a specific history entry by database ID."""
        try:
            await anyio.to_thread.run_sync(self._delete_entry_sync, history_id)
            return True
        except Exception:
            return False

    def _delete_entry_sync(self, history_id: int) -> None:
        """Synchronous delete entry for use with anyio.to_thread."""
        self._conn.execute("DELETE FROM command_history WHERE id = ?", (history_id,))
        self._conn.commit()

    async def delete_session(self, session_uuid: str) -> bool:
        """Delete all history entries for a session."""
        try:
            await anyio.to_thread.run_sync(self._delete_session_sync, session_uuid)
            return True
        except Exception:
            return False

    def _delete_session_sync(self, session_uuid: str) -> None:
        """Synchronous delete session for use with anyio.to_thread."""
        self._conn.execute(
            "DELETE FROM command_history WHERE session_uuid = ?",
            (session_uuid,),
        )
        self._conn.commit()

    async def delete_entry_by_index(
        self, index: int, session_uuid: Optional[str] = None
    ) -> bool:
        """
        Delete a history entry by its display index within a session.

        Args:
            index: 1-based display index (1 = oldest entry in the session)
            session_uuid: Session UUID (defaults to current session)

        Returns:
            True if deletion succeeded, False otherwise
        """
        try:
            session_id = session_uuid or self.session_uuid
            return await anyio.to_thread.run_sync(
                self._delete_entry_by_index_sync,
                index,
                session_id,
            )
        except Exception:
            return False

    def _delete_entry_by_index_sync(self, index: int, session_uuid: str) -> bool:
        """Synchronous delete by index for use with anyio.to_thread."""
        # Get the entry at the specified index using rowid
        # Use ORDER BY id ASC to get consistent ordering (id is auto-increment, correlates with time)
        cursor = self._conn.execute(
            """
            SELECT id, command, timestamp
            FROM command_history
            WHERE session_uuid = ?
            ORDER BY id ASC
            LIMIT 1 OFFSET ?
            """,
            (session_uuid, index - 1),
        )
        row = cursor.fetchone()

        if row:
            db_id = row[0]
            command = row[1]

            # Delete and verify
            self._conn.execute("DELETE FROM command_history WHERE id = ?", (db_id,))
            self._conn.commit()

            # Verify deletion
            verify = self._conn.execute(
                "SELECT COUNT(*) FROM command_history WHERE id = ?", (db_id,)
            ).fetchone()[0]

            import sys

            if verify == 0:
                print(
                    f"[HistoryManager] Deleted: id={db_id}, cmd={command[:30]}",
                    file=sys.stderr,
                )
                return True
            else:
                print(
                    f"[HistoryManager] ERROR: Deletion failed for id={db_id}",
                    file=sys.stderr,
                )
                return False

        import sys

        print(f"[HistoryManager] Entry {index} not found", file=sys.stderr)
        return False

    async def clear_history(self) -> bool:
        """Clear all history."""
        try:
            await anyio.to_thread.run_sync(self._clear_history_sync)
            return True
        except Exception:
            return False

    def _clear_history_sync(self) -> None:
        """Synchronous clear history for use with anyio.to_thread."""
        self._conn.execute("DELETE FROM command_history")
        self._conn.commit()

    async def check_consecutive_failures(self, count: int = 2) -> bool:
        """
        Check if the last 'count' consecutive commands in current session failed.

        Args:
            count: Number of consecutive failures to check

        Returns:
            True if last 'count' commands all failed (returncode != 0)
        """
        try:
            return await anyio.to_thread.run_sync(
                self._check_consecutive_failures_sync,
                count,
            )
        except Exception:
            return False

    def _check_consecutive_failures_sync(self, count: int) -> bool:
        """Synchronous check consecutive failures for use with anyio.to_thread."""
        cursor = self._conn.execute(
            """
            SELECT returncode
            FROM command_history
            WHERE session_uuid = ?
            ORDER BY timestamp DESC
            LIMIT ?
            """,
            (self.session_uuid, count),
        )
        rows = cursor.fetchall()

        if len(rows) < count:
            return False

        for row in rows:
            returncode = row[0]
            if returncode is None or returncode == 0:
                return False

        return True

    def get_db_path(self) -> str:
        """Get the database file path."""
        return str(self.db_path)

    def get_session_uuid(self) -> str:
        """Get current session UUID."""
        return self.session_uuid

    def search_prefix_sync(
        self,
        prefix: str,
        session_uuid: Optional[str] = None,
        source: Optional[str] = "user",
    ) -> Optional[str]:
        """Return the most recent command starting with prefix."""
        prefix = str(prefix or "")
        if not prefix:
            return None

        params: list[Any] = [f"{prefix}%"]
        where = "WHERE command LIKE ?"
        if session_uuid:
            where += " AND session_uuid = ?"
            params.append(session_uuid)
        if source is not None:
            where += " AND COALESCE(source, 'user') = ?"
            params.append(source)

        cursor = self._conn.execute(
            f"""
            SELECT command
            FROM command_history
            {where}
            ORDER BY id DESC
            LIMIT 1
            """,
            tuple(params),
        )
        row = cursor.fetchone()
        if row is None:
            return None
        return str(row[0])

    def get_recent_commands_sync(
        self,
        limit: int = 200,
        session_uuid: Optional[str] = None,
        source: Optional[str] = "user",
    ) -> list[str]:
        """Return recent commands in chronological order for prompt history."""
        if limit <= 0:
            return []

        params: list[Any] = []
        where = ""
        if session_uuid:
            where = "WHERE session_uuid = ?"
            params.append(session_uuid)
        if source is not None:
            where += f"{' AND' if where else 'WHERE'} COALESCE(source, 'user') = ?"
            params.append(source)

        cursor = self._conn.execute(
            f"""
            SELECT command
            FROM command_history
            {where}
            ORDER BY id DESC
            LIMIT ?
            """,
            tuple([*params, limit]),
        )
        rows = cursor.fetchall()

        ordered: list[str] = []
        seen: set[str] = set()
        for row in reversed(rows):
            command = str(row[0] or "").strip()
            if not command or command in seen:
                continue
            seen.add(command)
            ordered.append(command)
        return ordered
