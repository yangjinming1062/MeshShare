# MeshShare v1 协议

## HTTP 接口

| 方法 | 路径 | 用途 |
| --- | --- | --- |
| `GET` | `/api/v1/info` | 节点身份、应用版本、能力、端口和访问地址 |
| `GET` | `/api/v1/peers` | 当前在线设备 |
| `GET` | `/api/v1/metrics` | 本机最新硬件指标快照 |
| `GET` | `/api/v1/mobile/access` | 网页引导地址、HTTPS 地址和 CA 指纹 |
| `GET` | `/api/v1/mobile/ca` | 下载接入节点的本地 CA 证书 |
| `POST` | `/api/v1/peers/connect` | 通过手动地址加入节点 |
| `GET` | `/api/v1/clipboard` | 当前共享剪贴板快照 |
| `POST` | `/api/v1/clipboard` | 主动发布剪贴板文字或 PNG，网页端发送文字 |
| `GET` | `/api/v1/clipboard/history` | 查询本机剪贴板历史元数据和当前保留设置 |
| `GET` | `/api/v1/clipboard/history/{event_id}` | 查询单条历史记录的完整文字/PNG 内容 |
| `POST` | `/api/v1/clipboard/history/{event_id}/restore` | 恢复单条本机历史并发布新的剪贴板事件 |
| `DELETE` | `/api/v1/clipboard/history` | 清空本机历史，保留当前共享快照 |
| `POST` | `/api/v1/transfers` | 创建文件清单和接收确认任务 |
| `GET` | `/api/v1/transfers` | 查询本节点的传输任务，可用 `status=waiting` 过滤 |
| `POST` | `/api/v1/transfers/{id}/reject` | 拒绝任务 |
| `POST` | `/api/v1/transfers/{id}/cancel` | 取消任务 |
| `GET` | `/api/v1/transfers/{id}/status` | 查询进度 |
| `PUT` | `/api/v1/transfers/{id}/items/{item_id}` | 直接写入单个文件流 |
| `POST` | `/api/v1/mobile/sessions` | 创建网页接收会话 |
| `GET` | `/api/v1/mobile/sessions` | 查询本机活跃网页会话 |
| `POST` | `/api/v1/mobile/transfers` | 为网页会话创建待下载清单 |
| `GET` | `/api/v1/mobile/transfers?session_id=...` | 查询网页会话的下载任务并续期会话 |
| `POST` | `/api/v1/mobile/transfers/{id}/accept` | 网页端确认接收文件 |
| `POST` | `/api/v1/mobile/transfers/{id}/reject` | 网页端拒绝文件 |
| `POST` | `/api/v1/mobile/transfers/{id}/cancel` | 网页端取消未完成或待下载文件 |
| `POST` | `/api/v1/mobile/transfers/{id}/fail` | 桌面端上传无法恢复时标记失败 |
| `PUT` | `/api/v1/mobile/transfers/{id}/items/{item_id}` | 桌面端把文件流写入网页端临时文件区 |
| `GET` | `/api/v1/mobile/transfers/{id}/items/{item_id}/download?session_id=...` | 网页端按流下载已准备好的文件 |
| `POST` | `/api/v1/mobile/transfers/{id}/items/{item_id}/complete` | 网页端完成本地写入后确认单个项目 |
| `POST/GET/PUT` | `/api/v1/mobile/peers/{peer_id}/api/v1/transfers/...` | HTTPS 网页通过接入节点代理访问其他桌面节点的传输接口，避免浏览器混合内容和跨节点证书问题 |

接口业务错误使用 `{ "error": "..." }`。`GET /api/v1/clipboard` 查询返回最新快照；历史列表只返回来源、时间、内容类型和截断预览，详情接口才返回完整内容，避免一次加载全部历史图片或大文本。历史接口只访问接入节点本机保存的记录，不修改节点间 WebSocket 的最新快照交换。文件接口按传输任务查询状态。桌面接收端目录只能由本机 Tauri `accept_transfer` 命令选择并确认；自动接收使用节点本地配置的目录。网页端选择的目录只保留在浏览器的 `FileSystemDirectoryHandle` 中，不通过接口发送路径。

## WebSocket 消息

消息使用 `{ "type": "VariantName", "payload": ... }`。结构以 `backend/src/model.rs` 中的 `WireMessage` 为准。

- `Hello` 携带节点身份和能力。
- `Hello` 同时携带 `protocol_version`；版本不一致的节点不会建立会话。
- `Ping` 和 `Pong` 用于连接保活。
- `ClipboardUpdate` 的 `payload.event` 包含事件 ID、原始来源节点、HLC 和剪贴板内容。接收端用当前 WebSocket Peer Channel 确认发送方；多跳转发时原始来源节点可以与当前发送方不同。
- `ClipboardSnapshot` 的 `payload.snapshot` 用于新连接交换最新剪贴板快照，`source_node_id` 保留原始产生事件的节点，接收端仍按 HLC 比较新旧并按事件 ID 去重。
- `TransferOffer` 的 `payload.offer` 包含传输清单、总大小和发送节点。
- `TransferAction` 表示接受、拒绝或取消，可携带失败原因；接收目录只由接收节点本地选择，不通过节点消息传输。
- `TransferProgress` 的 `payload.progress` 包含传输状态、已完成字节和错误。
- `MetricsUpdate` 的 `payload.metrics` 包含节点最新硬件快照。

