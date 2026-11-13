# MeshShare v1 协议

## HTTP 接口

| 方法 | 路径 | 用途 |
| --- | --- | --- |
| `GET` | `/api/v1/info` | 节点身份、应用版本、能力、端口和访问地址 |
| `GET` | `/api/v1/peers` | 当前在线设备 |
| `GET` | `/api/v1/metrics` | 本机最新硬件指标快照 |
| `POST` | `/api/v1/peers/connect` | 通过手动地址加入节点 |
| `GET` | `/api/v1/clipboard` | 当前内存中的剪贴板快照 |
| `POST` | `/api/v1/clipboard` | 主动发布剪贴板文字或 PNG，移动网页发送文字 |
| `POST` | `/api/v1/transfers` | 创建文件清单和接收确认任务 |
| `POST` | `/api/v1/transfers/{id}/accept` | 接收端确认并提供目录 |
| `POST` | `/api/v1/transfers/{id}/reject` | 拒绝任务 |
| `POST` | `/api/v1/transfers/{id}/cancel` | 取消任务 |
| `GET` | `/api/v1/transfers/{id}/status` | 查询进度 |
| `PUT` | `/api/v1/transfers/{id}/items/{item_id}` | 直接写入单个文件流 |

接口业务错误使用 `{ "error": "..." }`。剪贴板查询返回最新快照，文件接口按传输任务查询状态。

## WebSocket 消息

消息使用 `{ "type": "VariantName", "payload": ... }`。结构以 `src-tauri/src/model.rs` 中的 `WireMessage` 为准。

- `Hello` 携带节点身份和能力。
- `Ping` 和 `Pong` 用于连接保活。
- `ClipboardUpdate` 的 `payload.event` 包含事件 ID、来源节点、HLC 和剪贴板内容。
- `TransferOffer` 的 `payload.offer` 包含传输清单、总大小和发送节点。
- `TransferAction` 表示接受、拒绝或取消。
- `TransferProgress` 的 `payload.progress` 包含传输状态、已完成字节和错误。
- `MetricsUpdate` 的 `payload.metrics` 包含节点最新硬件快照。

剪贴板事件 ID 在节点内存中去重十分钟。节点重启后从空快照开始。

## 硬件指标

`HardwareMetrics` 只包含当前快照：`node_id`、`sampled_at_ms`、系统 CPU 使用率、内存已用/总量、网络下行/上行字节速率和逐 GPU 的利用率、显存已用/总量。`memory_kind` 区分独立显存、统一内存和未知类型；平台没有对应能力时字段为空并附带 `unavailable_reason`。

`PeerInfo.metrics_received_at_ms` 是本地收到远端快照的时间。`metrics_stale` 由服务端按 5 秒阈值计算，供 UI 区分尚未采集和已过期状态。

节点通过 `Hello` 中的 `payload.node.capabilities` 声明 `metrics.system` 和 `metrics.gpu`。发送端仅向声明 `metrics.system` 的在线对端发送 `MetricsUpdate`，以兼容旧节点。对端未声明 `metrics.gpu` 时，发送的 GPU 列表为空。接收端校验快照中的 `node_id` 与当前 WebSocket 对端一致，然后更新本地 `PeerInfo.metrics` 并通知本地界面。

节点每秒生成一次内存快照。

## 传输安全约束

`relative_path` 必须是目标目录下的相对路径，禁止盘符和 `..`。接收端先在最终文件同目录写入随机 `.part` 文件，校验 SHA-256 和清单大小后改为最终文件名。流读取、写入、校验或改名报错时，接收端会尝试清理该临时文件。
