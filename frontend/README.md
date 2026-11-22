# Svelte 客户端模块指南

## 界面入口

`src/main.ts` 加载 `src/App.svelte`。由入口识别运行环境与窗口类型，选择桌面控制台、移动界面或只读悬浮条。`src/components/MobileApp.svelte` 通过回调复用上传、接收与剪贴板操作，不自行判断节点归属。

## 请求与状态

- `src/api.ts` 统一处理节点地址、JSON、缓存、重定向与 `ApiError`。本机地址来自 Tauri 或页面来源，远程目标由 `setApiTarget` 切换。错误分支按状态码与类型判断，不匹配错误文案。
- `src/types.ts` 映射线协议，保留 snake_case。`src/App.svelte` 编排状态与事件，组件通过 props 和回调交互；可独立验证的计算放在同级 TypeScript 模块。

## 按任务定位

| 任务 | 代码入口 |
| --- | --- |
| 设备地址与同源代理 | `src/peers.ts` 选择候选，`src/api.ts` 在操作前核对节点身份。 |
| 文件发送 | `src/upload.ts` 构造清单并上传文件。`src/App.svelte` 随节点快照刷新网页任务，页面直发任务单独向接收节点查询。 |
| 递归收集拖放目录、保留相对路径 | `src/drop.ts` |
| 流式下载、摘要与本地目录保存 | `src/mobileSave.ts`、`src/sha256.ts` |
| 传输进度与显示格式 | `src/transferProgress.ts`、`src/format.ts` |
| 历史预览的子序列匹配与排序 | `src/clipboardSearch.ts` |
| 桌面与 Android 终端标签、本机接入开关 | `src/components/TerminalWorkspace.svelte`、`src/components/TerminalAccess.svelte` |
| 终端输入输出 | `src/terminal.ts` 每个会话独立排队，通过 `src/terminalNative.ts` 调用本机 IPC。xterm 渲染完数据块后确认序号。 |
| 图标与共享样式 | `src/icons.ts`、`src/app.css` |

`tests/` 使用 `node:test` 验证数据处理与客户端状态。启动、类型检查、测试与打包命令统一见[开发指南](../docs/DEVELOPMENT.md)。
