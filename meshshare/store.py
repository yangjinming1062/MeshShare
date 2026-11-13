from __future__ import annotations

import sqlite3
import threading
from collections.abc import Iterator
from contextlib import contextmanager
from pathlib import Path
from typing import Any


SCHEMA = """
CREATE TABLE IF NOT EXISTS messages (
  id TEXT PRIMARY KEY,
  text TEXT NOT NULL,
  author TEXT NOT NULL,
  created_at TEXT NOT NULL,
  deleted_at TEXT
);
CREATE INDEX IF NOT EXISTS messages_created_idx ON messages(created_at DESC);
CREATE TABLE IF NOT EXISTS clipboard (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  text TEXT NOT NULL,
  source TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS files (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  media_type TEXT NOT NULL,
  size INTEGER NOT NULL,
  sha256 TEXT NOT NULL,
  created_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS changes (
  revision INTEGER PRIMARY KEY AUTOINCREMENT,
  resource_type TEXT NOT NULL,
  resource_id TEXT NOT NULL,
  action TEXT NOT NULL,
  created_at TEXT NOT NULL
);
"""


class Store:
    """Small SQLite repository; each operation uses a short-lived connection."""

    def __init__(self, path: Path) -> None:
        self.path = path
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self._lock = threading.RLock()
        with self._connect() as connection:
            connection.executescript(SCHEMA)

    @contextmanager
    def _connect(self) -> Iterator[sqlite3.Connection]:
        connection = sqlite3.connect(self.path, timeout=10)
        connection.row_factory = sqlite3.Row
        try:
            yield connection
            connection.commit()
        except BaseException:
            connection.rollback()
            raise
        finally:
            connection.close()

    def _change(self, connection: sqlite3.Connection, resource_type: str, resource_id: str, action: str, now: str) -> int:
        cursor = connection.execute(
            "INSERT INTO changes(resource_type, resource_id, action, created_at) VALUES (?, ?, ?, ?)",
            (resource_type, resource_id, action, now),
        )
        return int(cursor.lastrowid)

    def add_message(self, item: dict[str, Any], now: str) -> int:
        with self._lock, self._connect() as db:
            db.execute(
                "INSERT INTO messages(id, text, author, created_at) VALUES (?, ?, ?, ?)",
                (item["id"], item["text"], item["author"], item["created_at"]),
            )
            return self._change(db, "message", item["id"], "upsert", now)

    def delete_message(self, message_id: str, now: str) -> int | None:
        with self._lock, self._connect() as db:
            cursor = db.execute("DELETE FROM messages WHERE id = ?", (message_id,))
            if not cursor.rowcount:
                return None
            return self._change(db, "message", message_id, "delete", now)

    def set_clipboard(self, item: dict[str, str], now: str) -> int:
        with self._lock, self._connect() as db:
            db.execute(
                "INSERT INTO clipboard(id, text, source, updated_at) VALUES (1, ?, ?, ?) "
                "ON CONFLICT(id) DO UPDATE SET text=excluded.text, source=excluded.source, updated_at=excluded.updated_at",
                (item["text"], item["source"], item["updated_at"]),
            )
            return self._change(db, "clipboard", "1", "upsert", now)

    def add_file(self, item: dict[str, Any], now: str) -> int:
        with self._lock, self._connect() as db:
            db.execute(
                "INSERT INTO files(id, name, media_type, size, sha256, created_at) VALUES (?, ?, ?, ?, ?, ?)",
                (item["id"], item["name"], item["media_type"], item["size"], item["sha256"], item["created_at"]),
            )
            return self._change(db, "file", item["id"], "upsert", now)

    def delete_file(self, file_id: str, now: str) -> int | None:
        with self._lock, self._connect() as db:
            cursor = db.execute("DELETE FROM files WHERE id = ?", (file_id,))
            if not cursor.rowcount:
                return None
            return self._change(db, "file", file_id, "delete", now)

    def state(self, message_limit: int = 500) -> dict[str, Any]:
        with self._connect() as db:
            messages = [dict(row) for row in db.execute(
                "SELECT id, text, author, created_at FROM messages ORDER BY created_at DESC LIMIT ?", (message_limit,)
            )]
            clipboard = db.execute("SELECT text, source, updated_at FROM clipboard WHERE id = 1").fetchone()
            files = [dict(row) for row in db.execute(
                "SELECT id, name, media_type, size, sha256, created_at FROM files ORDER BY created_at DESC"
            )]
            latest = db.execute("SELECT COALESCE(MAX(revision), 0) AS revision FROM changes").fetchone()
        return {"revision": int(latest["revision"]), "messages": messages, "clipboard": dict(clipboard) if clipboard else None, "files": files}

    def changes_since(self, revision: int, limit: int = 1000) -> list[dict[str, Any]]:
        with self._connect() as db:
            return [dict(row) for row in db.execute(
                "SELECT revision, resource_type, resource_id, action, created_at FROM changes WHERE revision > ? ORDER BY revision LIMIT ?",
                (revision, limit),
            )]

    def get_file(self, file_id: str) -> dict[str, Any] | None:
        with self._connect() as db:
            row = db.execute("SELECT id, name, media_type, size, sha256, created_at FROM files WHERE id = ?", (file_id,)).fetchone()
        return dict(row) if row else None
