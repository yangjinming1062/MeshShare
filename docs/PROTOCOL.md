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
| `GET` | `/api/v1/transfers` | 查询本节点的传输任务，可用 `status=waiting` 过滤 |
| `POST` | `/api/v1/transfers/{id}/reject` | 拒绝任务 |
| `POST` | `/api/v1/transfers/{id}/cancel` | 取消任务 |
| `GET` | `/api/v1/transfers/{id}/status` | 查询进度 |
| `PUT` | `/api/v1/transfers/{id}/items/{item_id}` | 直接写入单个文件流 |
| `POST` | `/api/v1/mobile/sessions` | 创建手机接收会话 |
| `GET` | `/api/v1/mobile/sessions` | 查询本机活跃手机会话 |
| `POST` | `/api/v1/mobile/transfers` | 为手机会话创建待下载清单 |
| `GET` | `/api/v1/mobile/transfers?session_id=...` | 查询手机会话的下载任务并续期会话 |
| `POST` | `/api/v1/mobile/transfers/{id}/accept` | 手机确认接收文件 |
| `POST` | `/api/v1/mobile/transfers/{id}/reject` | 手机拒绝文件 |
| `POST` | `/api/v1/mobile/transfers/{id}/fail` | 桌面端上传无法恢复时标记失败 |
| `PUT` | `/api/v1/mobile/transfers/{id}/items/{item_id}` | 桌面端把文件流写入手机临时文件区 |
| `GET` | `/api/v1/mobile/transfers/{id}/items/{item_id}/download?session_id=...` | 手机按流下载已准备好的文件 |

接口业务错误使用 `{ "error": "..." }`。剪贴板查询返回最新快照，文件接口按传输任务查询状态。接收端目录只能由本机 Tauri `accept_transfer` 命令选择并确认。

## WebSocket 消息

消息使用 `{ "type": "VariantName", "payload": ... }`。结构以 `src-tauri/src/model.rs` 中的 `WireMessage` 为准。

- `Hello` 携带节点身份和能力。
- `Hello` 同时携带 `protocol_version`；版本不一致的节点不会建立会话。
- `Ping` 和 `Pong` 用于连接保活。
- `ClipboardUpdate` 的 `payload.event` 包含事件 ID、原始来源节点、HLC 和剪贴板内容。接收端用当前 WebSocket Peer Channel 确认发送方；多跳转发时原始来源节点可以与当前发送方不同。
- `ClipboardSnapshot` 的 `payload.snapshot` 用于新连接交换最新剪贴板快照，`source_node_id` 保留原始产生事件的节点，接收端仍按 HLC 比较新旧并按事件 ID 去重。
- `TransferOffer` 的 `payload.offer` 包含传输清单、总大小和发送节点。
- `TransferAction` 表示接受、拒绝或取消。
- `TransferProgress` 的 `payload.progress` 包含传输状态、已完成字节和错误。
- `MetricsUpdate` 的 `payload.metrics` 包含节点最新硬件快照。

剪贴板事件 ID 在节点内存中去重十分钟。节点重启后从空快照开始。

## 硬件指标

`HardwareMetrics` 只包含当前快照：`node_id`、`sampled_at_ms`、系统 CPU 使用率、内存已用/总量、网络下行/上行字节速率和逐 GPU 的利用率、显存已用/总量。`memory_kind` 区分独立显存、统一内存和未知类型；平台没有对应能力时字段为空并附带 `unavailable_reason`。

`PeerInfo.metrics_received_at_ms` 是本地收到远端快照的时间。`metrics_stale` 由服务端按 5 秒阈值计算，供 UI 区分尚未采集和已过期状态。

节点通过 `Hello` 中的 `payload.node.capabilities` 声明 `metrics.system` 和 `metrics.gpu`。发送端仅向声明 `metrics.system` 的在线对端发送 `MetricsUpdate`；对端未声明 `metrics.gpu` 时，发送的 GPU 列表为空。接收端校验快照中的 `node_id` 与当前 WebSocket 对端一致，然后更新本地 `PeerInfo.metrics` 并通知本地界面。

节点每秒生成一次内存快照。

`TransferStatus::ready` 只用于手机接收：所有清单项目已经在扫码节点的临时文件区校验完成，可以由手机下载。

## 传输安全约束

`relative_path` 必须是目标目录下的相对路径，禁止盘符、分隔符穿越、`.` 和 `..`，同一清单不能出现归一化后的重复路径。接收端在确认传输存在后逐级检查目录，拒绝符号链接；同一传输项目使用锁避免并发写入。接收端先在最终文件同目录创建随机 `.part` 文件，校验 SHA-256 和清单大小后用不可覆盖的硬链接提交为最终文件名。流读取、写入、校验或提交报错时，接收端会尝试清理临时文件。WebSocket 传入的传输清单和剪贴板图片也执行同样的大小、哈希、路径和 PNG 校验。

手机接收使用扫码节点上的短期会话。桌面端先把文件直接上传到该节点的临时文件区，手机确认后从同一节点通过流式 GET 下载；上传端按已完成项目断点重试，服务端对已暂存项目幂等返回，连续失败达到上限后通过 `fail` 接口标记任务失败；每个清单项目在流式下载完成后记录完成，全部项目下载后任务进入 `completed`。会话空闲 90 秒后后台清理任务和临时目录，节点启动时清理遗留目录。每个会话按活动传输清单总大小预留 5 GiB 临时空间，拒绝或取消的任务立即释放配额，失败任务只按已暂存字节保留，已准备好的任务继续保留完整清单大小；超过配额的新清单会被拒绝。

## 网络边界

节点只绑定本机回环、RFC1918 私网、链路本地和 Tailscale 组网 IPv4 地址，并只公布实际成功监听的地址。服务使用明文 HTTP/WebSocket、无账号和无令牌，CORS 保持开放；产品边界是受信任局域网/组网，不应通过端口转发暴露到互联网。
