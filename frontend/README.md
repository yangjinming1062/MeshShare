# Svelte 客户端维护指南

## 代码入口

| 工作内容 | 入口 |
| --- | --- |
| 主界面、目标切换和事件协调 | [`src/main.ts`](src/main.ts) · [`src/App.svelte`](src/App.svelte) |
| 节点请求、地址选择与协议类型 | [`src/api.ts`](src/api.ts) · [`src/peers.ts`](src/peers.ts) · [`src/types.ts`](src/types.ts) |
| 移动界面与目录保存 | [`src/components/MobileApp.svelte`](src/components/MobileApp.svelte) · [`src/mobileSave.ts`](src/mobileSave.ts) |
| 文件收集、上传与进度 | [`src/drop.ts`](src/drop.ts) · [`src/upload.ts`](src/upload.ts) · [`src/transferProgress.ts`](src/transferProgress.ts) |
| 剪贴板图片与历史查询 | [`src/components/ClipboardImage.svelte`](src/components/ClipboardImage.svelte) · [`src/clipboardSearch.ts`](src/clipboardSearch.ts) |
| 终端标签、会话和渲染 | [`src/components/TerminalWorkspace.svelte`](src/components/TerminalWorkspace.svelte) · [`src/terminal.ts`](src/terminal.ts) · [`src/terminalRenderer.ts`](src/terminalRenderer.ts) |
| Windows、macOS 悬浮条 | [`monitor.html`](monitor.html) · [`src/components/Monitor.svelte`](src/components/Monitor.svelte) |

## 修改时核对

- 节点请求复用 `api.ts` 的目标校验和错误类型。远程设置与远程目录操作绑定打开时的节点及界面会话。切换管理目标后，后台上传仍跟踪原任务所在的节点。
- 刷新复用 [`snapshotRead`](src/snapshotRead.ts) 合并并发读和补读。切换目标后忽略旧响应。剪贴板按节点时钟应用快照，用户正在编辑的草稿由界面维护。
- 升级 xterm 时核对 [Vite 配置](vite.config.ts)中的真彩色补丁，并在安装包中验证渲染。

前端单元测试位于 [`tests/`](tests/)，启动、类型检查、测试和构建命令见[开发指南](../docs/DEVELOPMENT.md)。
