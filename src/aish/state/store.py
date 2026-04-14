"""aish local session persistence (single-machine).

Requirements:
- Store session records in SQLite.
- Each aish start creates a new session.
- Multi-process concurrent write support via WAL mode.

The stored schema is intentionally small and stable:
- session_uuid: globally unique session id
- created_at: UTC timestamp
- model: model identifier used for this run
- api_base: optional provider base url
- state: JSON blob for lightweight extensibility
"""

from __future__ import annotations

import datetime as dt
import json
import sqlite3
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Dict, Optional, Sequence


@dataclass(slots=True)
class SessionRecord:
    session_uuid: str
    created_at: dt.datetime
    model: str
    api_base: Optional[str]
    run_user: Optional[str]
    state: Dict[str, Any]


class SessionStore:
    """Lightweight SQLite-backed session metadata store with multi-process support."""

    def __init__(self, db_path: Path) -> None:
        self._db_path = db_path.expanduser()
        self._db_path.parent.mkdir(parents=True, exist_ok=True)
        # check_same_thread=False allows sharing connection across threads
        # WAL mode enables multi-process concurrent access
        self._conn = sqlite3.connect(str(self._db_path), check_same_thread=False)
        self._init_schema()

    def close(self) -> None:
        self._conn.close()

    def _init_schema(self) -> None:
        # Enable WAL mode for better concurrency
        self._conn.execute("PRAGMA journal_mode=WAL;")
        self._conn.execute("PRAGMA synchronous=NORMAL;")
        # Set busy timeout to handle concurrent access
        self._conn.execute("PRAGMA busy_timeout=5000;")  # 5 seconds

        self._conn.execute(
            """
            CREATE TABLE IF NOT EXISTS sessions (
                session_uuid TEXT PRIMARY KEY,
                created_at TIMESTAMP NOT NULL,
                model TEXT NOT NULL,
                api_base TEXT,
                run_user TEXT,
                state TEXT
            );
            """
        )

        # Best-effort migration for older databases.
        try:
            self._conn.execute("ALTER TABLE sessions ADD COLUMN run_user TEXT;")
        except Exception:
            pass  # Column already exists

        self._conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_sessions_created_at ON sessions(created_at);"
        )

        self._conn.commit()

    @staticmethod
    def _now() -> dt.datetime:
        return dt.datetime.utcnow()

    @staticmethod
    def _load_state(raw: Any) -> Dict[str, Any]:
        if raw in (None, ""):
            return {}
        if isinstance(raw, dict):
            return dict(raw)
        if isinstance(raw, str):
            try:
                decoded = json.loads(raw)
            except json.JSONDecodeError:
                return {}
            return decoded if isinstance(decoded, dict) else {}
        return {}

    @staticmethod
    def _dump_state(state: Dict[str, Any]) -> str:
        return json.dumps(state or {}, ensure_ascii=False)

    @staticmethod
    def _record_from_row(row: Sequence[Any]) -> SessionRecord:
        session_uuid = str(row[0])
        created_at = row[1]
        model = str(row[2])
        api_base = None if row[3] is None else str(row[3])
        run_user = None if row[4] is None else str(row[4])
        state = SessionStore._load_state(row[5])
        return SessionRecord(
            session_uuid=session_uuid,
            created_at=created_at,
            model=model,
            api_base=api_base,
            run_user=run_user,
            state=state,
        )

    def create_session(
        self,
        *,
        model: str,
        api_base: Optional[str] = None,
        run_user: Optional[str] = None,
        session_uuid: Optional[str] = None,
        state: Optional[Dict[str, Any]] = None,
    ) -> SessionRecord:
        now = self._now()
        session_uuid = session_uuid or str(uuid.uuid4())
        payload: Dict[str, Any] = dict(state or {})
        payload.setdefault("status", "active")

        self._conn.execute(
            """
            INSERT INTO sessions (session_uuid, created_at, model, api_base, run_user, state)
            VALUES (?, ?, ?, ?, ?, ?)
            """,
            (session_uuid, now, model, api_base, run_user, self._dump_state(payload)),
        )
        self._conn.commit()

        return SessionRecord(
            session_uuid=session_uuid,
            created_at=now,
            model=model,
            api_base=api_base,
            run_user=run_user,
            state=payload,
        )

    def get_session(self, session_uuid: str) -> Optional[SessionRecord]:
        row = self._conn.execute(
            """
            SELECT session_uuid, created_at, model, api_base, run_user, state
            FROM sessions
            WHERE session_uuid = ?
            """,
            (session_uuid,),
        ).fetchone()
        return None if row is None else self._record_from_row(row)

    def list_sessions(self, limit: int = 20) -> list[SessionRecord]:
        rows = self._conn.execute(
            """
            SELECT session_uuid, created_at, model, api_base, run_user, state
            FROM sessions
            ORDER BY created_at DESC
            LIMIT ?
            """,
            (limit,),
        ).fetchall()
        return [self._record_from_row(row) for row in rows]

    def update_session_state(
        self,
        session_uuid: str,
        state_patch: Dict[str, Any],
    ) -> Optional[SessionRecord]:
        current = self.get_session(session_uuid)
        if current is None:
            return None

        merged_state = dict(current.state)
        merged_state.update(state_patch)
        self._conn.execute(
            """
            UPDATE sessions
            SET state = ?
            WHERE session_uuid = ?
            """,
            (self._dump_state(merged_state), session_uuid),
        )
        self._conn.commit()
        current.state = merged_state
        return current
