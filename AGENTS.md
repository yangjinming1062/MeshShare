# AGENTS.md

## 任务入口

先阅读 [`RULES.md`](RULES.md) 了解协作、文档、代码和验证规则，再按任务范围读取对应文档：

- 产品介绍、安装和上手见 [`README.md`](README.md)
- 源码启动、构建和验证见 [`docs/DEVELOPMENT.md`](docs/DEVELOPMENT.md)
- 模块边界、状态权威、信任边界和平台：[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)
- 拓扑、同步序列和部署图：[`docs/ARCHITECTURE_MAP.md`](docs/ARCHITECTURE_MAP.md)
- 用户可见行为、限制和验收：[`docs/DESIGN.md`](docs/DESIGN.md)
- 节点 HTTP 接口、WebSocket 消息和传输约束见 [`docs/PROTOCOL.md`](docs/PROTOCOL.md)
- Rust/Tauri 节点与桌面入口：`src-tauri/`
- Svelte/TypeScript 桌面和移动客户端：`web/`

实现改动前确认生产方、消费方和恢复路径；改动公开 API、持久化 schema 或文件生命周期时同步架构文档和相关验证。不要向仓库提交为临时验证创建的测试代码；正式检查放在现有测试入口。
