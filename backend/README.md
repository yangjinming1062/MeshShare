# Rust 节点维护指南

## 代码入口

| 工作内容 | 入口 |
| --- | --- |
| 启动、托盘与窗口 | [`src/lib.rs`](src/lib.rs) · [`src/desktop/`](src/desktop/) |
| Linux-headless 版与 systemd | [`src/headless.rs`](src/headless.rs) · [`src/headless_service/`](src/headless_service/) · [`packaging/headless/`](packaging/headless/) |
| 设备发现、端口与网页证书 | [`src/discovery.rs`](src/discovery.rs) · [`src/listeners.rs`](src/listeners.rs) · [`src/mobile_tls.rs`](src/mobile_tls.rs) |
| HTTP、WebSocket 与协议类型 | [`src/server.rs`](src/server.rs) · [`src/server/socket.rs`](src/server/socket.rs) · [`src/model.rs`](src/model.rs) |
| 共享状态、剪贴板历史、传输任务 | [`src/runtime/`](src/runtime/) |
| 系统剪贴板 | [`src/clipboard.rs`](src/clipboard.rs) |
| 设置、指标、悬浮条 | [`src/control.rs`](src/control.rs) · [`src/metrics.rs`](src/metrics.rs) · [`src/monitor.rs`](src/monitor.rs) |
| 本机终端、远程 SSH | [`src/terminal/`](src/terminal/) |
| 更新与 Android 原生插件 | [`src/updater/`](src/updater/) · [`src/android/update/`](src/android/update/) · [`android/`](android/) |

## 修改时核对

- [配置加载](src/runtime/config.rs)先校验 `node.json` 和节点角色，再进入[启动清理](src/runtime/mod.rs)。新增持久化设置时检查已有配置的读取、原子写入与重启恢复。节点配置损坏时保留原文件并停止启动，恢复应使用该节点的完整备份。
- 同时持有对端记录和消息通道锁时，沿用 `peers` → `peer_channels` 的顺序。共享剪贴板变更经同一[历史入口](src/runtime/history.rs)排序，复用现有的去重和持久化流程。
- 文件接收需要保留校验通过后提交、避免覆盖和失败后清理的行为。修改 [HTTP 接收](src/server.rs)时，同时核对[传输状态及临时文件登记](src/runtime/transfer.rs)和重启清理。

[运行检查、打包与平台验收](../docs/DEVELOPMENT.md) · [systemd 冒烟测试](tests/headless-smoke.mjs)
