# MeshShare v1 协议

## HTTP 接口

| 方法 | 路径 | 用途 |
| --- | --- | --- |
| `GET` | `/api/v1/info` | 节点身份、应用版本、能力、端口和访问地址 |
| `GET` | `/api/v1/peers` | 当前在线设备 |
| `GET` | `/api/v1/metrics` | 本机最新硬件指标快照 |
| `GET` | `/api/v1/clipboard/status` | 本机系统剪贴板可用性和重试状态 |
| `GET` | `/api/v1/settings` | 节点设置、更新状态、监控能力和剪贴板状态 |
| `PATCH` | `/api/v1/settings` | 修改节点设置；目录路径由目标节点校验 |
| `POST` | `/api/v1/settings/update-check` | 请求目标节点立即检查更新 |
| `GET` | `/api/v1/mobile/access` | 网页 HTTP/HTTPS 地址和 CA 指纹 |
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
| `POST` | `/api/v1/transfers/{id}/accept` | 用目标节点上的绝对路径接受任务 |
| `POST` | `/api/v1/transfers/{id}/reject` | 拒绝任务 |
| `POST` | `/api/v1/transfers/{id}/cancel` | 取消任务 |
| `POST` | `/api/v1/transfers/{id}/fail` | 发送端重试耗尽后标记失败 |
| `POST` | `/api/v1/transfers/{id}/retry` | 重新打开可恢复的失败任务 |
| `DELETE` | `/api/v1/transfers/{id}` | 移除失败任务记录 |
| `GET` | `/api/v1/transfers/{id}/status` | 查询进度 |
| `PUT` | `/api/v1/transfers/{id}/items/{item_id}` | 直接写入单个文件流 |
| `POST` | `/api/v1/relay/transfers` | 远程管理界面在目标节点创建由目标节点拥有的发送任务 |
| `PUT` | `/api/v1/relay/transfers/{id}/items/{item_id}` | 远程管理界面把文件流交给目标节点，再由目标节点转发给其 peer |
| `POST` | `/api/v1/relay/transfers/{id}/cancel` | 取消目标节点拥有的中继发送任务并通知目标 peer |
| `POST` | `/api/v1/relay/transfers/{id}/fail` | 中继发送端重试耗尽后标记失败并通知目标 peer |
| `POST` | `/api/v1/relay/transfers/{id}/retry` | 重新打开中继失败任务并通知目标 peer |
| `POST` | `/api/v1/mobile/sessions` | 创建网页接收会话 |
| `GET` | `/api/v1/mobile/sessions` | 查询本机活跃网页会话 |
| `POST` | `/api/v1/mobile/transfers` | 为网页会话创建待下载清单 |
| `GET` | `/api/v1/mobile/transfers?session_id=...` | 查询网页会话的下载任务并续期会话 |
| `POST` | `/api/v1/mobile/transfers/{id}/accept` | 网页端确认接收文件 |
| `POST` | `/api/v1/mobile/transfers/{id}/reject` | 网页端拒绝文件 |
| `POST` | `/api/v1/mobile/transfers/{id}/cancel` | 网页端取消未完成或待下载文件 |
| `POST` | `/api/v1/mobile/transfers/{id}/fail` | 桌面端上传无法恢复时标记失败 |
| `DELETE` | `/api/v1/mobile/transfers/{id}` | 移除失败的网页传输记录并清理暂存目录 |
| `PUT` | `/api/v1/mobile/transfers/{id}/items/{item_id}` | 桌面端把文件流写入网页端临时文件区 |
| `GET` | `/api/v1/mobile/transfers/{id}/items/{item_id}/download?session_id=...` | 网页端按流下载暂存文件。`auto_complete=true` 在响应提供全部内容时确认，暂存文件仍可供重试 |
| `POST` | `/api/v1/mobile/transfers/{id}/items/{item_id}/complete` | 网页端完成本地写入后确认单个项目 |
| `GET` | `/api/v1/mobile/peers/{peer_id}/api/v1/info` | HTTPS 网页通过接入节点核对目标设备标识 |
| `POST/GET/PUT` | `/api/v1/mobile/peers/{peer_id}/api/v1/transfers/...` | HTTPS 网页通过接入节点代理访问其他桌面节点的传输接口，避免浏览器混合内容和跨节点证书问题 |

