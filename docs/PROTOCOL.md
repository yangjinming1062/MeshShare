# MeshShare v1 协议

## HTTP 接口

| 方法 | 路径 | 用途 |
| --- | --- | --- |
| `GET` | `/api/v1/info` | 节点身份、能力、端口和访问地址 |
| `GET` | `/api/v1/peers` | 当前在线设备 |
| `POST` | `/api/v1/peers/connect` | 通过手动地址加入节点 |
| `GET` | `/api/v1/clipboard` | 当前内存中的剪贴板快照 |
| `POST` | `/api/v1/clipboard` | 手机显式同步文字/PNG |
| `POST` | `/api/v1/transfers` | 创建文件清单和接收确认任务 |
| `POST` | `/api/v1/transfers/{id}/accept` | 接收端确认并提供目录 |
| `POST` | `/api/v1/transfers/{id}/reject` | 拒绝任务 |
| `POST` | `/api/v1/transfers/{id}/cancel` | 取消任务 |
| `GET` | `/api/v1/transfers/{id}/status` | 查询进度 |
| `PUT` | `/api/v1/transfers/{id}/items/{item_id}` | 直接写入单个文件流 |

所有 JSON 错误使用 `{ "error": "..." }`，不返回剪贴板历史或文件列表。

## WebSocket 消息

消息使用 `{ "type": "VariantName", "payload": ... }`：

- `Hello`：节点身份和能力。
- `Ping` / `Pong`：连接保活。
- `ClipboardUpdate`：事件 ID、来源节点、HLC、文字和 base64 PNG。
- `TransferOffer`：传输清单、总大小和发送节点。
- `TransferAction`：接受、拒绝或取消。
- `TransferProgress`：传输状态、已完成字节和错误。

剪贴板事件 ID 在节点内存中去重十分钟。节点重启后不恢复历史事件。

## 传输安全约束

`relative_path` 不能包含绝对路径、盘符或 `..`。接收端先写入目标目录中的随机 `.part` 文件，校验 SHA-256 和清单大小后才改为最终文件名。任何失败路径都删除临时文件。
