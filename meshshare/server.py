from __future__ import annotations

import cgi
import json
import mimetypes
import os
import queue
import socket
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlparse

from . import __version__
from .service import ServiceError, ShareService


WEB_DIR = Path(__file__).resolve().parent.parent / "web"


class MeshShareServer(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True

    def __init__(self, address: tuple[str, int], *, data_dir: Path, max_file_bytes: int, max_message_chars: int) -> None:
        self.service = ShareService(data_dir, max_file_bytes=max_file_bytes, max_message_chars=max_message_chars)
        super().__init__(address, MeshShareHandler)


def lan_addresses() -> list[str]:
    addresses: set[str] = set()
    for _, _, _, _, sockaddr in socket.getaddrinfo(socket.gethostname(), None, socket.AF_INET):
        address = sockaddr[0]
        if not address.startswith("127."):
            addresses.add(address)
    return sorted(addresses) or ["127.0.0.1"]


class MeshShareHandler(BaseHTTPRequestHandler):
    server: MeshShareServer
    protocol_version = "HTTP/1.1"

    def handle(self) -> None:
        try:
            super().handle()
        except (BrokenPipeError, ConnectionResetError):
            # Browsers routinely close an SSE connection while reconnecting.
            pass

    def log_message(self, format: str, *args: object) -> None:
        # Never log request bodies, query values, or clipboard/file content.
        print(f"{self.address_string()} - {format % args}")

    @property
    def service(self) -> ShareService:
        return self.server.service

    def _send_json(self, value: object, status: int = 200) -> None:
        body = json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(body)

    def _error(self, error: ServiceError | Exception) -> None:
        if isinstance(error, ServiceError):
            status, code, message = error.status, error.code, error.message
        else:
            status, code, message = 500, "INTERNAL_ERROR", "服务器内部错误"
            print(f"MeshShare error: {error!r}")
        self._send_json({"error": {"code": code, "message": message}}, status)

    def _body_json(self, limit: int = 1024 * 1024) -> dict[str, object]:
        length = self.headers.get("Content-Length")
        if length is None:
            raise ServiceError("LENGTH_REQUIRED", "请求需要 Content-Length", 411)
        try:
            size = int(length)
        except ValueError as exc:
            raise ServiceError("INVALID_LENGTH", "Content-Length 无效") from exc
        if size < 0 or size > limit:
            raise ServiceError("REQUEST_TOO_LARGE", "请求体过大", 413)
        try:
            value = json.loads(self.rfile.read(size))
        except (UnicodeDecodeError, json.JSONDecodeError) as exc:
            raise ServiceError("INVALID_JSON", "请求 JSON 无效") from exc
        if not isinstance(value, dict):
            raise ServiceError("INVALID_JSON", "请求必须是 JSON 对象")
        return value

    def _path(self) -> tuple[str, list[str], dict[str, list[str]]]:
        parsed = urlparse(self.path)
        return parsed.path, [part for part in parsed.path.split("/") if part], parse_qs(parsed.query)

    def do_GET(self) -> None:  # noqa: N802
        try:
            path, parts, query = self._path()
            if path == "/api/health":
                self._send_json({"ok": True, "version": __version__})
            elif path == "/api/info":
                self._send_json({"node_id": self.service.node_id, "node_name": self.service.node_name, "version": __version__, "platform": os.name, "features": ["clipboard", "messages", "files", "sse"], "max_file_bytes": self.service.max_file_bytes, "max_message_chars": self.service.max_message_chars})
            elif path == "/api/state":
                self._send_json(self.service.state())
            elif path == "/api/events":
                self._sse(query)
            elif path == "/api/files" or (len(parts) == 3 and parts[:2] == ["api", "files"]):
                if path == "/api/files":
                    self._send_json({"files": self.service.state()["files"]})
                else:
                    self._download(parts[2])
            elif path == "/" or not path.startswith("/api/"):
                self._static(path)
            else:
                raise ServiceError("NOT_FOUND", "接口不存在", 404)
        except (BrokenPipeError, ConnectionResetError):
            return
        except ServiceError as exc:
            self._error(exc)
        except Exception as exc:  # keep one malformed request from killing the process
            self._error(exc)

    def do_POST(self) -> None:  # noqa: N802
        try:
            path, parts, _ = self._path()
            if path == "/api/messages":
                payload = self._body_json()
                item = self.service.add_message(payload.get("text", ""), payload.get("author", payload.get("sender", "匿名设备")))
                self._send_json(item, 201)
            elif path == "/api/clipboard":
                payload = self._body_json()
                item = self.service.set_clipboard(payload.get("text", ""), payload.get("source", payload.get("sender", "匿名设备")))
                self._send_json(item, 200)
            elif path == "/api/files":
                self._upload()
            else:
                raise ServiceError("NOT_FOUND", "接口不存在", 404)
        except ServiceError as exc:
            self._error(exc)
        except Exception as exc:
            self._error(exc)

    def do_DELETE(self) -> None:  # noqa: N802
        try:
            path, parts, _ = self._path()
            if len(parts) == 3 and parts[:2] == ["api", "messages"]:
                self.service.delete_message(parts[2])
            elif len(parts) == 3 and parts[:2] == ["api", "files"]:
                self.service.delete_file(parts[2])
            else:
                raise ServiceError("NOT_FOUND", "接口不存在", 404)
            self._send_json({"ok": True})
        except ServiceError as exc:
            self._error(exc)
        except Exception as exc:
            self._error(exc)

    def _sse(self, query: dict[str, list[str]]) -> None:
        try:
            since = int(query.get("since", ["0"])[0])
        except ValueError as exc:
            raise ServiceError("INVALID_REVISION", "revision 无效") from exc
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream; charset=utf-8")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "keep-alive")
        self.send_header("X-Accel-Buffering", "no")
        self.end_headers()
        subscriber = self.service.events.subscribe()
        try:
            self.wfile.write(b": connected\n\n")
            self.wfile.flush()
            for change in self.service.changes_since(since):
                self._write_event(change["revision"], change)
            while True:
                try:
                    event = subscriber.get(timeout=15)
                    payload = {"revision": event.revision, "resource_type": event.resource_type, "resource_id": event.resource_id, "action": event.action}
                    self._write_event(event.revision, payload)
                except queue.Empty:
                    self.wfile.write(b": heartbeat\n\n")
                    self.wfile.flush()
        finally:
            self.service.events.unsubscribe(subscriber)

    def _write_event(self, event_id: int, payload: object) -> None:
        body = json.dumps(payload, ensure_ascii=False, separators=(",", ":"))
        self.wfile.write(f"id: {event_id}\ndata: {body}\n\n".encode("utf-8"))
        self.wfile.flush()

    def _upload(self) -> None:
        content_length = self.headers.get("Content-Length")
        length = int(content_length) if content_length and content_length.isdigit() else None
        if length is not None and length > self.service.max_file_bytes + 2 * 1024 * 1024:
            raise ServiceError("FILE_TOO_LARGE", "上传请求过大", 413)
        content_type = self.headers.get("Content-Type", "")
        if not content_type.startswith("multipart/form-data"):
            raise ServiceError("INVALID_UPLOAD", "上传需要 multipart/form-data")
        form = cgi.FieldStorage(fp=self.rfile, headers=self.headers, environ={"REQUEST_METHOD": "POST", "CONTENT_TYPE": content_type})
        field = form["file"] if "file" in form else None
        if field is None or not getattr(field, "file", None):
            raise ServiceError("INVALID_UPLOAD", "缺少 file 字段")
        item = self.service.save_upload(field.file, getattr(field, "filename", "upload.bin"), getattr(field, "type", None), None)
        self._send_json(item, 201)

    def _download(self, file_id: str) -> None:
        item, path = self.service.file_path(file_id)
        self.send_response(200)
        self.send_header("Content-Type", str(item["media_type"]))
        self.send_header("Content-Length", str(item["size"]))
        self.send_header("Content-Disposition", f"attachment; filename*=UTF-8''{_quote_filename(str(item['name']))}")
        self.end_headers()
        with path.open("rb") as source:
            while chunk := source.read(1024 * 1024):
                self.wfile.write(chunk)

    def _static(self, path: str) -> None:
        relative = "index.html" if path in {"", "/"} else path.removeprefix("/")
        if ".." in Path(relative).parts:
            raise ServiceError("NOT_FOUND", "资源不存在", 404)
        target = (WEB_DIR / relative).resolve()
        if WEB_DIR not in target.parents and target != WEB_DIR:
            raise ServiceError("NOT_FOUND", "资源不存在", 404)
        if not target.is_file():
            raise ServiceError("NOT_FOUND", "资源不存在", 404)
        body = target.read_bytes()
        self.send_response(200)
        self.send_header("Content-Type", mimetypes.guess_type(target.name)[0] or "application/octet-stream")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-cache")
        self.end_headers()
        self.wfile.write(body)


def _quote_filename(value: str) -> str:
    from urllib.parse import quote
    return quote(value, safe="!#$&+-.^_`|~")


def run_server(
    host: str,
    port: int,
    data_dir: str | Path,
    *,
    max_file_bytes: int = 100 * 1024 * 1024,
    max_message_chars: int = 10_000,
) -> None:
    server = MeshShareServer((host, port), data_dir=Path(data_dir), max_file_bytes=max_file_bytes, max_message_chars=max_message_chars)
    actual_port = server.server_address[1]
    print(f"MeshShare running on http://{host}:{actual_port}", flush=True)
    for address in lan_addresses():
        print(f"  LAN client: http://{address}:{actual_port}", flush=True)
    print("Press Ctrl-C to stop.", flush=True)
    try:
        server.serve_forever(poll_interval=0.2)
    except KeyboardInterrupt:
        print("\nStopping MeshShare.", flush=True)
    finally:
        server.server_close()
