"""Native desktop launcher for MeshShare.

The shared board remains a browser UI so phones and tablets can use the same
client. This Tk window owns the LAN server lifecycle and makes the desktop
experience a double-click application when bundled with PyInstaller.
"""

from __future__ import annotations

import threading
import tkinter as tk
import webbrowser
from pathlib import Path
from tkinter import messagebox

from .server import MeshShareServer, lan_addresses


class MeshShareApp:
    def __init__(self, root: tk.Tk, *, host: str, port: int, data_dir: str | Path) -> None:
        self.root = root
        self.root.title("MeshShare · 局域网共享")
        self.root.geometry("560x390")
        self.root.minsize(480, 330)
        self.root.protocol("WM_DELETE_WINDOW", self.close)
        self.server = MeshShareServer(
            (host, port),
            data_dir=Path(data_dir),
            max_file_bytes=100 * 1024 * 1024,
            max_message_chars=10_000,
        )
        self.thread = threading.Thread(target=self.server.serve_forever, kwargs={"poll_interval": 0.2}, daemon=True)
        self.thread.start()
        self.port = self.server.server_address[1]
        if host not in {"", "0.0.0.0", "::"}:
            self.urls = [f"http://{host}:{self.port}"]
        else:
            self.urls = [f"http://{address}:{self.port}" for address in lan_addresses()]
        self._build_ui()
        self.root.after(600, self.open_board)

    def _build_ui(self) -> None:
        outer = tk.Frame(self.root, padx=28, pady=24)
        outer.pack(fill="both", expand=True)
        tk.Label(outer, text="MeshShare", font=("Arial", 25, "bold"), anchor="w").pack(fill="x")
        tk.Label(
            outer,
            text="局域网共享已启动。把下面的地址发给同一 Wi‑Fi 中的手机、平板或电脑。",
            fg="#5f6b7a",
            justify="left",
            anchor="w",
            wraplength=500,
        ).pack(fill="x", pady=(8, 18))

        tk.Label(outer, text="共享板地址", font=("Arial", 11, "bold"), anchor="w").pack(fill="x")
        address_frame = tk.Frame(outer, bg="#f1f4f8", padx=12, pady=10)
        address_frame.pack(fill="x", pady=(7, 10))
        for url in self.urls:
            tk.Label(address_frame, text=url, bg="#f1f4f8", fg="#1d4ed8", anchor="w", font=("Courier", 11)).pack(fill="x")

        self.status = tk.Label(outer, text=f"运行中 · 端口 {self.port} · 数据目录 {Path(self.server.service.data_dir)}", fg="#147a4b", anchor="w")
        self.status.pack(fill="x", pady=(4, 18))
        actions = tk.Frame(outer)
        actions.pack(fill="x")
        tk.Button(actions, text="打开共享板", command=self.open_board, padx=12, pady=6).pack(side="left")
        tk.Button(actions, text="复制地址", command=self.copy_address, padx=12, pady=6).pack(side="left", padx=8)
        tk.Button(actions, text="退出", command=self.close, padx=12, pady=6).pack(side="right")
        tk.Label(
            outer,
            text="关闭此窗口会停止局域网服务。文件和共享内容保存在数据目录中。",
            fg="#7b8491",
            anchor="w",
            justify="left",
            wraplength=500,
        ).pack(fill="x", side="bottom", pady=(20, 0))

    @property
    def primary_url(self) -> str:
        return self.urls[0] if self.urls else f"http://127.0.0.1:{self.port}"

    def open_board(self) -> None:
        webbrowser.open(self.primary_url)

    def copy_address(self) -> None:
        self.root.clipboard_clear()
        self.root.clipboard_append(self.primary_url)
        self.root.update()
        self.status.configure(text=f"已复制地址 · {self.primary_url}")

    def close(self) -> None:
        if not messagebox.askyesno("退出 MeshShare", "关闭窗口并停止局域网服务？", parent=self.root):
            return
        self.server.shutdown()
        self.server.server_close()
        self.root.destroy()


def run_gui(host: str, port: int, data_dir: str | Path) -> None:
    root = tk.Tk()
    try:
        MeshShareApp(root, host=host, port=port, data_dir=data_dir)
    except OSError as error:
        root.withdraw()
        messagebox.showerror("MeshShare 启动失败", f"无法启动局域网服务：{error}")
        root.destroy()
        return
    root.mainloop()
