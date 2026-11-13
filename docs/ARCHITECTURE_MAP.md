# MeshShare 架构图谱

## 节点拓扑

```mermaid
flowchart LR
  A["桌面节点 A（Rust + Tauri）"] <-->|mDNS / WebSocket| B["桌面节点 B（Rust + Tauri）"]
  A <-->|HTTP 文件直传| B
  P["手机浏览器"] -->|扫码 HTTP/WS| A
  P -.->|文件直传路径| B
  M["监控悬浮条"] -->|本机快照 HTTP/WS| A
```

手机通过可达的桌面节点进入网页，从该节点读取设备列表。文件传输面向目标节点。虚线表示移动端文件发送路径，默认 HTTP 页面会在发起传输前受浏览器安全上下文限制。

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
  A->>B: WebSocket ClipboardUpdate
  B->>B: 去重、比较 HLC
  B->>B: 更新快照、尝试写入系统剪贴板
  B-->>A: 转发事件，由 A 去重
  B->>B: 抑制系统剪贴板回读产生的重复事件
```

## 文件直传

以下是接收方完成确认时的传输流程。当前桌面界面未能正确显示收件确认。

```mermaid
sequenceDiagram
  participant S as 发送方
  participant R as 接收方
  actor U as 接收方用户
  S->>R: POST transfer offer(manifest)
  R-->>U: 提示接收请求
  U->>R: 确认并选择保存目录
  S->>R: 查询任务状态
  R-->>S: 已接受
  S->>R: PUT transfer item(stream)
  R->>R: 临时文件、SHA-256、原子改名
  R-->>S: progress/status
```
