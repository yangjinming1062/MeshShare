from __future__ import annotations

import hashlib
import os
import re
import secrets
import socket
import uuid
from datetime import datetime, timezone
from pathlib import Path
from typing import BinaryIO

from .events import Event, EventHub
from .store import Store


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="milliseconds")


class ServiceError(Exception):
    def __init__(self, code: str, message: str, status: int = 400) -> None:
        super().__init__(message)
        self.code, self.message, self.status = code, message, status


class ShareService:
    def __init__(self, data_dir: Path, *, max_file_bytes: int = 100 * 1024 * 1024, max_message_chars: int = 10_000) -> None:
        self.data_dir = data_dir
        self.files_dir = data_dir / "files"
        self.tmp_dir = data_dir / "tmp"
        self.files_dir.mkdir(parents=True, exist_ok=True)
        self.tmp_dir.mkdir(parents=True, exist_ok=True)
        self.store = Store(data_dir / "meshshare.sqlite3")
        self.events = EventHub()
        self.max_file_bytes = max_file_bytes
        self.max_message_chars = max_message_chars
        self.node_id = self._load_node_id()
        self.node_name = socket.gethostname() or "MeshShare"

    def _load_node_id(self) -> str:
        path = self.data_dir / "node-id"
        try:
            value = path.read_text(encoding="ascii").strip()
            if re.fullmatch(r"[0-9a-f-]{36}", value):
                return value
        except OSError:
            pass
        value = str(uuid.uuid4())
        path.write_text(value + "\n", encoding="ascii")
        try:
            os.chmod(path, 0o600)
        except OSError:
            pass
        return value

    def _publish(self, revision: int, resource_type: str, resource_id: str, action: str) -> None:
        self.events.publish(Event(revision, resource_type, resource_id, action))

    def state(self) -> dict[str, object]:
        return self.store.state()

    def changes_since(self, revision: int) -> list[dict[str, object]]:
        return self.store.changes_since(revision)

    def add_message(self, text: str, author: str = "匿名设备") -> dict[str, object]:
        if not isinstance(text, str) or not text.strip():
            raise ServiceError("INVALID_MESSAGE", "消息不能为空")
        if len(text) > self.max_message_chars:
            raise ServiceError("MESSAGE_TOO_LARGE", f"消息不能超过 {self.max_message_chars} 个字符", 413)
        author = author.strip() if isinstance(author, str) else "匿名设备"
        author = author[:80] or "匿名设备"
        item = {"id": str(uuid.uuid4()), "text": text, "author": author, "created_at": utc_now()}
        revision = self.store.add_message(item, utc_now())
        self._publish(revision, "message", str(item["id"]), "upsert")
        return item

    def delete_message(self, message_id: str) -> None:
        revision = self.store.delete_message(message_id, utc_now())
        if revision is None:
            raise ServiceError("MESSAGE_NOT_FOUND", "消息不存在", 404)
        self._publish(revision, "message", message_id, "delete")

    def set_clipboard(self, text: str, source: str = "匿名设备") -> dict[str, str]:
        if not isinstance(text, str):
            raise ServiceError("INVALID_CLIPBOARD", "剪贴板内容必须是文本")
        if len(text) > self.max_message_chars:
            raise ServiceError("CLIPBOARD_TOO_LARGE", f"剪贴板不能超过 {self.max_message_chars} 个字符", 413)
        if not isinstance(source, str):
            source = "匿名设备"
        item = {"text": text, "source": (source.strip() or "匿名设备")[:80], "updated_at": utc_now()}
        revision = self.store.set_clipboard(item, utc_now())
        self._publish(revision, "clipboard", "1", "upsert")
        return item

    def save_upload(self, stream: BinaryIO, name: str, media_type: str | None, content_length: int | None) -> dict[str, object]:
        safe_name = Path(name or "upload.bin").name.strip() or "upload.bin"
        safe_name = safe_name[:255]
        if content_length is not None and content_length > self.max_file_bytes:
            raise ServiceError("FILE_TOO_LARGE", f"文件不能超过 {self.max_file_bytes} 字节", 413)
        temporary = self.tmp_dir / f".{secrets.token_hex(16)}.upload"
        total = 0
        digest = hashlib.sha256()
        try:
            with temporary.open("wb") as target:
                while True:
                    chunk = stream.read(min(1024 * 1024, self.max_file_bytes + 1 - total))
                    if not chunk:
                        break
                    total += len(chunk)
                    if total > self.max_file_bytes:
                        raise ServiceError("FILE_TOO_LARGE", f"文件不能超过 {self.max_file_bytes} 字节", 413)
                    target.write(chunk)
                    digest.update(chunk)
            file_id = str(uuid.uuid4())
            destination = self.files_dir / file_id
            os.replace(temporary, destination)
            candidate_type = media_type.split(";", 1)[0].strip() if isinstance(media_type, str) else ""
            if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9!#$&^_.+-]*/[A-Za-z0-9][A-Za-z0-9!#$&^_.+-]*", candidate_type):
                candidate_type = "application/octet-stream"
            item = {"id": file_id, "name": safe_name, "media_type": candidate_type, "size": total, "sha256": digest.hexdigest(), "created_at": utc_now()}
            try:
                revision = self.store.add_file(item, utc_now())
            except BaseException:
                destination.unlink(missing_ok=True)
                raise
        except ServiceError:
            temporary.unlink(missing_ok=True)
            raise
        except OSError as exc:
            temporary.unlink(missing_ok=True)
            raise ServiceError("FILE_WRITE_FAILED", f"无法保存文件：{exc}", 507) from exc
        self._publish(revision, "file", file_id, "upsert")
        return item

    def file_path(self, file_id: str) -> tuple[dict[str, object], Path]:
        item = self.store.get_file(file_id)
        path = self.files_dir / file_id
        if item is None or not path.is_file():
            raise ServiceError("FILE_NOT_FOUND", "文件不存在", 404)
        return item, path

    def delete_file(self, file_id: str) -> None:
        _, path = self.file_path(file_id)
        revision = self.store.delete_file(file_id, utc_now())
        if revision is None:
            raise ServiceError("FILE_NOT_FOUND", "文件不存在", 404)
        path.unlink(missing_ok=True)
        self._publish(revision, "file", file_id, "delete")
