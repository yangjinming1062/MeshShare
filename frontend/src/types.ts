export type AddressInfo = {
  address: string;
  url: string;
  label: string;
  kind: string;
};

export type TerminalKeys = { client_fingerprint: string; host_fingerprint?: string };

export type NodeInfo = {
  node_id: string;
  node_name: string;
  version: string;
  protocol_version: number;
  platform: string;
  port: number;
  addresses: AddressInfo[];
  mobile_addresses: AddressInfo[];
  capabilities: string[];
  terminal?: TerminalKeys;
};

export type UpdateFrequency = 'daily' | 'weekly' | 'disabled';

export type UpdateState = 'unavailable' | 'disabled' | 'idle' | 'checking' | 'available' | 'deferred' | 'downloading' | 'installing' | 'up_to_date' | 'needs_authorization' | 'error';

export type UpdateStatus = {
  state: UpdateState;
  current_version: string;
  available_version: string | null;
  downloaded_bytes: number;
  content_length: number | null;
  error: string | null;
};

export type ReceiveMode = 'confirm' | 'auto';

export type ReceiveSettings = {
  mode: ReceiveMode;
  default_path?: string | null;
};

export type MobileSession = {
  session_id: string;
  node_name: string;
  label: string;
  created_at_ms: number;
};

export type PeerInfo = {
  node_id: string;
  node_name: string;
  platform: string;
  addresses: AddressInfo[];
  mobile_addresses: AddressInfo[];
  capabilities: string[];
  terminal?: TerminalKeys;
  online: boolean;
  endpoint?: string;
  metrics?: HardwareMetrics;
  metrics_stale: boolean;
};

export type GpuMemoryKind = 'dedicated' | 'unified' | 'unknown';

export type GpuMetrics = {
  name: string;
  utilization_percent?: number;
  memory_used_bytes?: number;
  memory_total_bytes?: number;
  memory_kind: GpuMemoryKind;
  unavailable_reason?: string;
};

export type LoadAverage = {
  one: number;
  five: number;
  fifteen: number;
};

export type DiskMetrics = {
  name: string;
  mount_point: string;
  file_system: string;
  total_bytes: number;
  available_bytes: number;
  removable: boolean;
};

export type HardwareMetrics = {
  node_id: string;
  sampled_at_ms: number;
  cpu_usage_percent?: number;
  cpu_model?: string;
  cpu_logical_cores?: number;
  cpu_physical_cores?: number;
  cpu_frequency_mhz?: number;
  memory_used_bytes?: number;
  memory_total_bytes?: number;
  memory_available_bytes?: number;
  swap_used_bytes?: number;
  swap_total_bytes?: number;
  load_average?: LoadAverage;
  uptime_seconds?: number;
  network_down_bytes_per_sec?: number;
  network_up_bytes_per_sec?: number;
  gpus: GpuMetrics[];
  disks: DiskMetrics[];
};

export type ClipboardSnapshot = {
  event_id: string;
  source_node_id: string;
  source_name: string;
  clock: { physical_ms: number; logical: number; node_id: string };
  text?: string;
  image_png?: string;
  updated_at_ms: number;
};

export type ClipboardSession = 'wayland' | 'x11' | 'other' | 'unknown';

export type ClipboardStatus = {
  session: ClipboardSession;
  available: boolean;
  retrying: boolean;
  message?: string | null;
};

export type MonitorBackend = 'wayland' | 'x11' | 'other' | 'unknown';

export type MonitorCapabilities = {
  backend: MonitorBackend;
  can_persist_position: boolean;
  can_click_through: boolean;
  can_keep_above: boolean;
};

export type NodeMonitorSettings = {
  visible: boolean;
  locked: boolean;
  capabilities: MonitorCapabilities;
};

export type NodeSettings = {
  clipboard_sync_enabled: boolean;
  autostart_enabled: boolean;
  update_frequency: UpdateFrequency;
  update_status: UpdateStatus | null;
  receive_settings: ReceiveSettings;
  clipboard_history_settings: ClipboardHistorySettings;
  clipboard_status: ClipboardStatus;
  monitor: NodeMonitorSettings;
};

export type NodeSettingsPatch = {
  clipboard_sync_enabled?: boolean;
  autostart_enabled?: boolean;
  update_frequency?: UpdateFrequency;
  receive_settings?: ReceiveSettings;
  clipboard_history_settings?: { mode: ClipboardHistoryMode; value: number };
  monitor?: { visible?: boolean; locked?: boolean };
};

export type ClipboardHistoryMode = 'count' | 'days';

export type ClipboardHistorySettings = {
  mode: ClipboardHistoryMode;
  count: number;
  days: number;
};

