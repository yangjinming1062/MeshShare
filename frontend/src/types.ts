export type AddressInfo = {
  address: string;
  url: string;
  label: string;
  kind: string;
};

export type NodeInfo = {
  node_id: string;
  node_name: string;
  version: string;
  protocol_version: number;
  platform: string;
  port: number;
  addresses: AddressInfo[];
  mobile_addresses?: AddressInfo[];
  capabilities: string[];
};

export type UpdateFrequency = 'daily' | 'weekly' | 'disabled';

export type UpdateState = 'unavailable' | 'disabled' | 'idle' | 'checking' | 'available' | 'deferred' | 'downloading' | 'installing' | 'up_to_date' | 'error';

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
  node_id: string;
  node_name: string;
  label: string;
  created_at_ms: number;
  last_seen_ms: number;
};

export type PeerInfo = {
  node_id: string;
  node_name: string;
  platform: string;
  addresses: AddressInfo[];
  mobile_addresses?: AddressInfo[];
  capabilities: string[];
  online: boolean;
  last_seen_ms: number;
  endpoint?: string;
  source: string;
  metrics?: HardwareMetrics;
  metrics_received_at_ms?: number;
  metrics_stale?: boolean;
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
  disks?: DiskMetrics[];
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

/** 发送前的本地准备态：文件摘要未算完，任务还没有在节点上登记。 */
export type TransferPrepare = {
  prepare_id: string;
  target_name: string;
  item_count: number;
  total_bytes: number;
  hashed_bytes: number;
};

export type MobileTransferView = {
  session_id: string;
  progress: TransferProgress;
  downloaded_item_ids?: string[];
  retryable_download_item_ids?: string[];
};

export type PhoneTransfer = TransferProgress & {
  peerName?: string;
  downloadedItemIds?: string[];
  retryableDownloadItemIds?: string[];
};

export type WireMessage =
  | { type: 'Hello'; payload: { node: NodeInfo } }
  | { type: 'Ping'; payload: { at_ms: number } }
  | { type: 'Pong'; payload: { at_ms: number } }
  | { type: 'ClipboardUpdate'; payload: { event: unknown } }
  | { type: 'ClipboardSnapshot'; payload: { snapshot: ClipboardSnapshot } }
  | { type: 'TransferOffer'; payload: { offer: TransferOffer } }
  | { type: 'TransferAction'; payload: { transfer_id: string; action: string; reason?: string; retry_count?: number } }
  | { type: 'TransferProgress'; payload: { progress: TransferProgress } }
  | { type: 'MetricsUpdate'; payload: { metrics: HardwareMetrics } };
