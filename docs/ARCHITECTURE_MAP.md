# MeshShare 架构图谱

## 节点拓扑

```mermaid
flowchart LR
  A["桌面节点 A（Rust + Tauri）"] <-->|mDNS / WebSocket| B["桌面节点 B（Rust + Tauri）"]
  A <-->|HTTP 文件直传| B
  P["网页端浏览器"] -->|HTTP 零安装 / HTTPS 增强| A
  P -.->|文件直传路径| B
  M["监控悬浮条"] -->|本机快照 HTTP/WS| A
```

网页端通过可达的桌面节点进入网页，从该节点读取设备列表，也可以选择当前接入节点作为文件目标。网页端建立临时接收会话后，桌面端可把文件发送到接入节点的临时文件区。HTTP 页面无需安装证书，点接收后文件自动下载到浏览器默认位置，节点在下载流写出后确认完成；安装节点 CA 并打开 HTTPS 页面后，支持选择本地目录并逐项流式写入。网页端发送文件仍使用分块哈希直传。

## 远程管理上下文

```mermaid
sequenceDiagram
  participant U as 管理端主窗口
  participant A as 本机节点 A
  participant B as 目标节点 B
  U->>A: 点击设备图标/名称
  U->>B: GET /api/v1/info、/settings、/peers、/metrics
  U->>B: 连接 /api/v1/ws
  B-->>U: 目标节点状态和事件
  U->>B: PATCH /api/v1/settings 或执行目标传输操作
  B->>B: 校验路径并持久化目标配置
  B-->>U: 返回新状态；离线时按代次重连
```

远程窗口切换只改变 UI 的目标上下文，不转移状态权威，也不累计层级：切换目标直接替换，返回按钮或目标设备列表中的本机节点都回到 A。返回本机时重新使用 A 的本地 API 和 Tauri 命令。目标节点更新或重启后，窗口保留目标标识并恢复轮询和 WebSocket。

远程发送文件时，管理端把清单和文件流提交给 B 的中继接口；B 以自身身份向选定 peer 发送清单，接受后把流转发到 peer，并由 B 保留传输状态。

## 硬件指标同步

```mermaid
sequenceDiagram
  participant C as 节点采集器
  participant A as 节点 A
  participant B as 支持 metrics 的节点 B
  participant U as 本机 UI/悬浮条
  C->>A: 每秒采集 CPU、内存、GPU、显存、网络快照
  A->>U: 本机 MetricsUpdate
  A->>B: 能力过滤后的 MetricsUpdate
  B->>B: 校验来源节点并更新 PeerInfo.metrics
  B-->>U: 仅在本地事件流显示远端指标
```

指标只保留最新快照。远端超过 5 秒没有更新时标记为过期；GPU/显存读取失败不会影响 CPU、内存和网络指标。

## 剪贴板同步

```mermaid
sequenceDiagram
  participant A as 节点 A
  participant B as 节点 B
  A->>A: 系统剪贴板产生变化
  A->>A: 按内容去重并写入本机历史
  A->>B: WebSocket ClipboardUpdate
  B->>B: 去重、比较 HLC
  B->>B: 写入本机历史
  B->>B: 仅较新事件更新快照并尝试写入系统剪贴板
  B-->>A: 转发事件，由 A 去重
  B->>B: 抑制系统剪贴板回读产生的重复事件
```

历史索引保存在各自节点的数据目录，默认按最近 50 条或 30 天保留，桌面端可在 1–500 条和 1–365 天范围内配置。历史不通过 WebSocket 批量同步；新连接仍只交换当前快照。网页读取列表、详情、恢复和清空都通过本机 HTTP API 完成。

## 文件直传

以下是接收方完成确认时的传输流程。任务状态由 Rust 运行时维护，窗口重载后通过任务查询恢复。

```mermaid
sequenceDiagram
  participant S as 发送方
  participant R as 接收方
  actor U as 接收方用户
  S->>R: POST transfer offer(manifest)
  R-->>U: 提示接收请求
  U->>R: 本机 Tauri 命令确认并选择保存目录
  S->>R: 查询任务状态
  R-->>S: 已接受
  S->>R: PUT transfer item(stream)
  R->>R: 检查路径、临时文件、SHA-256、不可覆盖提交
  R-->>S: progress/status
```

网页接收流程使用同一节点的临时文件区：

```mermaid
sequenceDiagram
  participant P as 网页端
  participant R as 接入节点
  participant U as 桌面端
  P->>R: 创建并续期 MobileSession
  P->>R: 获取网页 HTTP/HTTPS 地址和 CA 信息
  alt HTTPS 增强模式
    P->>P: 安装 CA 并选择本地存储目录
  else HTTP 零安装模式
    P->>P: 使用浏览器默认下载位置
  end
  U->>R: 创建网页下载清单
  P->>R: 确认接收
  U->>R: PUT 文件流并校验
  P->>R: GET 文件流下载
  P->>R: POST 项目完成确认
  R->>R: 清理已完成暂存文件；会话过期后清理剩余临时文件
```
