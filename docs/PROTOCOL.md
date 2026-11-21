# MeshShare v1 协议

## HTTP 接口

业务错误使用 `{ "error": "..." }`。除特别说明外，接口只面向受信任私网。

### 节点与设置

| 方法 | 路径 | 用途 |
| --- | --- | --- |
| `GET` | `/api/v1/info` | 节点身份、版本、能力、端口和访问地址 |
| `GET` | `/api/v1/peers` | 当前在线设备 |
| `GET` | `/api/v1/metrics` | 本机最新硬件快照 |
| `GET` | `/api/v1/clipboard/status` | 本机剪贴板可用性和重试状态 |
| `GET` | `/api/v1/settings` | 设置、更新状态、监控能力和剪贴板状态 |
| `PATCH` | `/api/v1/settings` | 修改设置；目录路径由目标节点校验 |
| `POST` | `/api/v1/settings/update-check` | 让目标节点立即检查更新 |
| `GET` | `/api/v1/mobile/access` | 网页 HTTP/HTTPS 地址和 CA 指纹 |
| `GET` | `/api/v1/mobile/ca` | 下载接入节点的本地 CA |
| `POST` | `/api/v1/peers/connect` | 通过手动地址发起连接 |

设置中的 `monitor.visible` 表示保存的用户显示偏好，不表示窗口当前一定可见。全屏自动隐藏仅在桌面壳内临时生效，不修改该字段；全屏期间设置为 `true` 后仍需等全屏条件解除才显示，设置为 `false` 则解除后也不会自动显示。

### 剪贴板

| 方法 | 路径 | 用途 |
| --- | --- | --- |
| `GET` | `/api/v1/clipboard` | 当前共享快照 |
| `POST` | `/api/v1/clipboard` | 发布文字或 PNG，网页端发送文字 |
| `GET` | `/api/v1/clipboard/history` | 历史元数据和保留设置 |
| `GET` | `/api/v1/clipboard/history/{event_id}` | 单条历史的完整内容 |
| `GET` | `/api/v1/clipboard/history/{event_id}/thumbnail` | 图片历史的 96×72 PNG 缩略图；无图片或记录不存在时返回 404 |
| `POST` | `/api/v1/clipboard/history/{event_id}/restore` | 恢复单条历史并发布新事件 |
| `DELETE` | `/api/v1/clipboard/history` | 清空历史，保留当前快照 |

### 传输

| 方法 | 路径 | 用途 |
| --- | --- | --- |
| `POST` | `/api/v1/transfers` | 创建接收任务并返回接收端状态 |
| `GET` | `/api/v1/transfers` | 查询本机任务 |
| `GET` | `/api/v1/transfers/history` | 最近 10 条终态历史，可用 `session_id` 过滤 |
| `DELETE` | `/api/v1/transfers/history` | 清空历史并同步删除对端配对记录 |
| `POST` | `/api/v1/transfers/{id}/accept` | 用目标节点上的绝对路径接受任务 |
| `POST` | `/api/v1/transfers/{id}/reject` | 拒绝任务 |
| `POST` | `/api/v1/transfers/{id}/cancel` | 取消任务 |
| `POST` | `/api/v1/transfers/{id}/fail` | 发送端重试耗尽后标记失败 |
| `POST` | `/api/v1/transfers/{id}/retry` | 重新打开可恢复的失败任务 |
| `DELETE` | `/api/v1/transfers/{id}` | 移除失败任务记录 |
| `GET` | `/api/v1/transfers/{id}/status` | 查询进度 |
| `PUT` | `/api/v1/transfers/{id}/items/{item_id}` | 写入单个文件流 |
| `POST` | `/api/v1/outgoing` | 把待发送清单交给本机节点暂存（仅回环） |
| `PUT` | `/api/v1/outgoing/{id}/items/{item_id}` | 写入暂存区并校验大小，按写入内容回填 SHA-256（仅回环） |
| `POST` | `/api/v1/outgoing/{id}/start` | 登记出站任务并发送清单（仅回环） |
| `POST` | `/api/v1/outgoing/{id}/cancel` | 取消暂存并清理目录（仅回环） |
| `POST` | `/api/v1/outgoing/paths` | 按原路径读取并计算摘要（仅回环） |
| `GET` | `/api/v1/outgoing/{id}` | 查询准备进度；结束或取消后 404（仅回环） |

传输历史路径属于返回记录的节点。接收记录可携带 `item_root`，表示在所记录接收目录中首个已提交条目的顶层名称。重试更换接收目录时，定位项也以新目录的提交为准。没有定位项、旧记录缺少该字段或定位项已移除时，仍可打开存在的接收目录。

