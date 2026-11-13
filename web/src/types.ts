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
  capabilities: string[];
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

export type HardwareMetrics = {
  node_id: string;
  sampled_at_ms: number;
  cpu_usage_percent?: number;
  memory_used_bytes?: number;
  memory_total_bytes?: number;
  network_down_bytes_per_sec?: number;
  network_up_bytes_per_sec?: number;
  gpus: GpuMetrics[];
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

export type MobileTransferView = {
  session_id: string;
  progress: TransferProgress;
};

export type WireMessage =
  | { type: 'Hello'; payload: { node: NodeInfo } }
  | { type: 'Ping'; payload: { at_ms: number } }
  | { type: 'Pong'; payload: { at_ms: number } }
  | { type: 'ClipboardUpdate'; payload: { event: unknown } }
  | { type: 'ClipboardSnapshot'; payload: { snapshot: ClipboardSnapshot } }
  | { type: 'TransferOffer'; payload: { offer: TransferOffer } }
  | { type: 'TransferAction'; payload: { transfer_id: string; action: string; destination?: string; reason?: string } }
  | { type: 'TransferProgress'; payload: { progress: TransferProgress } }
  | { type: 'MetricsUpdate'; payload: { metrics: HardwareMetrics } };
