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
  platform: string;
  port: number;
  addresses: AddressInfo[];
  capabilities: string[];
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

export type TransferStatus = 'waiting' | 'accepted' | 'rejected' | 'uploading' | 'completed' | 'failed' | 'cancelled';

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
