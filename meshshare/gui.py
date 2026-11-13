"""Native desktop client for MeshShare.

The HTTP/SSE web client is still served for phones and tablets, but the
computer client renders the shared board directly in this window.
"""

from __future__ import annotations

import mimetypes
import shutil
import threading
import tkinter as tk
from pathlib import Path
from tkinter import filedialog, messagebox, ttk
from typing import Any, Callable

from .server import MeshShareServer, lan_addresses
from .service import ServiceError

BG = "#f4f7fb"
CARD = "#ffffff"
INK = "#172033"
MUTED = "#6b7485"
LINE = "#e1e7ef"
BLUE = "#315bd6"
BLUE_DARK = "#2448b8"
GREEN = "#16845a"
RED = "#bd3e55"
HEADER = "#121b2e"


def format_size(value: int) -> str:
    if value < 1024:
        return f"{value} B"
    if value < 1024 * 1024:
        return f"{value / 1024:.1f} KB"
    if value < 1024 * 1024 * 1024:
        return f"{value / 1024 / 1024:.1f} MB"
    return f"{value / 1024 / 1024 / 1024:.1f} GB"


def short_time(value: str) -> str:
    return value.replace("T", " ").replace("+00:00", " UTC")[:19]


class MeshShareApp:
    """Own the LAN server and render all desktop sharing features natively."""

    def __init__(self, root: tk.Tk, *, host: str, port: int, data_dir: str | Path) -> None:
        self.root = root
        self.root.title("MeshShare · 局域网共享")
        self.root.geometry("1120x760")
        self.root.minsize(900, 620)
        self.root.configure(bg=BG)
        self.root.protocol("WM_DELETE_WINDOW", self.minimize_to_taskbar)
        self._closing = False
        self._uploading = False

        self.server = MeshShareServer(
            (host, port), data_dir=Path(data_dir),
            max_file_bytes=100 * 1024 * 1024, max_message_chars=10_000,
        )
        self.service = self.server.service
        self.thread = threading.Thread(
            target=self.server.serve_forever, kwargs={"poll_interval": 0.2},
            daemon=True, name="meshshare-http",
        )
        self.thread.start()
        self.port = self.server.server_address[1]
        if host not in {"", "0.0.0.0", "::"}:
            self.urls = [f"http://{host}:{self.port}"]
        else:
            self.urls = [f"http://{address}:{self.port}" for address in lan_addresses()]
        self.primary_url = self.urls[0] if self.urls else f"http://127.0.0.1:{self.port}"

        self._build_styles()
        self._build_menu()
        self._build_ui()
        self.refresh_state()

    def _build_styles(self) -> None:
        style = ttk.Style(self.root)
        try:
            style.theme_use("clam")
        except tk.TclError:
            pass
        style.configure("App.TFrame", background=BG)
        style.configure("Header.TFrame", background=HEADER)
        style.configure("Card.TFrame", background=CARD)
        style.configure("Title.TLabel", background=HEADER, foreground="#ffffff", font=("Arial", 25, "bold"))
        style.configure("HeaderSub.TLabel", background=HEADER, foreground="#b7c3d8", font=("Arial", 10))
        style.configure("HeaderMeta.TLabel", background=HEADER, foreground="#aebbd0", font=("Arial", 9))
        style.configure("CardTitle.TLabel", background=CARD, foreground=INK, font=("Arial", 14, "bold"))
        style.configure("CardHint.TLabel", background=CARD, foreground=MUTED, font=("Arial", 9))
        style.configure("Primary.TButton", background=BLUE, foreground="#ffffff", padding=(12, 7), font=("Arial", 10, "bold"))
        style.map("Primary.TButton", background=[("active", BLUE_DARK), ("disabled", "#9eaddb")])
        style.configure("Secondary.TButton", background="#edf1ff", foreground="#2448b8", padding=(11, 7), font=("Arial", 10))
        style.configure("Danger.TButton", background="#fff0f2", foreground=RED, padding=(11, 7), font=("Arial", 10))
        style.configure("Treeview", background="#fbfcfe", fieldbackground="#fbfcfe", foreground=INK, rowheight=30, font=("Arial", 9))
        style.configure("Treeview.Heading", background="#eef2f7", foreground="#526074", font=("Arial", 9, "bold"))
        style.map("Treeview", background=[("selected", "#dce6ff")], foreground=[("selected", INK)])

    def _build_menu(self) -> None:
        menu = tk.Menu(self.root)
        app_menu = tk.Menu(menu, tearoff=False)
        app_menu.add_command(label="显示窗口", command=self.show_window)
        app_menu.add_separator()
        app_menu.add_command(label="退出程序", command=self.exit_app)
        menu.add_cascade(label="MeshShare", menu=app_menu)
        help_menu = tk.Menu(menu, tearoff=False)
        help_menu.add_command(label="复制手机访问地址", command=self.copy_address)
        menu.add_cascade(label="帮助", menu=help_menu)
        self.root.configure(menu=menu)

    def _build_ui(self) -> None:
        self.root.columnconfigure(0, weight=1)
        self.root.rowconfigure(1, weight=1)
        header = ttk.Frame(self.root, style="Header.TFrame", padding=(28, 21, 28, 19))
        header.grid(row=0, column=0, sticky="nsew")
        header.columnconfigure(0, weight=1)
        brand = ttk.Frame(header, style="Header.TFrame")
        brand.grid(row=0, column=0, sticky="w")
        ttk.Label(brand, text="MeshShare", style="Title.TLabel").pack(anchor="w")
        ttk.Label(brand, text="局域网共享中心 · 桌面原生客户端", style="HeaderSub.TLabel").pack(anchor="w", pady=(5, 0))
        network = ttk.Frame(header, style="Header.TFrame")
        network.grid(row=0, column=1, sticky="e")
        status_line = ttk.Frame(network, style="Header.TFrame")
        status_line.pack(anchor="e")
        self.status_dot = tk.Label(status_line, text="●", bg=HEADER, fg="#55d39a", font=("Arial", 13))
        self.status_dot.pack(side="left")
        self.status_label = ttk.Label(status_line, text="服务运行中", style="HeaderSub.TLabel")
        self.status_label.pack(side="left", padx=(5, 0))
        self.address_label = ttk.Label(network, text=f"手机访问：{self.primary_url}", style="HeaderMeta.TLabel")
        self.address_label.pack(anchor="e", pady=(6, 0))
        ttk.Button(network, text="复制地址", style="Secondary.TButton", command=self.copy_address).pack(anchor="e", pady=(8, 0))

        body = ttk.Frame(self.root, style="App.TFrame", padding=(22, 18, 22, 12))
        body.grid(row=1, column=0, sticky="nsew")
        body.columnconfigure(0, weight=3, uniform="columns")
        body.columnconfigure(1, weight=2, uniform="columns")
        body.rowconfigure(1, weight=1)
        self._build_clipboard_card(body)
        self._build_messages_card(body)
        self._build_files_card(body)
        footer = ttk.Frame(self.root, style="App.TFrame", padding=(22, 2, 22, 14))
        footer.grid(row=2, column=0, sticky="ew")
        ttk.Label(footer, text="关闭按钮只会最小化窗口，服务继续运行；从 MeshShare 菜单选择“退出程序”才会停止服务。", style="CardHint.TLabel").pack(anchor="w")

    def _card(self, parent: ttk.Frame, row: int, column: int, columnspan: int = 1) -> ttk.Frame:
        frame = ttk.Frame(parent, style="Card.TFrame", padding=18)
        frame.grid(row=row, column=column, columnspan=columnspan, sticky="nsew", padx=7, pady=7)
        frame.columnconfigure(0, weight=1)
        frame.rowconfigure(2, weight=1)
        return frame

    def _build_clipboard_card(self, parent: ttk.Frame) -> None:
        card = self._card(parent, 0, 0, 2)
        head = ttk.Frame(card, style="Card.TFrame")
        head.grid(row=0, column=0, sticky="ew")
        head.columnconfigure(0, weight=1)
        ttk.Label(head, text="共享剪贴板", style="CardTitle.TLabel").grid(row=0, column=0, sticky="w")
        self.clipboard_meta = ttk.Label(head, text="还没有共享内容", style="CardHint.TLabel")
        self.clipboard_meta.grid(row=0, column=1, sticky="e")
        self.clipboard_text = tk.Text(card, height=5, wrap="word", undo=True, bg="#fbfcfe", fg=INK, insertbackground=INK, relief="solid", bd=1, highlightthickness=0, padx=10, pady=9, font=("Arial", 10))
        self.clipboard_text.grid(row=1, column=0, sticky="nsew", pady=(10, 10))
        card.rowconfigure(1, weight=1)
        actions = ttk.Frame(card, style="Card.TFrame")
        actions.grid(row=2, column=0, sticky="ew")
        actions.columnconfigure(0, weight=1)
        self.clipboard_source = ttk.Entry(actions)
        self.clipboard_source.insert(0, self.service.node_name)
        self.clipboard_source.grid(row=0, column=0, sticky="ew", padx=(0, 8))
        ttk.Button(actions, text="发布到共享板", style="Primary.TButton", command=self.publish_clipboard).grid(row=0, column=1, padx=(0, 8))
        ttk.Button(actions, text="复制到本机", style="Secondary.TButton", command=self.copy_clipboard).grid(row=0, column=2)

    def _build_messages_card(self, parent: ttk.Frame) -> None:
        card = self._card(parent, 1, 0)
        head = ttk.Frame(card, style="Card.TFrame")
        head.grid(row=0, column=0, sticky="ew")
        head.columnconfigure(0, weight=1)
        ttk.Label(head, text="共享消息", style="CardTitle.TLabel").grid(row=0, column=0, sticky="w")
        self.message_count = ttk.Label(head, text="0 条", style="CardHint.TLabel")
        self.message_count.grid(row=0, column=1, sticky="e")
        compose = ttk.Frame(card, style="Card.TFrame")
        compose.grid(row=1, column=0, sticky="ew", pady=(11, 11))
        compose.columnconfigure(1, weight=1)
        self.message_author = ttk.Entry(compose, width=18)
        self.message_author.insert(0, self.service.node_name)
        self.message_author.grid(row=0, column=0, padx=(0, 7), sticky="ew")
        self.message_input = ttk.Entry(compose)
        self.message_input.grid(row=0, column=1, padx=(0, 7), sticky="ew")
        self.message_input.bind("<Return>", lambda _event: self.publish_message())
        ttk.Button(compose, text="发送", style="Primary.TButton", command=self.publish_message).grid(row=0, column=2)
        table_frame = ttk.Frame(card, style="Card.TFrame")
        table_frame.grid(row=2, column=0, sticky="nsew")
        table_frame.columnconfigure(0, weight=1)
        table_frame.rowconfigure(0, weight=1)
        self.message_tree = ttk.Treeview(table_frame, columns=("author", "time", "text"), show="headings", selectmode="browse")
        self.message_tree.heading("author", text="设备")
        self.message_tree.heading("time", text="时间")
        self.message_tree.heading("text", text="内容")
        self.message_tree.column("author", width=120, anchor="w", stretch=False)
        self.message_tree.column("time", width=142, anchor="w", stretch=False)
        self.message_tree.column("text", width=280, anchor="w")
        self.message_tree.grid(row=0, column=0, sticky="nsew")
        scroll = ttk.Scrollbar(table_frame, orient="vertical", command=self.message_tree.yview)
        scroll.grid(row=0, column=1, sticky="ns")
        self.message_tree.configure(yscrollcommand=scroll.set)
        ttk.Button(card, text="删除选中的消息", style="Danger.TButton", command=self.delete_message).grid(row=3, column=0, sticky="e", pady=(10, 0))

    def _build_files_card(self, parent: ttk.Frame) -> None:
        card = self._card(parent, 1, 1)
        card.rowconfigure(1, weight=1)
        head = ttk.Frame(card, style="Card.TFrame")
        head.grid(row=0, column=0, sticky="ew")
        head.columnconfigure(0, weight=1)
        ttk.Label(head, text="文件中转", style="CardTitle.TLabel").grid(row=0, column=0, sticky="w")
        self.file_count = ttk.Label(head, text="0 个", style="CardHint.TLabel")
        self.file_count.grid(row=0, column=1, sticky="e")
        table_frame = ttk.Frame(card, style="Card.TFrame")
        table_frame.grid(row=1, column=0, sticky="nsew", pady=(11, 10))
        table_frame.columnconfigure(0, weight=1)
        table_frame.rowconfigure(0, weight=1)
        self.file_tree = ttk.Treeview(table_frame, columns=("name", "size", "time"), show="headings", selectmode="browse")
        self.file_tree.heading("name", text="文件")
        self.file_tree.heading("size", text="大小")
        self.file_tree.heading("time", text="时间")
        self.file_tree.column("name", width=245, anchor="w")
        self.file_tree.column("size", width=80, anchor="e", stretch=False)
        self.file_tree.column("time", width=138, anchor="w", stretch=False)
        self.file_tree.grid(row=0, column=0, sticky="nsew")
        scroll = ttk.Scrollbar(table_frame, orient="vertical", command=self.file_tree.yview)
        scroll.grid(row=0, column=1, sticky="ns")
        self.file_tree.configure(yscrollcommand=scroll.set)
        self.file_status = ttk.Label(card, text="文件只保存在当前共享电脑。", style="CardHint.TLabel")
        self.file_status.grid(row=2, column=0, sticky="w")
        actions = ttk.Frame(card, style="Card.TFrame")
        actions.grid(row=3, column=0, sticky="ew", pady=(10, 0))
        ttk.Button(actions, text="上传文件", style="Primary.TButton", command=self.choose_upload).pack(side="left")
        ttk.Button(actions, text="下载选中", style="Secondary.TButton", command=self.download_file).pack(side="left", padx=7)
        ttk.Button(actions, text="删除", style="Danger.TButton", command=self.delete_file).pack(side="right")

    def _set_status(self, text: str, online: bool = True) -> None:
        self.status_label.configure(text=text)
        self.status_dot.configure(fg="#55d39a" if online else "#f17b88")

    def refresh_state(self, schedule: bool = True) -> None:
        if self._closing:
            return
        try:
            state = self.service.state()
            self._set_status("服务运行中", True)
            clipboard = state.get("clipboard")
            if clipboard and self.root.focus_get() is not self.clipboard_text:
                self.clipboard_text.delete("1.0", "end")
                self.clipboard_text.insert("1.0", str(clipboard.get("text", "")))
            if clipboard:
                self.clipboard_meta.configure(text=f"{clipboard.get('source', '匿名设备')} · {short_time(str(clipboard.get('updated_at', '')))}")
            else:
                self.clipboard_meta.configure(text="还没有共享内容")
            self._render_messages(state.get("messages", []))
            self._render_files(state.get("files", []))
        except Exception as error:
            self._set_status(f"同步失败：{error}", False)
        if schedule:
            self.root.after(1200, self.refresh_state)

    def _render_messages(self, messages: list[dict[str, Any]]) -> None:
        selected = self.message_tree.selection()
        self.message_tree.delete(*self.message_tree.get_children())
        for item in messages:
            self.message_tree.insert("", "end", iid=str(item["id"]), values=(item.get("author", "匿名设备"), short_time(str(item.get("created_at", ""))), item.get("text", "")))
        if selected and self.message_tree.exists(selected[0]):
            self.message_tree.selection_set(selected[0])
        self.message_count.configure(text=f"{len(messages)} 条")

    def _render_files(self, files: list[dict[str, Any]]) -> None:
        selected = self.file_tree.selection()
        self.file_tree.delete(*self.file_tree.get_children())
        for item in files:
            self.file_tree.insert("", "end", iid=str(item["id"]), values=(item.get("name", ""), format_size(int(item.get("size", 0))), short_time(str(item.get("created_at", "")))))
        if selected and self.file_tree.exists(selected[0]):
            self.file_tree.selection_set(selected[0])
        self.file_count.configure(text=f"{len(files)} 个")

    def copy_address(self) -> None:
        self.root.clipboard_clear()
        self.root.clipboard_append(self.primary_url)
        self.root.update()
        self.address_label.configure(text=f"已复制：{self.primary_url}")

    def publish_clipboard(self) -> None:
        text = self.clipboard_text.get("1.0", "end-1c")
        try:
            self.service.set_clipboard(text, self.clipboard_source.get().strip() or self.service.node_name)
            self._set_status("剪贴板已发布")
            self.refresh_state(schedule=False)
        except ServiceError as error:
            messagebox.showerror("无法发布剪贴板", error.message, parent=self.root)

    def copy_clipboard(self) -> None:
        text = self.clipboard_text.get("1.0", "end-1c")
        if not text:
            return
        self.root.clipboard_clear()
        self.root.clipboard_append(text)
        self.root.update()
        self._set_status("已复制到本机剪贴板")

    def publish_message(self) -> None:
        text = self.message_input.get().strip()
        if not text:
            return
        try:
            self.service.add_message(text, self.message_author.get().strip() or self.service.node_name)
            self.message_input.delete(0, "end")
            self.refresh_state(schedule=False)
        except ServiceError as error:
            messagebox.showerror("无法发送消息", error.message, parent=self.root)

    def delete_message(self) -> None:
        selection = self.message_tree.selection()
        if not selection or not messagebox.askyesno("删除消息", "删除选中的共享消息？", parent=self.root):
            return
        try:
            self.service.delete_message(selection[0])
            self.refresh_state(schedule=False)
        except ServiceError as error:
            messagebox.showerror("无法删除消息", error.message, parent=self.root)

    def choose_upload(self) -> None:
        if self._uploading:
            return
        paths = filedialog.askopenfilenames(parent=self.root, title="选择要共享的文件")
        if not paths:
            return
        self._uploading = True
        self.file_status.configure(text=f"正在上传 {len(paths)} 个文件…")
        self._run_worker(lambda: self._upload_paths(paths), self._upload_finished)

    def _upload_paths(self, paths: tuple[str, ...]) -> tuple[int, str | None]:
        completed = 0
        try:
            for value in paths:
                path = Path(value)
                media_type = mimetypes.guess_type(path.name)[0] or "application/octet-stream"
                with path.open("rb") as stream:
                    self.service.save_upload(stream, path.name, media_type, path.stat().st_size)
                completed += 1
            return completed, None
        except Exception as error:
            return completed, str(error)

    def _upload_finished(self, result: tuple[int, str | None]) -> None:
        completed, error = result
        self._uploading = False
        self.file_status.configure(text=f"已上传 {completed} 个文件" if error is None else f"已上传 {completed} 个文件，发生错误：{error}")
        self.refresh_state(schedule=False)

    def download_file(self) -> None:
        selection = self.file_tree.selection()
        if not selection:
            messagebox.showinfo("下载文件", "请先选择一个文件。", parent=self.root)
            return
        try:
            item, source = self.service.file_path(selection[0])
        except ServiceError as error:
            messagebox.showerror("无法下载文件", error.message, parent=self.root)
            return
        target = filedialog.asksaveasfilename(parent=self.root, title="保存共享文件", initialfile=str(item["name"]))
        if not target:
            return
        self.file_status.configure(text=f"正在下载 {item['name']}…")
        self._run_worker(lambda: self._copy_file(source, Path(target)), lambda error: self._download_finished(item["name"], error))

    @staticmethod
    def _copy_file(source: Path, target: Path) -> str | None:
        try:
            shutil.copyfile(source, target)
            return None
        except Exception as error:
            return str(error)

    def _download_finished(self, name: object, error: str | None) -> None:
        self.file_status.configure(text=f"已下载 {name}" if error is None else f"下载失败：{error}")

    def delete_file(self) -> None:
        selection = self.file_tree.selection()
        if not selection or not messagebox.askyesno("删除文件", "删除选中的共享文件？", parent=self.root):
            return
        try:
            self.service.delete_file(selection[0])
            self.refresh_state(schedule=False)
        except ServiceError as error:
            messagebox.showerror("无法删除文件", error.message, parent=self.root)

    def _run_worker(self, function: Callable[[], Any], callback: Callable[[Any], None]) -> None:
        def worker() -> None:
            result = function()
            self.root.after(0, lambda: callback(result))

        threading.Thread(target=worker, daemon=True).start()

    def minimize_to_taskbar(self) -> None:
        self.root.iconify()
        self._set_status("后台运行中")

    def show_window(self) -> None:
        self.root.deiconify()
        self.root.lift()
        self.root.focus_force()

    def exit_app(self) -> None:
        if self._closing:
            return
        if not messagebox.askyesno("退出 MeshShare", "退出程序并停止局域网服务？", parent=self.root):
            return
        self._closing = True
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