### 远程管理

| 方法 | 路径 | 用途 |
| --- | --- | --- |
| `GET` | `/api/v1/management/files/roots` | 可浏览的起始位置 |
| `GET` | `/api/v1/management/files?path=...` | 列出目标目录一层内容，只返回名称、类型、大小和时间 |
| `POST` | `/api/v1/management/outgoing/paths` | 用目标机器绝对路径建立发送任务 |
| `GET` | `/api/v1/management/outgoing/{id}` | 查询远程准备进度；结束或取消后 404 |
| `POST` | `/api/v1/management/outgoing/{id}/cancel` | 取消远程准备任务 |

### 网页会话

| 方法 | 路径 | 用途 |
| --- | --- | --- |
| `POST` | `/api/v1/mobile/sessions` | 创建网页接收会话 |
| `GET` | `/api/v1/mobile/sessions` | 查询本机活跃会话 |
| `POST` | `/api/v1/mobile/transfers` | 为会话创建待下载清单 |
| `GET` | `/api/v1/mobile/transfers?session_id=...` | 查询下载任务并续期会话 |
| `POST` | `/api/v1/mobile/transfers/{id}/accept` | 网页端确认接收 |
| `POST` | `/api/v1/mobile/transfers/{id}/reject` | 网页端拒绝 |
| `POST` | `/api/v1/mobile/transfers/{id}/cancel` | 取消未完成或待下载任务 |
| `POST` | `/api/v1/mobile/transfers/{id}/fail` | 桌面端上传无法恢复时标记失败 |
| `DELETE` | `/api/v1/mobile/transfers/{id}` | 移除失败记录并清理暂存目录 |
| `PUT` | `/api/v1/mobile/transfers/{id}/items/{item_id}` | 桌面端写入网页临时文件区 |
| `GET` | `/api/v1/mobile/transfers/{id}/items/{item_id}/download?session_id=...` | 网页端流式下载；`auto_complete=true` 在提供全部内容时确认 |
| `POST` | `/api/v1/mobile/transfers/{id}/items/{item_id}/complete` | 网页端本地写入后确认 |
| `GET` | `/api/v1/mobile/peers/{peer_id}/api/v1/info` | HTTPS 网页核对目标设备标识 |
| `POST/GET/PUT` | `/api/v1/mobile/peers/{peer_id}/api/v1/transfers/...` | HTTPS 网页经接入节点代理访问其他桌面节点 |

历史列表只返回来源、时间、内容类型和截断预览，详情接口才返回完整内容；两者都只访问目标节点本机记录，不改变节点间的快照交换。`accept` 的 `destination` 必须是目标节点上已存在且通过目录校验的路径，管理端不会把本机文件选择器的路径当作远程路径。只有 `failed` 记录可以被删除：删除后立即从节点状态移除，记录不存在返回 404，任务不处于失败状态返回 409。历史按结束时间倒序；`session_id` 只匹配网页会话记录，清空只删除历史，不影响活动与失败任务。

普通直传清单中的 `target_node_id` 必须等于接收节点标识，否则返回 421。地址回退通过 `GET /api/v1/info` 核对端点身份，后续状态查询、上传和重试共用已验证地址；已验证的 WebSocket 端点优先，其他受限私网候选在有限预算内依次尝试。节点安装更新期间，失败任务重试返回 425。

远程管理目标应声明 `management.remote.v1`，远程文件选择还需 `management.files.v1`；缺少能力标记或接口不存在时，界面保留本机上下文并提示升级。管理接口只返回目录元数据，路径发送的 `target_node_id` 必须是目标已连接的 peer，发送源固定为目标节点自身。这组接口只在目标机器本机开启 `remote_file_access` 时可用，关闭时统一返回 403；该设置不出现在 `GET/PATCH /api/v1/settings` 中。相对路径返回 400，不可访问路径返回 404。

## WebSocket 消息

消息使用 `{ "type": "VariantName", "payload": ... }`，结构以 `backend/src/model.rs` 中的 `WireMessage` 为准。

