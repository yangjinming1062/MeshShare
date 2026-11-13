# MeshShare 架构图谱

## 运行拓扑

```mermaid
flowchart LR
  H[主持设备\nPython 服务] -->|HTTP :8765| A[手机浏览器]
  H -->|HTTP :8765| B[笔记本浏览器]
  H -->|HTTP :8765| C[平板浏览器]
  H --> D[(SQLite\n共享状态)]
  H --> E[(data/files\n文件内容)]
```

主持设备和客户端必须在可互通的局域网/VLAN 中。服务只监听局域网接口，客户端不直接互相连接。

## 请求与同步

```mermaid
sequenceDiagram
  participant C as 客户端
  participant S as 服务端
  participant DB as SQLite
  C->>S: GET /api/state
  S->>DB: 读取快照与 revision
  DB-->>S: state
  S-->>C: JSON snapshot
  C->>S: POST /api/messages 或 /api/files
  S->>DB: 事务写入 + change
  DB-->>S: 新 revision
  S-->>C: 201/202 JSON
  S-->>C: SSE event(revision)
  C->>S: GET /api/state?since=revision
  S-->>C: 新快照
```

SSE 连接是通知通道，不是数据存储。服务端重启、代理缓冲或网络切换后，客户端都会回到状态快照。

## 构建与部署

```mermaid
flowchart TD
  P[桌面程序 / python -m meshshare] --> V[校验配置]
  V --> M[创建 data/ 与 schema]
  M --> T[ThreadingHTTPServer]
  T --> W[web/ 静态资源]
  T --> API[/api/*]
  API --> S[Service]
  S --> DB[(SQLite)]
  S --> F[data/files]
```

无前端构建产物和第三方运行时依赖；发布包只需 Python 源码、`web/` 和空的数据目录。