接口业务错误使用 `{ "error": "..." }`。`GET /api/v1/clipboard` 查询返回最新快照；历史列表只返回来源、时间、内容类型和截断预览，详情接口才返回完整内容，避免一次加载全部历史图片或大文本。历史接口只访问目标节点本机保存的记录，不修改节点间 WebSocket 的最新快照交换。`GET/PATCH /api/v1/settings` 供桌面管理界面读取和修改目标节点设置；自动更新、开机自启和监控操作在目标节点本机执行。`POST /api/v1/transfers/{id}/accept` 的 `destination` 必须是目标节点上已存在且通过目录校验的路径，管理端不会把本机文件选择器的路径当作远程路径。文件接口按传输任务查询状态。只有 `failed` 记录可以被删除：删除后该任务立即从节点状态移除，后续 `GET` 列表不再返回；记录不存在时返回 404，任务不处于失败状态时返回 409。网页端选择的目录只保留在浏览器的 `FileSystemDirectoryHandle` 中，不通过接口发送路径。

普通直传清单中的 `target_node_id` 必须等于接收节点的标识，地址指向其他节点时返回 421。地址回退通过 `GET /api/v1/info` 核对端点身份，后续状态查询、上传和重试共用已验证的地址；已验证的 WebSocket 端点优先，其他受限私网候选地址在有限探测预算内按顺序尝试。节点正在安装更新时，失败任务的重试返回 425。

桌面主窗口可以直接连接在线节点的 HTTP 和 WebSocket 地址，在同一窗口切换管理目标。目标节点通过 `NodeInfo.capabilities` 声明远程管理能力；缺少能力标记或返回接口不存在时，界面保留本机上下文并提示目标节点需要升级。远程管理继续沿用受信任私网和 Tailscale 网络的无账号模型，不应通过端口转发暴露到互联网。

远程管理目标应声明 `management.remote.v1`；目标节点的远程文件中继还需声明 `transfer.relay.v1`。中继创建请求中的 `source_node_id` 必须是目标节点自身，`target_node_id` 必须是目标节点已连接的 peer；目标节点先通过现有 WebSocket 发送清单，目标设备接受后才转发文件流，并沿用普通传输的路径、哈希、状态和取消校验。

## WebSocket 消息

消息使用 `{ "type": "VariantName", "payload": ... }`。结构以 `backend/src/model.rs` 中的 `WireMessage` 为准。

- `Hello` 携带节点身份和能力。
- `Hello` 同时携带 `protocol_version`；版本不一致的节点不会建立会话。
- `Ping` 和 `Pong` 用于连接保活。
- `ClipboardUpdate` 的 `payload.event` 包含事件 ID、原始来源节点、HLC 和剪贴板内容。接收端用当前 WebSocket Peer Channel 确认发送方；多跳转发时原始来源节点可以与当前发送方不同。
- `ClipboardSnapshot` 的 `payload.snapshot` 用于新连接交换最新剪贴板快照，`source_node_id` 保留原始产生事件的节点，接收端仍按 HLC 比较新旧并按事件 ID 去重。
- `TransferOffer` 的 `payload.offer` 包含传输清单、总大小和发送节点。
- `TransferAction` 表示接受、拒绝、取消、失败或重试，可携带失败原因和重试代次；接收目录只由接收节点本地选择，不通过节点消息传输。
- `TransferProgress` 的 `payload.progress` 包含传输状态、已完成字节、错误和可选的 `retry_count`。重试代次递增后才允许失败任务回到活动状态，旧失败消息不能覆盖新进度。
- `MetricsUpdate` 的 `payload.metrics` 包含节点最新硬件快照。

节点在 `Hello` 的 `capabilities` 中声明 `clipboard.text` 时接收剪贴板事件并允许写入本机系统剪贴板；声明 `clipboard.view` 时只接收并展示最新快照，不写入系统剪贴板。发送端会跳过明确未声明这两项能力的对端。Android 声明 `clipboard.view`，所以可以在共享剪贴板面板查看桌面内容，但不会自动回写系统剪贴板；Android 通过本地 HTTP 接口主动发布的文字仍会发送给声明 `clipboard.text` 的桌面节点。