- `Hello` 携带节点身份、能力和 `protocol_version`。会话注册要求双方协议版本一致。注册后节点出现在设备列表。
- `Ping` / `Pong` 用于保活。
- `ClipboardUpdate` 的 `payload.event` 包含事件 ID、原始来源节点、HLC 和内容。接收端用当前 Peer Channel 确认发送方；多跳转发时来源可以与当前发送方不同。
- `ClipboardSnapshot` 的 `payload.snapshot` 用于新连接交换最新快照，保留原始 `source_node_id`；接收端按 HLC 比较新旧并按事件 ID 去重。
- `TransferOffer` 的 `payload.offer` 包含清单、总大小和发送节点。
- `TransferAction` 表示接受、拒绝、取消、失败或重试，可携带失败原因和重试代次；接收目录只由接收节点本地选择，不通过消息传输。节点把长时间无进展的传输判为失败时也会发送 `fail`。
- `TransferProgress` 的 `payload.progress` 包含状态、已完成字节、错误和可选的 `retry_count`。只有代次递增才允许失败任务回到活动状态，旧失败消息不能覆盖新进度。传输进行中，节点本地进度与状态接口返回的 `completed_bytes` 还包含当前条目已接收但尚未提交的字节，`completed_items` 仍只统计已提交条目；两者都不回退且不超过总量。
- `MetricsUpdate` 的 `payload.metrics` 包含节点最新硬件快照。
- `TransferHistoryDeleted` 的 `payload.transfer_ids` 请求对端删除配对记录；接收端只接受有限数量，只删除当前 Peer 自己的记录，不反向转发。

发送端的本地出站进度在收到清单回复前为 `waiting`，同时标记 `awaiting_offer_reply: true`。接收节点通过 HTTP 建单响应或 WebSocket 进度返回任务状态，`accepted` 表示已确定接收目录，`waiting` 表示仍需确认。该标记只用于发送端本地的建立阶段，收到任务状态后清除。旧节点缺少该字段时按 `false` 处理。

能力声明决定消息是否发送：`clipboard.text` 表示接收剪贴板事件并允许写入本机系统剪贴板，`clipboard.view` 表示只展示快照；发送端跳过未声明两者的对端。Android 声明 `clipboard.view`，但通过本地 HTTP 主动发布的文字仍会发给声明 `clipboard.text` 的桌面节点。`transfer.history.v1` 表示可处理 `TransferHistoryDeleted`；旧节点反序列化未知消息会关闭连接，因此只对声明该能力的对端发送。对端离线时待删除标识写入节点数据目录，重连后补发；已知不支持时丢弃队列。

桌面和浏览器界面也使用该 WebSocket 接收本节点事件，但不发送 `Hello`。界面连接建立后立即发送 `Ping`，之后每 15 秒一次；节点返回 `Pong` 并保持界面流，不把界面注册为对端设备。没有界面心跳且未完成节点握手的普通连接在超时后关闭。

接收端只接受与当前清单总量一致且进度不回退的 `TransferProgress`；同一重试代次内终态不会被旧消息覆盖。剪贴板事件 ID 在内存去重十分钟，历史按内容哈希去重并持久化；节点重启后从本机历史和当前快照恢复，不从其他节点批量拉取历史。

## 硬件指标

`HardwareMetrics` 只包含当前快照：`node_id`、`sampled_at_ms`、CPU 使用率与型号、逻辑/物理核心数、频率、内存已用/可用/总量、交换分区、系统负载、运行时间、网络上下行速率、磁盘分区容量与可用空间，以及逐 GPU 的利用率、显存用量和类型。磁盘条目包含名称、挂载点、文件系统和可移动标记。新增字段都带默认值，旧节点缺失字段按空值或空列表处理，协议版本不变。

显存只统计独立显存，共享内存与统一内存不折算，没有独立显存的核显字段为空；平台无法读取时附 `unavailable_reason`。Windows 不提供系统负载均值时该字段为空；Android 只填充网络上下行速率。`PeerInfo.metrics_received_at_ms` 是本地收到快照的时间，`metrics_stale` 由服务端按 5 秒阈值计算。

节点通过 `Hello` 的 `capabilities` 声明指标能力：`metrics.system`、`metrics.network`、`metrics.gpu` 表示可提供对应本机字段，`metrics.view.system`、`metrics.view.gpu` 表示希望接收对端字段。发送端只向声明提供或查看相应指标的对端发送 `MetricsUpdate`；对端未声明系统能力时 CPU、内存、交换分区、负载、运行时间和磁盘为空，未声明 GPU 时列表为空，未声明 `metrics.network` 时速率为空。接收端校验快照中的 `node_id` 与当前对端一致后更新本地缓存并通知界面。

节点约每秒生成快照；磁盘容量每个周期刷新，磁盘列表约每十秒刷新；GPU 采集需要调用系统性能计数器，实际间隔可能略长。

