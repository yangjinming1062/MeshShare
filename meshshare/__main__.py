"""Command line entry point for ``python -m meshshare``."""

from __future__ import annotations

import argparse

from .server import run_server


def main() -> None:
    parser = argparse.ArgumentParser(prog="meshshare", description="MeshShare LAN sharing desktop client")
    parser.add_argument("--headless", action="store_true", help="run the LAN server without opening a desktop window")
    parser.add_argument("--host", default="0.0.0.0", help="bind address (default: all interfaces)")
    parser.add_argument("--port", type=int, default=8765, help="HTTP port (default: 8765)")
    parser.add_argument("--data-dir", default=".meshshare", help="persistent data directory")
    parser.add_argument("--max-upload-mb", type=int, default=100, help="maximum uploaded file size (default: 100 MB)")
    parser.add_argument("--max-message-chars", type=int, default=10_000, help="maximum message length (default: 10000)")
    args = parser.parse_args()
    if not 0 <= args.port <= 65535:
        parser.error("port must be between 0 and 65535")
    if args.max_upload_mb <= 0 or args.max_message_chars <= 0:
        parser.error("size limits must be positive")
    if args.headless:
        run_server(args.host, args.port, args.data_dir, max_file_bytes=args.max_upload_mb * 1024 * 1024, max_message_chars=args.max_message_chars)
        return
    from .gui import run_gui
    run_gui(args.host, args.port, args.data_dir)


if __name__ == "__main__":
    main()