桌面和浏览器界面也使用该 WebSocket 接收本节点事件，但不作为节点 Peer 发送 `Hello`。界面连接建立后立即发送 `Ping`，之后每 15 秒发送一次；节点直接返回 `Pong` 并保持界面流，不会把界面注册为对端设备。没有发送界面心跳且未完成节点握手的普通连接仍会在握手超时后关闭。

接收端只接受与当前清单总量一致且进度不回退的 `TransferProgress`；同一重试代次内终态不会被旧消息覆盖，失败任务只有通过递增重试代次才能恢复。

剪贴板事件 ID 仍在节点内存中去重十分钟；历史同时按内容哈希去重并持久化。节点重启后从本机历史和当前快照文件恢复，不会从其他节点批量拉取历史。

## 硬件指标

`HardwareMetrics` 只包含当前快照：`node_id`、`sampled_at_ms`、系统 CPU 使用率、内存已用/总量、网络下行/上行字节速率和逐 GPU 的利用率、显存已用/总量。显存只统计独立显存，共享显存与统一内存不折算进去，没有独立显存的核显因此字段为空：Windows 读取 GPU 性能计数器的独立占用并用注册表记录的独立容量，Linux 逐卡读取 sysfs 的 VRAM，并通过 PCI 设备信息填充人类可读型号；PCI 数据库把多个 AMD Navi 31 变体合并命名时，在显存容量能唯一对应型号的情况下进一步细化名称，缺少 `lspci` 时回退到驱动名；macOS 读取 `spdisplays_vram`。Windows 的性能计数器只有适配器 LUID 维度、没有每卡容量，因此聚合成一条记录；Linux 与 macOS 按卡分别上报。`memory_kind` 只是平台内存类型提示；平台没有对应能力时字段为空并附带 `unavailable_reason`。Android 节点只填充网络上下行速率，CPU、内存和 GPU 字段为空。

`PeerInfo.metrics_received_at_ms` 是本地收到远端快照的时间。`metrics_stale` 由服务端按 5 秒阈值计算，供 UI 区分尚未采集和已过期状态。

节点通过 `Hello` 中的 `payload.node.capabilities` 声明本机指标和查看对端指标的能力：`metrics.system`、`metrics.network`、`metrics.gpu` 表示可提供对应本机字段，`metrics.view.system`、`metrics.view.gpu` 表示希望接收对应对端字段。发送端仅向声明 `metrics.system` 或 `metrics.network`，或声明相应 `metrics.view.*` 的在线对端发送 `MetricsUpdate`；对端未声明系统指标能力时，CPU 和内存字段为空；未声明 GPU 指标能力时，GPU 列表为空；未声明 `metrics.network` 时，上下行速率为空。接收端校验快照中的 `node_id` 与当前 WebSocket 对端一致，然后更新本地 `PeerInfo.metrics` 并通知本地界面。Android 只发布网络字段，但通过 `metrics.view.system` 和 `metrics.view.gpu` 接收桌面完整快照。

节点以约一秒的周期生成硬件快照；GPU 采集需要调用系统性能计数器，实际间隔可能略长。

剪贴板文字和 PNG 图片单项均不超过 16 MiB，图片解码还限制为不超过 8192×8192 像素和 128 MiB 解码分配；历史默认最多保留 50 条或 30 天，桌面端可分别配置到 1–500 条或 1–365 天，当前版本不设置额外总容量上限。事件 ID、来源节点 ID 和来源名称也有长度上限，接收端会在写入当前快照、历史索引或去重表前校验这些边界。

传输进度的错误和目标字段、节点地址元数据以及硬件指标中的 GPU 名称和不可用原因均有长度或数量上限；超出边界的远端消息会被忽略。

`TransferStatus::ready` 只用于网页接收：所有清单项目已经在接入节点的临时文件区校验完成，可以由网页端下载。

## 传输安全约束