剪贴板文字和 PNG 单项均不超过 16 MiB，图片解码不超过 8192×8192 像素和 128 MiB 分配；历史默认保留 50 条或 30 天，可配置为 1–500 条或 1–365 天，payload 默认上限 512 MiB。事件 ID、来源节点 ID 和来源名称有长度上限，接收端在写入快照、历史索引或去重表前校验。进度错误、目标字段、地址元数据和 GPU 名称/不可用原因也有长度或数量上限，超限的远端消息会被忽略。`TransferStatus::ready` 只用于网页接收：清单项目已在接入节点暂存区校验完成。

## 传输安全约束

- **路径。** `relative_path` 必须是目标目录下的相对路径，禁止盘符、分隔符穿越、`.` 和 `..`，同一清单不能出现归一化后的重复路径。清单和内容没有固定业务上限，但受控制面解析预算、网页清单内存预算、`u64` 溢出检查、磁盘安全余量和运行时清理约束。运行时最多保留有限条桌面传输记录，优先淘汰终态；失败记录最长保留 24 小时且数量受限。
- **写入。** 接收端逐级检查目录并拒绝符号链接，同一项目加锁避免并发写入。先在同目录创建随机 `.part` 文件并登记，校验清单大小；清单携带 SHA-256 时核对内容摘要，留空时以写入内容的摘要回填清单（页面创建的清单留空，节点直传的清单必须携带），随后用不可覆盖的硬链接提交为最终文件名；Android 应用专属 Download 不支持硬链接时，优先使用系统提供的不可覆盖重命名原语，旧内核或不支持时在节点进程锁内重新确认目标不存在后重命名。节点启动时清理仍登记的遗留 `.part`；读取、写入、校验或提交报错，以及请求被取消时都会尝试清理，提交成功后才移除登记。
- **网页。** 接入节点持有短期会话，桌面先把文件上传到暂存区。HTTP 页面逐项流式下载，`Content-Disposition` 同时提供 ASCII `filename` 和 UTF-8 `filename*` 并过滤路径分隔符与控制字符；`auto_complete=true` 在节点提供完整响应时确认项目，空文件在响应准备好时确认。`retryable_download_item_ids` 标记仍可重新下载的项目，暂存文件随会话到期、终态任务回收或显式确认清理。HTTPS 页面校验并流式写入本地目录后调用 `complete`，网页选择的目录只保留在浏览器的 `FileSystemDirectoryHandle` 中，不通过接口发送路径；HTTPS 网页经接入节点同源代理向其他桌面节点流式上传。会话空闲 90 秒后后台清理任务和临时目录，活动传输五分钟无进展也会失败并清理。
- **原生态发送。** 源文件交给本机节点（桌面按原路径读取，Android 复制到暂存），对端确认后由该节点上传并负责重试，页面刷新或窗口不在前台不影响。桌面上传前比对源文件大小与修改时间，变化时直接失败。单个项目遇到网络中断、请求超时或服务端暂时不可用时，在保持 `accepted`/`uploading` 的前提下按项目退避重试，达到上限后通过 `fail` 标记失败。失败任务可通过递增重试代次恢复，保留已提交项目并依靠重复 PUT 幂等续传；接收目录失效时回到等待确认。已接受但迟迟未开始上传的传输约 90 秒后失败并通知对端，上传中仍按五分钟无进展判定停滞。

## 网络边界

节点只绑定回环、RFC1918 私网、链路本地和 Tailscale 组网 IPv4 地址，节点信息和浏览器访问接口只公布成功监听的非回环地址，网络变化后更新，允许返回空列表。HTTP 和 WebSocket 使用明文，无账号和令牌；跨来源管理请求只接受本机 Tauri/Android 来源、私网或 Tailscale 来源，明显来自公网网站的写请求会被拒绝。服务面向受信任局域网和组网环境，应避免通过端口转发暴露到互联网。

Android 使用明文 HTTP/WebSocket，安装包允许局域网明文流量；该设置不提供认证或加密，不能用于公网。检测到带默认路由的全隧道 VPN 且存在 Wi‑Fi 或以太网时，把新 socket 绑定到非 VPN 物理网络以保持 mDNS、HTTP 和 WebSocket 可达；检测到 Tailscale 的 `100.64.0.0/10` 或 `fd7a:115c:a1e0::/48` 地址/路由时保留 Tailscale。VPN 或 Wi‑Fi 切换会重建监听器和 mDNS；没有物理网络、VPN 锁定非 VPN 流量或系统拒绝绑定时保留默认网络，局域网发现可能为空。
