# MeshShare 节点协议

HTTP 和 WebSocket 使用 `/api/v1`，原生节点的当前握手版本为 `2`。请求和响应以[路由及处理函数](../backend/src/server.rs)、[节点消息类型](../backend/src/model.rs)和[设置类型](../backend/src/control.rs)为准。常见业务错误返回 `{"error":"…"}`。节点的地址范围和操作权限见[网络与权限](ARCHITECTURE.md#网络与权限)。

## HTTP 接口

按节点 ID 选择管理目标。桌面客户端在写入前用 `GET /api/v1/info` 核对候选地址，并用 `X-MeshShare-Target` 指定预期处理请求的节点 ID。节点收到此请求头时会校验，目标不符时返回 421。

### 节点与设置

| 方法 | 路径 | 用途 |
| --- | --- | --- |
| `GET` | `/api/v1/info` | 节点身份、版本、能力和地址 |
| `GET` | `/api/v1/peers` | 已完成握手的对端节点及其地址 |
| `GET` | `/api/v1/metrics` | 本机最新硬件快照 |
| `GET` / `PATCH` | `/api/v1/settings` | 读取设置与能力 / 修改设置 |
| `POST` | `/api/v1/settings/update-check` | 请求目标节点检查更新 |
| `POST` | `/api/v1/management/service/restart` | 请求 Linux-headless 版重启服务 |

Linux-headless 版声明 `node.headless.v1`。它的 `autostart_enabled` 对应 systemd 自启状态，读取失败时设置快照返回 `service_control_error`。重启请求必须携带 `X-MeshShare-Target`，无需请求体。受理后返回 202，目标缺失或不符返回 421，有活动任务时返回 409，服务控制不可用时返回 503。响应中断时按节点 ID 重连，避免重复提交重启请求。

桌面的 `monitor.visible` 保存悬浮条显示偏好，实际显示仍受全屏自动隐藏影响。

### 剪贴板

| 方法 | 路径 | 用途 |
| --- | --- | --- |
| `GET` / `POST` | `/api/v1/clipboard` | 读取当前快照 / 发布 `text` 或 Base64 编码的 `image_png` |
| `GET` | `/api/v1/clipboard/status` | 系统剪贴板可用性和重试状态 |
| `GET` | `/api/v1/clipboard/preview/{event_id}` | 读取当前图片的缩小预览 |
| `GET` / `DELETE` | `/api/v1/clipboard/history` | 读取本机历史摘要 / 清空历史 |
| `GET` | `/api/v1/clipboard/history/{event_id}` | 读取完整历史内容 |
| `GET` | `/api/v1/clipboard/history/{event_id}/thumbnail` | 读取 96×72 PNG 历史缩略图 |
| `POST` | `/api/v1/clipboard/history/{event_id}/restore` | 用历史内容发布新事件 |

- 读取快照时可同时传入 `known_event_id` 和 `known_clock`。后者格式为 `physical_ms:logical:node_id`。两者均匹配时返回 204，其余返回最新快照或 JSON `null`。
- 预览的最长边为 720 像素，可用同格式的 `clock` 绑定版本。事件已替换、版本不符或没有可用缩略预览时返回 404，客户端可用快照中的原图生成预览。
- 文字和 PNG 文件各限 16 MiB，PNG 尺寸上限为 8192×8192。历史在目标节点本地保存，清空历史后仍可读取当前快照。

### 节点文件传输

| 方法 | 路径 | 用途 |
| --- | --- | --- |
| `POST` / `GET` | `/api/v1/transfers` | 提交 `TransferOffer` / 查询本机任务 |
| `GET` / `DELETE` | `/api/v1/transfers/history` | 查询最近 10 条终态历史 / 清空历史 |
| `POST` | `/api/v1/transfers/{id}/accept` | 用 `{"destination":"…"}` 接收 |
| `POST` | `/api/v1/transfers/{id}/{reject,cancel,fail,retry}` | 拒绝、取消、标记失败或重开失败任务 |
| `GET` | `/api/v1/transfers/{id}/status` | 查询任务进度与清单 |
| `PUT` | `/api/v1/transfers/{id}/items/{item_id}` | 上传单个文件流 |
| `DELETE` | `/api/v1/transfers/{id}` | 移除失败任务 |

`destination` 必须是接收节点可用的现有绝对目录。建单时 `target_node_id` 须与接收节点一致，地址变更时先用 `/api/v1/info` 核对身份，再对已确认的地址查询状态、上传或重试。失败任务移除时，记录不存在返回 404，记录处于其他状态返回 409。

历史按结束时间倒序，`GET` 和 `DELETE` 均可带 `session_id` 过滤网页会话记录。历史里的绝对路径属于返回记录的节点。接收记录可带 `item_root` 表示接收目录下的首个已提交顶层条目，打开目录前按本机实际路径检查。

### 发送准备与远程文件访问

| 方法 | 路径 | 用途 |
| --- | --- | --- |
| `POST` | `/api/v1/outgoing` | 向本机提交暂存清单 |
| `PUT` | `/api/v1/outgoing/{id}/items/{item_id}` | 上传到本机暂存区，校验大小并回填摘要 |
| `POST` | `/api/v1/outgoing/{id}/start` | 启动已暂存的发送任务 |
| `POST` | `/api/v1/outgoing/paths` | 按本机路径准备发送 |
| `GET` / `POST` | `/api/v1/outgoing/{id}` / `/api/v1/outgoing/{id}/cancel` | 查询按路径发送的准备进度 / 取消准备或清理暂存 |
| `GET` | `/api/v1/management/files/roots` | 获取目标机器的浏览入口 |
| `GET` | `/api/v1/management/files?path=…` | 查看目标目录一层元数据，支持 `offset` 与 `limit` |
| `POST` | `/api/v1/management/outgoing/paths` | 按被管理机器的路径准备发送 |
| `GET` / `POST` | `/api/v1/management/outgoing/{id}` / `/api/v1/management/outgoing/{id}/cancel` | 查询 / 取消远程准备任务 |
| `POST` | `/api/v1/management/mobile/outgoing/paths` | 用 `session_id` 和 `paths` 从被管理机器向其网页会话发送 |

`/outgoing` 仅允许回环请求。按路径发往另一节点时提交 `target_node_id` 和 `paths`，由持有文件的节点读取绝对路径并发往已连接的接收方。准备任务退出后，查询返回 404。

桌面界面按 `management.remote.v1` 开放远程管理，按 `management.files.v1` 开放远程文件浏览；从被管理机器按路径直接发送到网页使用 `management.mobile-paths.v1`。远程浏览和路径发送均要求在被管理节点启用远程文件访问，否则返回 403。桌面版通过本机界面启用，Linux-headless 版随新建配置启用。

### 网页会话

| 方法 | 路径 | 用途 |
| --- | --- | --- |
| `GET` | `/api/v1/mobile/access` | 查询网页地址和本地 CA 指纹 |
| `GET` | `/api/v1/mobile/ca` | 下载接入节点的本地 CA |
| `POST` / `GET` | `/api/v1/mobile/sessions` | 建立 / 查询活跃会话 |
| `POST` | `/api/v1/mobile/transfers` | 用 `session_id` 和 `offer` 登记下载任务 |
| `GET` | `/api/v1/mobile/transfers?session_id=…` | 查询下载任务并续期会话 |
| `POST` | `/api/v1/mobile/transfers/{id}/{accept,reject,cancel}` | 用 `session_id` 确认接收、拒绝或取消 |
| `POST` / `DELETE` | `/api/v1/mobile/transfers/{id}/fail` / `/api/v1/mobile/transfers/{id}` | 标记上传失败 / 移除失败记录 |
| `PUT` | `/api/v1/mobile/transfers/{id}/items/{item_id}` | 上传文件到接入节点的网页暂存区 |
| `GET` | `/api/v1/mobile/transfers/{id}/items/{item_id}/download?session_id=…` | 流式下载 |
| `POST` | `/api/v1/mobile/transfers/{id}/items/{item_id}/complete` | 用 `session_id` 确认项目已保存 |

`ready` 表示清单内容已暂存并校验。浏览器默认下载使用 `auto_complete=true`，节点在提供完整响应内容时确认项目，并在 `retryable_download_item_ids` 标记仍可重新下载的项目。使用 HTTPS 网页选择目录时，浏览器在本地流式写入和校验后调用 `complete`。若确认请求失败，可以补发确认。

HTTPS 网页向其他节点传文件时，以 `/api/v1/mobile/peers/{peer_id}` 为前缀调用目标 `/api/v1/info`，以及建单、查询状态、上传项目、拒绝、取消、失败标记和重试接口。客户端对代理请求指定接入节点 ID，代理转发时指定最终接收节点 ID。

## WebSocket 消息

通过 `GET /api/v1/ws` 建立连接。消息形如 `{"type":"Ping","payload":{"at_ms":0}}`，结构见 [`WireMessage`](../backend/src/model.rs)。原生节点用 `Hello` 交换身份、版本和能力，当前连接地址、协议版本和通告有效时才登记对端。界面在连接建立时发送 `Ping`，之后每 15 秒保活，节点回复 `Pong`。用 `?clipboard_updates=notice` 订阅 `ClipboardChanged`，收到事件 ID 后通过 HTTP 获取快照。省略参数时，界面接收带内容的 `ClipboardUpdate`。

| 消息 | 调用方需要遵守的约定 |
| --- | --- |
| `ClipboardUpdate` / `ClipboardSnapshot` | 更新携带 `event`，重连交换 `snapshot`。按事件 ID 去重，按混合逻辑时钟选最新快照，转发保留原始来源。 |
| `TransferOffer` | `offer` 的发送方须等于连接对端，目标须等于接收节点。 |
| `TransferAction` | `accept`、`reject`、`cancel`、`fail` 只对当前 `retry_count` 生效。 |
| `TransferProgress` | 节点直传的在途进度可省略 `offer`，沿用已有清单。字节包含当前在途数据，项目数只计已提交项目。进度须在清单总量内单调增长，同代终态禁止回到活动状态。 |
| `TransferHistoryDeleted` | 仅在声明 `transfer.history.v1` 的节点间同步删除配对记录。离线时暂存待通知的 ID，重连后补发。 |
| `MetricsUpdate` | 携带采集节点的最新快照。快照 `node_id` 须等于连接对端。 |

原生节点向声明 `clipboard.text` 或 `clipboard.view` 的对端发送剪贴板消息。桌面支持写入系统剪贴板。Android 和 Linux-headless 版使用 `clipboard.view`，均可主动发布共享剪贴板，Linux-headless 版还会转发新的快照。

指标能力按 `metrics.system`、`metrics.gpu`、`metrics.network` 声明，接收系统和 GPU 数据也可声明相应 `metrics.view.*`。消息按接收方能力省略数据。硬件或平台缺少数据时使用空字段或空列表，显存字段采用专用显存数据。Android 提供网络速率。对端指标在本地接收超过 5 秒后标为 `metrics_stale`。

## 传输安全约束

- **清单与提交。** `relative_path` 必须是接收目录内的相对路径，拒绝路径穿越、归一化后重复的路径和符号链接。接收方先写临时文件，核对大小和 SHA-256，再提交最终文件，避免覆盖已有文件。原生节点经 WebSocket 发布的清单须有文件摘要，允许从 HTTP 建单中省略摘要，首次写入后由节点计算回填。
- **失败与重试。** 发送端尚未收到建单回复时用 `awaiting_offer_reply` 标识。收到 `accepted` 可开始上传，`waiting` 表示仍需接收方确认。暂时失败可按项目重试，重复 PUT 会校验已提交文件并返回当前进度。手动重试由接收方的 HTTP `retry` 发起，`retry_count` 加一并保留已提交项目。目录失效时回到等待确认，正在安装更新时返回 425。
- **超时与清理。** 已接受的节点传输在约 90 秒内仍未开始上传时失败，传输五分钟无进度时失败。网页会话闲置 90 秒后到期，节点回收相应暂存文件。启动时回收已登记的临时文件和上次运行的暂存区。

## 远程终端

原生节点用 `Hello` 的 `node.terminal.client_fingerprint` 通告进程级 Ed25519 客户端指纹。Android 可发起远程终端。已开放入站终端的桌面版和 Linux-headless 版同时声明 `terminal.ssh.v3` 和独立的 `host_fingerprint`。桌面版确认授权后会在现有连接上发送新的 `Hello`。指纹使用 OpenSSH SHA-256 格式。

发起端请求 `GET /api/v1/terminal/ssh`，携带 `Connection: Upgrade`、`Upgrade: meshshare-ssh-v3` 和 `X-MeshShare-Terminal-Target`。101 后同一连接承载 SSH。来源或入站授权校验失败返回 403，目标错误返回 421，升级协议错误返回 426，连接已满返回 429，节点退出时返回 503。

发起端用当前节点连接通告的指纹核对主机密钥。接收方以客户端节点 ID 为 SSH 用户名，按该节点当前连接校验客户端密钥。节点连接断开或客户端指纹变更会关闭对应 SSH 会话，重连后需重新打开终端。

SSH 通道使用私有扩展数据类型 `0xFE000000` 和单字节 `0x00` 标记一批输出结束。客户端将该批输出纳入有界队列后原样确认，服务端继续发送下一批。shell 正常退出时，服务端送完剩余输出后发送 EOF。