`relative_path` 必须是目标目录下的相对路径，禁止盘符、分隔符穿越、`.` 和 `..`，同一清单不能出现归一化后的重复路径；传输清单和内容没有固定业务字节数或项目数上限，但仍受控制面解析预算、网页端清单聚合内存预算、`u64` 溢出检查、可用磁盘安全余量和运行时状态清理约束。运行时最多保留 `MAX_TRANSFER_RECORDS` 条桌面传输记录，优先淘汰终态记录；失败记录最长保留 24 小时且失败记录数量受限，完成、拒绝和取消仍按较短保留期清理。接收端在确认传输存在后逐级检查目录，拒绝符号链接；同一传输项目使用锁避免并发写入。接收端先在最终文件同目录创建随机 `.part` 文件并登记路径，校验 SHA-256 和清单大小后用不可覆盖的硬链接提交为最终文件名；Android 应用专属 Download 存储不支持硬链接时，优先使用系统提供的不可覆盖重命名原语，旧内核或不支持该原语的文件系统则在节点进程锁内重新确认目标不存在后重命名，避免不同传输任务互相覆盖。节点启动时清理仍登记的遗留 `.part` 文件。流读取、写入、校验或提交报错，以及上传请求在处理过程中被取消时，接收端都会尝试清理临时文件；提交成功后才移除登记。WebSocket 传入的传输清单和剪贴板图片也执行同样的大小、哈希、路径和 PNG 校验。

网页接收使用接入节点上的短期会话。桌面端先把文件直接上传到该节点的临时文件区。HTTP 页面可以逐项流式下载到浏览器默认位置，响应的 `Content-Disposition` 同时提供 ASCII `filename` 回退和 UTF-8 `filename*`，并过滤路径分隔符与控制字符。带 `auto_complete=true` 的 GET 在节点提供完整响应时确认项目，空文件在响应准备好时确认。确认表示节点已提供下载。浏览器另行报告本地保存结果。会话列表中的 `retryable_download_item_ids` 标记仍可重新下载的项目，暂存文件随会话到期、终态任务回收或显式确认清理。具备安全上下文的 HTTPS 页面可以选择本地目录，校验并流式写入后调用 `complete` 清理该项目的暂存文件。Android App 在受信任局域网内直接使用普通节点传输接口和本地文件选择器。HTTP 下载依赖受信任局域网。上传端按已完成项目断点重试，服务端对已暂存项目幂等返回，连续失败达到上限后通过 `fail` 接口标记任务失败。网页端也可以通过 `cancel` 取消未完成或待下载任务。会话空闲 90 秒后后台清理任务和临时目录，活动传输超过五分钟没有进展也会自动失败并清理临时目录。网页 HTTPS 访问使用节点 CA 签发的证书。桌面端通过 `/api/v1/mobile/access` 提供安全地址、证书下载地址和指纹。HTTPS 网页通过同源代理向其他桌面节点流式上传文件。

桌面发送端上传单个项目遇到网络中断、请求超时或服务端暂时不可用时，会在保持 `accepted`/`uploading` 状态的前提下按项目退避重试；连续达到客户端重试上限后通过 `fail` 接口标记失败。失败任务可以通过递增重试代次重新打开，保留接收端已经提交的项目并依靠重复 PUT 幂等继续上传。接收端目录失效时重试会回到等待确认，避免使用未经重新确认的路径。

## 网络边界

节点只绑定本机回环、RFC1918 私网、链路本地和 Tailscale 组网 IPv4 地址。节点信息和浏览器访问接口公布当前已成功监听的非回环地址，网络变化后更新，允许返回空列表。HTTP 和 WebSocket 使用明文连接，无账号和令牌；跨来源管理请求只接受本机 Tauri/Android 来源、私网或 Tailscale 地址来源，明显来自公网网站的写请求会被拒绝。服务面向受信任的局域网和组网环境，应避免通过端口转发暴露到互联网。

Android App 使用明文 HTTP/WebSocket 连接本机和局域网节点，安装包允许局域网明文流量，以绕过浏览器对本地 HTTPS 证书和安全上下文的额外要求。该设置不提供认证或加密，不能用于公网网络。

Android 检测到带默认路由的全隧道 VPN 且存在 Wi‑Fi 或以太网时，会把 MeshShare 进程的新建 socket 绑定到非 VPN 的物理网络，以保持 mDNS、HTTP 和 WebSocket 的局域网可达性；检测到 Tailscale 的 `100.64.0.0/10` 或 `fd7a:115c:a1e0::/48` 地址/路由时保留 Tailscale 网络。VPN 或 Wi‑Fi 切换会触发监听器和 mDNS 重建；没有物理局域网、VPN 锁定非 VPN 流量或系统拒绝进程绑定时，节点保留系统默认网络，局域网发现可能为空。