export type ClipboardHistoryItem = {
  event_id: string;
  source_node_id: string;
  source_name: string;
  updated_at_ms: number;
  recorded_at_ms: number;
  has_text: boolean;
  has_image: boolean;
  text_preview?: string;
  size_bytes: number;
};

export type ClipboardHistoryResponse = {
  settings: ClipboardHistorySettings;
  items: ClipboardHistoryItem[];
};

export type ClipboardHistoryDetail = {
  item: ClipboardHistoryItem;
  snapshot: ClipboardSnapshot;
};

export type TransferItem = {
  item_id: string;
  name: string;
  relative_path: string;
  size: number;
  /** 页面创建清单时留空，由节点写入文件体时计算回填；下载校验以此为准。 */
  sha256: string;
  media_type: string;
  directory: boolean;
};

export type TransferOffer = {
  transfer_id: string;
  source_node_id: string;
  source_name: string;
  target_node_id: string;
  items: TransferItem[];
  total_bytes: number;
  created_at_ms: number;
};

export type TransferStatus = 'waiting' | 'accepted' | 'uploading' | 'ready' | 'rejected' | 'completed' | 'failed' | 'cancelled';

export type TransferProgress = {
  transfer_id: string;
  retry_count?: number;
  status: TransferStatus;
  /** 出站任务正在等待接收端返回清单对应的状态。 */
  awaiting_offer_reply: boolean;
  completed_bytes: number;
  total_bytes: number;
  completed_items: number;
  total_items: number;
  destination?: string;
  error?: string;
  offer?: TransferOffer;
  direction?: 'send' | 'receive';
};

export type TransferFailure = {
  progress: TransferProgress;
  failed_at_ms: number;
  retryable: boolean;
};

/** 已结束传输的摘要记录，由节点持久化保存。 */
export type TransferHistoryEntry = {
  transfer_id: string;
  direction: 'send' | 'receive';
  peer_node_id: string;
  peer_name: string;
  status: TransferStatus;
  completed_bytes: number;
  total_bytes: number;
  completed_items: number;
  total_items: number;
  source_paths?: string[];
  destination?: string;
  /** 接收方向记录首个已提交条目的顶层名称，用于定位落地项目。 */
  item_root?: string;
  error?: string;
  retry_count?: number;
  session_id?: string;
  finished_at_ms: number;
};

export type TransferHistoryCleared = {
  cleared: number;
};

/** 发送前的本地准备态：先算摘要，再把源文件交给本机节点暂存。 */
export type TransferPrepare = {
  prepare_id: string;
  target_name: string;
  item_count: number;
  total_bytes: number;
  hashed_bytes: number;
  phase: 'hash' | 'stage';
};

export type OutgoingStagingView = {
  transfer_id: string;
  staged_items: number;
  total_items: number;
};

/** 桌面端按原路径直传的准备进度：节点枚举并计算摘要，不复制源文件。 */
export type OutgoingPrepareView = {
  transfer_id: string;
  state: 'preparing' | 'failed';
  error?: string | null;
  hashed_bytes: number;
  total_bytes: number;
  items_ready: number;
  total_items: number;
};

export type MobileTransferView = {
  session_id: string;
  progress: TransferProgress;
  downloaded_item_ids: string[];
  retryable_download_item_ids?: string[];
};

export type PhoneTransfer = TransferProgress & {
  peer_name?: string;
  web?: MobileTransferView;
};

/** 远程管理页在目标机器上的目录浏览结果；路径都由目标节点规范化返回。 */
export type BrowseRoot = {
  label: string;
  path: string;
};

export type BrowseRoots = {
  home: string | null;
  roots: BrowseRoot[];
};

export type BrowseEntry = {
  name: string;
  path: string;
  directory: boolean;
  symlink: boolean;
  size: number;
  modified_ms: number | null;
};

export type BrowseListing = {
  path: string;
  parent: string | null;
  entries: BrowseEntry[];
  truncated: boolean;
};

export type WireMessage =
  | { type: 'Hello'; payload: { node: NodeInfo } }
  | { type: 'Ping'; payload: { at_ms: number } }
  | { type: 'Pong'; payload: { at_ms: number } }
  | { type: 'ClipboardUpdate'; payload: { event: unknown } }
  | { type: 'ClipboardSnapshot'; payload: { snapshot: ClipboardSnapshot } }
  | { type: 'TransferOffer'; payload: { offer: TransferOffer } }
  | { type: 'TransferAction'; payload: { transfer_id: string; action: 'accept' | 'reject' | 'cancel' | 'fail'; reason?: string | null; retry_count: number } }
  | { type: 'TransferProgress'; payload: { progress: TransferProgress } }
  | { type: 'MetricsUpdate'; payload: { metrics: HardwareMetrics } }
  | { type: 'TransferHistoryDeleted'; payload: { transfer_ids: string[] } };
