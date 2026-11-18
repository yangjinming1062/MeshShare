# MeshShare 前端

Svelte/TypeScript 界面，作为节点 HTTP/WebSocket 接口的客户端，覆盖桌面窗口、浏览器页面和安卓 App 三种形态。

## 开发流程

- `npm install` 安装依赖；`npm run dev` 启动 Vite 开发服务（仅绑定 127.0.0.1）。
- `npm run check` 跑 svelte-check 类型与模板检查；`npm run build` 产出 `dist/` 供节点静态托管与桌面打包。
- `npm test` 用 node:test 直接运行 `tests/*.test.ts`（类型剥离，不经过打包器）。
- 提交前 check、build、test 三项全部通过。

## 运行形态

- 入口统一为 `src/main.ts` → `App.svelte`，按窗口类型渲染不同界面：Tauri 桌面窗口是完整控制台，浏览器页面是手机临时加入界面，`?window=monitor` 是只读监控窗。
- 形态判定集中在 `App.svelte` 顶部；组件接收 props 或使用共享工具模块，不自行探测运行环境。
- 手机界面由 `components/MobileApp.svelte` 承担，发送、接收逻辑与桌面共用，通过回调 props 注入。
- 原生形态（桌面窗口、安卓 App）把源文件交给本机节点暂存，之后的上传由节点负责，不依赖页面存活；浏览器形态由页面直接向目标节点上传文件体。

## 与节点的交互

- HTTP 调用全部经过 `src/api.ts`：`request` 统一处理 JSON 编解码、`ApiError`（携带状态码）、拒绝重定向和禁用缓存；`initApi` 决定基础地址（Tauri 桥或页面来源），远程管理用 `setApiTarget` 切换目标节点。
- WebSocket 消息与线协议的类型一一对应，集中在 `src/types.ts`，wire 字段命名保持 snake_case。
- 创建传输清单时不算摘要，`items[].sha256` 留空，由节点写入文件体时计算回填；下载校验以记录里的摘要为准（`src/sha256.ts`、`src/mobileSave.ts`）。
- 错误分支只按状态码和错误类型（`instanceof ApiError`）判断，不匹配错误文案。

## 代码结构

- `App.svelte`：状态汇聚与业务编排（传输、剪贴板、设备、设置、远程管理）。可独立测试的纯逻辑放到同级 `.ts` 模块，组件只保留编排与展示。
- `components/`：面板与对话框等视图组件，通过 props/回调与 `App.svelte` 协作；请求统一走 `api.ts`，组件内不拼 fetch。
- `peers.ts`：对端地址归一、白名单校验与候选排序；网页形态的同源代理上下文以参数传入，函数本身保持纯函数。
- `drop.ts`：拖放文件收集，目录递归展开并保留相对路径。
- `format.ts`：状态文案、大小、进度等纯展示格式化。
- `clipboardSearch.ts`：剪贴板历史的模糊搜索与排序，按字符子序列匹配，保持纯函数。
- `sha256.ts` / `mobileSave.ts`：流式 SHA-256 与手机端保存、下载校验。
- `icons.ts` / `app.css`：图标路径表与全局样式。
- `tests/`：node:test 测试纯函数模块，断言风格跟随现有用例。

## 约定

- 注释用中文，只写维护所需的约束、原因与副作用，不复述代码，不加注释分隔横幅。
- 能一行表达的逻辑不拆多行；取值加判断优先合并成单条条件。
- 接口可不兼容演进，调整时同步改调用方；无调用方的 props、参数、图标、样式直接删除，不留兼容层。
