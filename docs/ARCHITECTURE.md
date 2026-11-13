# MeshShare 架构

## 范围与目标

MeshShare 是一个在同一局域网内共享短文本、剪贴板内容和文件的轻量服务。一个设备运行服务端，其余设备用浏览器打开服务地址作为客户端。服务端是状态权威，客户端只负责展示和发起操作；数据不离开局域网。

当前版本以单房间为边界。服务端绑定一个本地数据目录，重启后仍保留消息、剪贴板快照和已上传文件。多客户端通过 HTTP API 读写状态，通过 Server-Sent Events（SSE）接收变更通知；SSE 断线后由客户端重新拉取完整状态恢复。

这是对参考方案的首版收敛：用一个局域网共享中心和浏览器客户端替代桌面节点全互联、mDNS 和原生剪贴板守护进程。这样手机、平板和三种桌面系统可以立即共用同一客户端，服务端的领域服务与事件边界也为后续增加桌面节点互联保留了位置。

## 模块与依赖方向

```text
桌面 GUI 启动器 (`meshshare/gui.py`)
       │ 启动/停止
       ▼
HTTP 服务 (`meshshare/server.py`) ◄──── HTTP/JSON、multipart、SSE ──── 浏览器 PWA (`web/`)
       │ 校验请求、路由、错误映射
       ▼
领域服务 (`meshshare/service.py`)
       │ 事务边界、事件发布、文件生命周期
       ├──────────────► 持久化 (meshshare/store.py, SQLite)
       └──────────────► 文件仓库 (data/files/)
```

- `meshshare/server.py` 只处理 HTTP 生命周期、路由和序列化，不直接拼接 SQL。
- `meshshare/gui.py` 管理桌面窗口和服务生命周期；它不复制共享板业务逻辑，移动端仍使用同一浏览器客户端。
- `meshshare/service.py` 定义共享板的命令（新增/删除消息、更新剪贴板、上传/删除文件）和查询，负责输入约束、事务和事件。
- `meshshare/store.py` 封装 SQLite schema、读写和 revision 分配；revision 单调递增，是客户端同步游标。
- `meshshare/events.py` 管理进程内订阅者和 SSE 心跳。事件丢失不影响正确性，客户端收到断线或游标不连续时回到 `/api/state`。
- `web/` 是无构建步骤的静态 PWA。它不保存服务端密钥，不假设固定主机名，使用当前页面 origin 调 API。

依赖方向只能从入口到领域服务，再到存储/文件仓库；存储层不导入 HTTP。静态客户端只能依赖公开 API。

## 公共接口

| 方法 | 路径 | 用途 |
|---|---|---|
| `GET` | `/api/health`、`/api/info` | 服务状态、节点信息和能力 |
| `GET` | `/api/state` | 返回当前 revision、消息、最新剪贴板和文件元数据 |
| `GET` | `/api/events?since=<revision>` | SSE 变更通知，客户端收到后重新读取 state |
| `POST` | `/api/messages` | JSON `{text, author}` 发布消息 |
| `POST` | `/api/clipboard` | JSON `{text, source}` 更新最近共享剪贴板 |
| `POST` | `/api/files` | multipart `file` 上传文件 |
| `GET` | `/api/files/<id>` | 下载文件 |
| `DELETE` | `/api/messages/<id>`、`/api/files/<id>` | 删除共享内容 |

写入接口返回 JSON；输入错误、超限和不存在的资源使用 4xx，并带有 `error.code` 与 `error.message`。SSE 事件只携带 revision 和资源定位信息，正文与二进制内容始终通过普通 API 获取。

## 状态与一致性

SQLite 中保存 `messages`、`clipboard`、`files` 和 `changes`。每次成功命令在同一事务内写入业务数据和一条 change，change 的 `revision` 全局递增。读取 `/api/state` 返回当前 revision 的完整可渲染快照（最近消息和文件元数据），而不是让客户端自行拼接事件，因此重连和多标签页都可恢复；`since` 只用于 SSE 游标。

事件负载只包含 `revision`、资源类型和资源 id；客户端收到事件后重新获取状态。这样可避免把完整文件内容放进 SSE，也避免事件顺序成为业务正确性的唯一来源。

并发写入由服务端进程内锁和 SQLite 事务保护。删除会记录 change 事件并立即移除共享内容；当前单房间版本不提供用户身份认证，因此能访问局域网地址的设备都可以执行删除。

## 信任边界与安全

局域网内的所有能访问监听地址的设备都视为同一信任域。默认不向公网暴露，不启用 UPnP/端口映射。服务端限制请求体、单文件大小和文件名；文件保存使用随机 id，下载时使用安全的 `Content-Disposition` 名称，禁止路径穿越。文本按 UTF-8 处理并在浏览器以文本节点渲染，避免把消息当作 HTML。

如果网络不可信，应在反向代理后启用 TLS/访问令牌；这属于部署层增强，不由当前内置服务伪装提供。日志不记录消息正文、剪贴板正文或文件内容，只记录请求方法、路径、状态码和耗时。

## 生命周期与失败恢复

启动时创建数据目录和 schema，绑定监听地址后打印局域网访问地址；服务端退出时关闭 HTTP 监听，SQLite 连接按操作短暂打开并自动释放。文件先写入临时目录并校验 SHA-256，再原子移动到文件仓库；数据库写入失败会清理已移动文件。客户端 API 超时后显示离线状态并重试，服务端继续可被其他客户端使用。

## 平台支持

源码运行需要 Python 3.9+ 和 Tk，桌面打包使用 PyInstaller；Release 提供 Windows、macOS Intel、macOS Apple Silicon 和 Linux x64 独立程序。客户端支持现代 Chrome、Edge、Safari 和 Firefox；安装为 PWA 是可选项。发现服务地址依赖服务端启动时打印的局域网 IPv4 地址，首版不要求操作系统级发现协议。
