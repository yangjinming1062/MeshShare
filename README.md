# MeshShare

MeshShare 是一个局域网共享桌面程序：在一台电脑双击启动后，它会显示 GUI、启动服务并打开共享板；手机、平板和其他电脑用浏览器访问同一个地址，即可共享文本、最近一次剪贴板内容和文件。

## 快速开始

开发环境需要 Python 3.9 或更高版本：

```bash
python3 -m meshshare
```

桌面程序会显示类似 `http://192.168.1.20:8765` 的地址并自动打开共享板。在同一 Wi‑Fi/局域网的设备上打开它即可。默认监听 `0.0.0.0:8765`；可用 `--host`、`--port` 修改。

数据默认保存在 `.meshshare`：SQLite 保存共享板状态，`.meshshare/files/` 保存上传文件。停止程序后再次启动会继续使用这些数据；也可以通过 `--data-dir ./data` 修改。

没有桌面环境时使用无界面模式：

```bash
python3 -m meshshare --headless --data-dir ./data
```

## 主要能力

- 多个浏览器同时查看和发布共享消息。
- 共享最新剪贴板文本，用户点击后复制到本机剪贴板（浏览器安全策略不允许网页后台读取系统剪贴板）。
- 上传、下载和删除文件，并显示大小和上传时间。
- SSE 实时通知与断线后状态恢复。
- 仅使用 Python 标准库，无需安装第三方服务。
- GitHub Release 提供 Windows、macOS Intel、macOS Apple Silicon 和 Linux x64 独立程序包。

这是一个“桌面 GUI 共享中心 + 多终端浏览器”的首版：一台电脑运行桌面程序，其他电脑、手机和平板访问同一个局域网地址。它把共享状态集中保存，避免每台设备安装和维护独立客户端；桌面系统级后台剪贴板监听和无中心 P2P 会在后续版本作为独立模块加入。

## 验证

```bash
python3 -m compileall -q meshshare
python3 -m meshshare --help
```

从源码构建桌面程序需要安装 PyInstaller；GitHub tag workflow 会在对应平台 runner 上自动完成这一步并把构建包附加到 Release。

服务默认只适用于可信局域网。不要把监听端口直接暴露到公网；需要跨网访问时，请在受控的 VPN 或带认证/TLS 的反向代理后使用。

架构和设计约束见 [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)、[`docs/ARCHITECTURE_MAP.md`](docs/ARCHITECTURE_MAP.md) 和 [`docs/DESIGN.md`](docs/DESIGN.md)。
