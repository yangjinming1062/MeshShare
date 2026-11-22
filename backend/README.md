# Rust/Tauri 模块指南

## 入口

- `src/lib.rs`：桌面与 Android 两套启动入口、Tauri 命令装配、托盘与悬浮条窗口。Android 入口不注册桌面专属命令。
- `src/server.rs`：HTTP 路由、WebSocket 会话、上传与下载流、跨来源校验。
- `src/runtime/`：节点状态权威。`mod.rs` 保存运行时结构与生命周期；`config.rs` 负责 `node.json`、节点名与暂存目录登记；`history.rs` 负责剪贴板历史索引与 payload；`outgoing.rs` 负责按路径枚举与摘要；`network.rs` 负责监听地址判定与对端地址规范化。
- `src/listeners.rs`：按网卡维护 HTTP/HTTPS 监听，网卡增减时重建，HTTPS 证书随地址热更新。
- `src/discovery.rs` 负责 mDNS 广播与浏览、手动连接和共享拨号。`src/discovery/tailscale.rs` 负责桌面端 Tailscale 客户端查询和对端探测。
- `src/clipboard.rs`：系统剪贴板读写与 PNG 校验，桌面轮询线程在此启动。
- `src/metrics.rs`：硬件快照采集；`src/browse.rs`：远程管理目录浏览。
- `src/monitor.rs`：桌面悬浮条临时显隐、全屏抑制与遮挡判定；各平台的 `fullscreen.rs` 读取原生窗口信息。
- `src/updater.rs`：桌面更新调度；`src/control.rs`：设置补丁、设置快照与更新服务装配；`src/model.rs`：HTTP 与 WebSocket 共用的数据结构。
- `src/mobile_tls.rs`：本地 CA 与服务端证书；`src/fsutil.rs`：数据文件写入与面向界面的绝对路径展示。

平台专属代码放在对应目录：`src/android/`（网络适配、存储授权、目录命令、`update/`）、`src/linux/`（系统包更新、登录项）、`src/macos/`（security-scoped bookmark、LaunchAgent）、`src/windows/`（注册表登录项）。`src/autostart.rs` 只保留平台路由、启动参数与 Linux/macOS 共用的启动脚本模板。共享文件里无法避免的单点平台差异继续用 `#[cfg]` 表达，不为此新建中间抽象。

## 设置与持久化

新增设置项的链路：`model.rs` 的结构 → `runtime/config.rs` 的 `PersistedConfig` → `control.rs` 的补丁与快照 → 界面读取 `GET /api/v1/settings`。只在本机生效、不能远程读取的设置（远程文件访问）不进设置快照。

数据目录保存 `node.json`、`clipboard-history.json` 与 payload 目录、`clipboard-current.json`、`transfer-history.json`、`desktop-transfer-temp-files.json`、`mobile-tls/`，Android 另有 `android-update.json`。

索引类文件用 `fsutil::atomic_write` 原子替换，私钥用 `fsutil::atomic_write_private`。启动时清空网页暂存与出站暂存目录，并回收剪贴板索引与 payload 目录残留的临时文件。

`node.json` 缺失时创建节点身份和默认设置，损坏或遇到其他读取错误时中止节点启动。剪贴板历史与 Android 更新设置在文件缺失或 JSON 解析失败时分别使用空历史和默认更新设置。权限等其他读取错误分别中止节点启动和 Android 更新服务初始化。传输历史读取或解析失败时使用空历史。

## 并发模型

长期线程包括系统剪贴板、指标采样、mDNS 守护和桌面悬浮条检测。Tokio 侧负责监听重扫、Tailscale 探测、清理循环、出站上传监督、更新协调与每个对端连接。所有后台循环都以 `runtime.stopping` 退出，新增循环必须检查它。

悬浮条检测每 500 毫秒串行采样，窗口几何采集与显隐应用提交到主线程，每轮只保留一个在途回调。首次显示先检测；全屏立即抑制，连续两次无全屏才恢复。检测错误短暂保留状态，持续两秒后解除抑制；窗口操作成功后才更新已应用状态，失败下轮重试，日志只在原因变化或恢复时输出。X11 使用独立非阻塞本地 socket，每轮请求及连接握手均有 400 毫秒预算；不复用 GTK 的 X11 连接，不请求其他进程的窗口标题。

Linux 启动时从 GTK 已有显示对象判定并保存后端能力。X11 在悬浮条隐藏时创建原生窗口并设置初始位置，首轮检测据此选择屏幕。隐藏前保留实际窗口位置，让下一次检测和显示都在同一屏幕上进行。遮挡判定使用显示器工作区，以便普通窗口最大化盖住工作区后恢复悬浮条。X11 优先采用 EWMH 全屏状态，只有窗口未最大化时才用无边框覆盖整屏的几何判定补充。macOS 无法区分全屏与最大化时保留悬浮条。

Windows 的点击穿透样式可能按窗口初始隐藏标记影响可见性，因此先改样式，再提交实际显隐。显示时重新置顶并保留前台焦点，手动操作会重新应用显隐。采样前检查悬浮条实际矩形，离屏时以隐藏状态恢复位置和目标屏幕的 DPI 尺寸，再采样该屏幕。移动事件提交时须与当前仍在屏幕上的窗口位置一致，最小化产生的临时坐标也会被忽略。

Tailscale 状态读取与整轮 HTTP 探测分别设定时间预算。探测限制并发并按地址轮转，失败地址独立退避。客户端未运行、读取失败或对端离线时撤销相应候选。每个节点的候选由一个拨号任务消费，健康会话优先，断开后先试上次成功的端点。mDNS 公告更新后，已开始拨号的地址仍可凭有效的 `Hello` 完成握手。重复发现同一地址时沿用当前退避间隔。

锁顺序固定为 `peers` → `peer_channels`，持有 `peer_channels` 时不得再取 `peers`。剪贴板事件由 `clipboard_update` 串行接受，校验成功后登记事件 ID。桌面剪贴板线程负责系统读写，待写请求合并到最新快照。线程用成功写入的内容去重，读出期间被新快照取代的内容也不会重新发布。配置修改先取快照、写盘成功后再回写内存。

## 接收端提交

同一项目串行写入同目录的 `.part`。校验通过后优先以硬链接提交，不替换已有文件；Android 文件系统不支持时使用不可覆盖重命名，再回退到进程锁内检查并重命名。临时文件登记只有在清理成功后才移除，以便下次启动继续回收。

模块内的 `#[cfg(test)]` 覆盖状态转换、边界校验与清理；真实托盘、剪贴板和安装器按[平台验收](../docs/DEVELOPMENT.md)检查。
