# Rust/Tauri 模块指南

## 代码入口

| 维护事项 | 代码 |
| --- | --- |
| Linux 无界面服务 | `src/headless.rs` 装配独立服务、管理状态目录与退出。`src/headless_service/` 协调更新、检查签名并通过本机助手控制 systemd。 |
| 节点启动、Tauri 命令 | `src/lib.rs` 负责节点启动与 Android 装配。`src/desktop/` 负责桌面装配、托盘和悬浮条窗口。 |
| 监听与发现 | `src/listeners.rs`、`src/mobile_tls.rs` 管理端口和证书。`src/discovery.rs` 汇合手动、mDNS 和 Tailscale 候选。 |
| 请求与会话 | `src/server.rs` 负责 HTTP 路由、来源校验与文件流。`src/server/socket.rs` 处理 WebSocket 握手与消息。`src/model.rs` 定义线协议。 |
| 节点状态 | `src/runtime/mod.rs` 管理生命周期。`config.rs`、`history.rs`、`transfer_history.rs` 分别持有配置、剪贴板历史、传输历史。`peers.rs` 管理会话，`network.rs` 管理地址。`outgoing.rs`、`transfer.rs`、`mobile.rs` 分别处理发送、直传任务和网页暂存。 |
| 本机系统能力 | `src/clipboard.rs` 读写系统剪贴板，`src/metrics.rs` 采样，`src/monitor.rs` 判定全屏和显隐，`src/browse.rs` 浏览授权目录。平台实现位于 `src/{android,linux,macos,windows}/`。 |
| 终端 | `src/terminal/` 管理桌面伪终端、主窗口 IPC、桌面 SSH 接入和跨平台 SSH 客户端。 |
| 设置与更新 | `src/control.rs` 处理设置快照与补丁。`src/updater/desktop.rs` 调度桌面更新，`src/android/update/` 调度 Android 更新。 |

## 设置与持久化

Headless 新建的 `node.json` 带有 `headless: true`。已有配置缺少该字段时按原桌面或 Android 节点读取；角色不符时启动会在清理暂存目录前退出。每个服务实例独占自己的数据目录。

共享设置由 `runtime/config.rs` 的 `PersistedConfig` 落盘，经 `control.rs` 的补丁与快照提供给界面。桌面上的远程文件授权仅由本机原生命令修改。Headless 初始允许远程文件访问，设置接口同样不能扩大该授权。

SSH 会话使用进程内生成的密钥。桌面配置在 `node.json` 的 `terminal_access_migration` 中记录新版终端授权对应的旧策略摘要，或记录确认时没有旧策略。已有配置缺少此字段时保留旧版限制，即使从未生成过 `terminal-access.json`。桌面仅在自动继承旧版「允许所有节点」授权时从系统凭据库读取旧密钥并验证签名。新安装及本机手动确认均可在无 Secret Service 的 Linux 会话中使用。原策略文件和旧凭据保留供回退，操作入口见[使用指南](../docs/DESIGN.md#桌面终端)。

如果迁移时报旧授权文件不可读或无效，请从备份恢复该节点可读的普通文件后重试。如果 `node.json` 写入失败，修复本机数据目录的写入权限或可用空间后再确认。写入成功前，入站终端保持关闭。

数据目录保存 `node.json`、`clipboard-history.json` 与 payload 目录、`clipboard-current.json`、`transfer-history.json`、`desktop-transfer-temp-files.json`、`mobile-tls/`，Android 另有 `android-update.json`。

索引和配置由 `fsutil::atomic_write` 原子替换，本地 CA 私钥使用 `fsutil::atomic_write_private`。启动时清空网页暂存与出站暂存目录，并回收剪贴板索引与 payload 目录残留的临时文件。

`node.json` 缺失时创建节点身份和默认设置。已有文件缺少必填字段、损坏或无法读取时中止启动并保留原文件。需要恢复时，优先还原同一节点的完整配置备份。剪贴板历史与 Android 更新设置在文件缺失或 JSON 解析失败时分别使用空历史和默认更新设置。权限等其他读取错误分别中止节点启动和 Android 更新服务初始化。传输历史读取或解析失败时使用空历史。

## 并发模型

长期线程包括系统剪贴板、指标采样、mDNS 守护和桌面悬浮条检测。Tokio 侧负责监听重扫、Tailscale 探测、清理循环、出站上传监督、更新协调与每个对端连接。节点退出时置位 `runtime.stopping`，终端另用取消令牌关闭会话并回收子进程。

悬浮条每 500 毫秒串行采样，主线程读取窗口几何并应用显隐。首次显示前检查全屏，连续两次未全屏才恢复。持续两秒检测失败时解除抑制，窗口操作失败留待下轮重试。X11 非阻塞连接和请求各有 400 毫秒预算。

X11 从 GTK 获取显示后端，在窗口隐藏时定位到目标屏幕。优先使用 EWMH 全屏状态，几何补充判定排除最大化窗口。Windows 先应用点击穿透，再显隐并保留前台焦点。离屏窗口先按目标屏幕 DPI 恢复，延迟移动事件仅在仍与实际位置一致时写盘。

mDNS、手动连接与 Tailscale 共用每节点一个拨号任务。Tailscale 分批轮转探测，失败按地址退避。保留已连接端点，断开后优先尝试上次成功地址。mDNS 公告变化时，已发起的连接仍须完成身份与版本校验。

锁顺序固定为 `peers` → `peer_channels`，持有 `peer_channels` 时不得再取 `peers`。剪贴板事件由 `clipboard_update` 串行接受，校验成功后登记事件 ID。桌面剪贴板线程负责系统读写，待写请求合并到最新快照。线程用成功写入的内容去重，读出期间被新快照取代的内容也不会重新发布。配置修改先取快照、写盘成功后再回写内存。

## 接收端提交

同一项目串行写入同目录的 `.part`。校验通过后优先以硬链接提交，不替换已有文件；Android 文件系统不支持时使用不可覆盖重命名，再回退到进程锁内检查并重命名。临时文件登记只有在清理成功后才移除，以便下次启动继续回收。

模块内的 `#[cfg(test)]` 覆盖状态转换、边界校验与清理；真实托盘、剪贴板和安装器按[平台验收](../docs/DEVELOPMENT.md)检查。
