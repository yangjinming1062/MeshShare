# MeshShare 后端节点

桌面程序与 Android App 共用的 Rust 节点：一个进程内状态权威，通过 HTTP、WebSocket 和文件流同时服务本机界面、浏览器页面与对端节点。Tauri 壳只负责窗口、托盘、系统对话框和平台插件，不持有业务状态。

## 入口

- `src/lib.rs`：桌面与 Android 两套启动入口、Tauri 命令装配、托盘与悬浮条窗口。Android 入口不注册桌面专属命令。
- `src/server.rs`：HTTP 路由、WebSocket 会话、上传与下载流、跨来源校验。
- `src/runtime/`：节点状态权威。`mod.rs` 保存运行时结构与生命周期；`config.rs` 负责 `node.json`、节点名与暂存目录登记；`history.rs` 负责剪贴板历史索引与 payload；`outgoing.rs` 负责按路径枚举与摘要；`network.rs` 负责监听地址判定与对端地址规范化。
- `src/listeners.rs`：按网卡维护 HTTP/HTTPS 监听，网卡增减时重建，HTTPS 证书随地址热更新。
- `src/discovery.rs`：mDNS 广播与浏览、手动地址连接、拨号退避。
- `src/clipboard.rs`：系统剪贴板读写与 PNG 校验，桌面轮询线程在此启动。
- `src/metrics.rs`：硬件快照采集；`src/browse.rs`：远程管理目录浏览。
- `src/updater.rs`：桌面更新调度；`src/control.rs`：设置补丁、设置快照与更新服务装配；`src/model.rs`：HTTP 与 WebSocket 共用的数据结构。
- `src/mobile_tls.rs`：本地 CA 与服务端证书；`src/fsutil.rs`：数据文件写入与面向界面的绝对路径展示。

平台专属代码放在对应目录：`src/android/`（网络适配、存储授权、接收目录命令、`update/`）、`src/linux/`（系统包更新、登录项）、`src/macos/`（security-scoped bookmark、LaunchAgent）、`src/windows/`（注册表登录项）。`src/autostart.rs` 只保留平台路由、启动参数与 Linux/macOS 共用的启动脚本模板。共享文件里无法避免的单点平台差异继续用 `#[cfg]` 表达，不为此新建中间抽象。

## 接口面

本机界面、浏览器页面和远程管理使用同一套 HTTP 接口；设置类操作只有一条路径，不为同一状态再加命令。判据：需要系统对话框、窗口、托盘或平台插件时才新增 Tauri 命令，其余能力都走 HTTP。

当前保留的命令只做原生动作：节点地址、保存 CA 证书、更新代理、远程文件访问开关、悬浮条菜单与位置，以及 Android 的目录、网络和更新命令。

新增设置项的链路：`model.rs` 的结构 → `runtime/config.rs` 的 `PersistedConfig` → `control.rs` 的补丁与快照 → 界面读取 `GET /api/v1/settings`。只在本机生效、不能远程读取的设置（更新代理、远程文件访问）不进设置快照。

## 状态与数据文件

数据目录保存 `node.json`、`clipboard-history.json` 与 payload 目录、`clipboard-current.json`、`transfer-history.json`、`desktop-transfer-temp-files.json`、`mobile-tls/`，Android 另有 `android-update.json`。

索引类文件用 `fsutil::atomic_write` 原子替换，私钥用 `fsutil::atomic_write_private`。启动时清空网页暂存与出站暂存目录，并回收剪贴板索引与 payload 目录残留的临时文件。

`node.json` 缺失时创建节点身份和默认设置，损坏或遇到其他读取错误时中止节点启动。剪贴板历史与 Android 更新设置在文件缺失或 JSON 解析失败时分别使用空历史和默认更新设置。权限等其他读取错误分别中止节点启动和 Android 更新服务初始化。传输历史读取或解析失败时使用空历史。

## 并发模型

长期线程有三个：系统剪贴板、指标采样、mDNS 守护。Tokio 侧负责监听重扫、清理循环、出站上传监督、更新协调与每个对端连接。所有后台循环都以 `runtime.stopping` 退出，新增循环必须检查它。

锁顺序固定为 `peers` → `peer_channels`，持有 `peer_channels` 时不得再取 `peers`。剪贴板事件由 `clipboard_update` 串行接受，校验成功后登记事件 ID。桌面剪贴板线程负责系统读写，待写请求合并到最新快照。线程用成功写入的内容去重，读出期间被新快照取代的内容也不会重新发布。配置修改先取快照、写盘成功后再回写内存。

## 传输管线

发送端先准备清单：桌面按原路径枚举、计算摘要（`/api/v1/outgoing/paths`）；Android 与浏览器把字节交给本机暂存（`/api/v1/outgoing` + PUT + start）。接收端校验清单、目标节点标识与项目路径后返回进度，接受时由本地选择目标目录。

上传按项目加锁，流式校验大小与清单摘要（清单留空时按写入内容回填），校验通过后用硬链接提交为最终文件名；Android 应用专属存储不支持硬链接时使用不可覆盖重命名，旧内核回退到进程内串行检查。失败重试以 `retry_count` 代次单调推进，终态转换只在服务端判定一次。网页接收使用短期会话与暂存目录，会话过期或任务终态后清理。

## 安全边界

`origin_guard` 只允许本机、私网与 Tauri 来源的写请求和 WebSocket 升级；暂存接口与远程管理接口分别要求回环来源和目标机器开启远程文件访问。传输路径拒绝绝对路径、`..`、符号链接目录与同名覆盖。监听地址只包含回环、RFC1918、链路本地和 Tailscale IPv4，公网地址既不监听也不接受手动连接。

## 平台差异

Android 不启动 `arboard`、不注册托盘与悬浮条命令，网络绑定由原生插件完成；关闭 App 不保持连接。Linux 的悬浮条全局坐标、置顶与点击穿透取决于会话类型，Wayland 下这些能力按不支持处理。

## 验证

```bash
cargo fmt --manifest-path backend/Cargo.toml --all -- --check
cargo clippy --manifest-path backend/Cargo.toml --all-targets --all-features --locked -- -D warnings
cargo test --manifest-path backend/Cargo.toml --locked
```

测试写在各模块的 `#[cfg(test)] mod tests` 中，覆盖协议校验、路径安全、传输状态机与生命周期清理；不要为临时排查新建测试文件。涉及托盘、剪贴板会话、Android 插件和安装器的行为需要在目标平台手工验收。