接收端只接受与当前清单总量一致且进度不回退的 `TransferProgress`；终态不会被旧消息覆盖。

剪贴板事件 ID 仍在节点内存中去重十分钟；历史同时按内容哈希去重并持久化。节点重启后从本机历史和当前快照文件恢复，不会从其他节点批量拉取历史。

## 硬件指标

`HardwareMetrics` 只包含当前快照：`node_id`、`sampled_at_ms`、系统 CPU 使用率、内存已用/总量、网络下行/上行字节速率和逐 GPU 的利用率、显存已用/总量。显存只统计独立显存，共享显存与统一内存不折算进去，没有独立显存的核显因此字段为空：Windows 读取 GPU 性能计数器的独立占用并用注册表记录的独立容量，Linux 逐卡读取 sysfs 的 VRAM，macOS 读取 `spdisplays_vram`。Windows 的性能计数器只有适配器 LUID 维度、没有每卡容量，因此聚合成一条记录；Linux 与 macOS 按卡分别上报。`memory_kind` 只是平台内存类型提示；平台没有对应能力时字段为空并附带 `unavailable_reason`。

`PeerInfo.metrics_received_at_ms` 是本地收到远端快照的时间。`metrics_stale` 由服务端按 5 秒阈值计算，供 UI 区分尚未采集和已过期状态。

节点通过 `Hello` 中的 `payload.node.capabilities` 声明 `metrics.system` 和 `metrics.gpu`。发送端仅向声明 `metrics.system` 的在线对端发送 `MetricsUpdate`；对端未声明 `metrics.gpu` 时，发送的 GPU 列表为空。接收端校验快照中的 `node_id` 与当前 WebSocket 对端一致，然后更新本地 `PeerInfo.metrics` 并通知本地界面。

节点以约一秒的周期生成硬件快照；GPU 采集需要调用系统性能计数器，实际间隔可能略长。

剪贴板文字和 PNG 图片单项均不超过 16 MiB，图片解码还限制为不超过 8192×8192 像素和 128 MiB 解码分配；历史默认最多保留 50 条或 30 天，桌面端可分别配置到 1–500 条或 1–365 天，当前版本不设置额外总容量上限。事件 ID、来源节点 ID 和来源名称也有长度上限，接收端会在写入当前快照、历史索引或去重表前校验这些边界。

传输进度的错误和目标字段、节点地址元数据以及硬件指标中的 GPU 名称和不可用原因均有长度或数量上限；超出边界的远端消息会被忽略。

`TransferStatus::ready` 只用于网页接收：所有清单项目已经在接入节点的临时文件区校验完成，可以由网页端下载。

## 传输安全约束

`relative_path` 必须是目标目录下的相对路径，禁止盘符、分隔符穿越、`.` 和 `..`，同一清单不能出现归一化后的重复路径；传输清单和内容没有固定业务字节数或项目数上限，但仍受控制面解析预算、网页端清单聚合内存预算、`u64` 溢出检查、可用磁盘安全余量和运行时状态清理约束。运行时最多保留 `MAX_TRANSFER_RECORDS` 条桌面传输记录，优先淘汰终态记录。接收端在确认传输存在后逐级检查目录，拒绝符号链接；同一传输项目使用锁避免并发写入。接收端先在最终文件同目录创建随机 `.part` 文件并登记路径，校验 SHA-256 和清单大小后用不可覆盖的硬链接提交为最终文件名；节点启动时清理仍登记的遗留 `.part` 文件。流读取、写入、校验或提交报错时，接收端会尝试清理临时文件。WebSocket 传入的传输清单和剪贴板图片也执行同样的大小、哈希、路径和 PNG 校验。

网页接收使用接入节点上的短期会话。桌面端先把文件直接上传到该节点的临时文件区，网页端选择本地目录并确认后从同一节点通过流式 GET 下载；网页端校验并关闭本地文件后调用 `complete`，节点随后清理对应暂存文件。上传端按已完成项目断点重试，服务端对已暂存项目幂等返回，连续失败达到上限后通过 `fail` 接口标记任务失败；网页端也可以通过 `cancel` 取消未完成或待下载任务。会话空闲 90 秒后后台清理任务和临时目录，活动传输超过五分钟没有进展也会自动失败并清理临时目录，节点启动时清理遗留目录。网页 HTTPS 访问使用节点 CA 签发的证书；桌面端通过 `/api/v1/mobile/access` 提供安全地址、证书下载地址和指纹。HTTPS 网页向其他桌面节点发送文件时由接入节点以同源接口代理到目标节点，上传流仍保持流式传输。

## 网络边界

节点只绑定本机回环、RFC1918 私网、链路本地和 Tailscale 组网 IPv4 地址。节点信息和浏览器访问接口公布当前已成功监听的非回环地址，网络变化后更新，允许返回空列表。HTTP 和 WebSocket 使用明文连接，无账号和令牌，CORS 保持开放。服务面向受信任的局域网和组网环境，应避免通过端口转发暴露到互联网。
