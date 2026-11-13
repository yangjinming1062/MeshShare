use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;
pub const CLIPBOARD_IMAGE_LIMIT: usize = 16 * 1024 * 1024;
pub const MAX_TRANSFER_BYTES: u64 = 5 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AddressInfo {
    pub address: String,
    pub url: String,
    pub label: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfo {
    pub node_id: String,
    pub node_name: String,
    pub version: String,
    pub platform: String,
    pub port: u16,
    pub addresses: Vec<AddressInfo>,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfo {
    pub node_id: String,
    pub node_name: String,
    pub platform: String,
    pub addresses: Vec<AddressInfo>,
    pub capabilities: Vec<String>,
    pub online: bool,
    pub last_seen_ms: i64,
    pub endpoint: Option<String>,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Hlc {
    pub physical_ms: i64,
    pub logical: u64,
    pub node_id: String,
}

impl Hlc {
    pub fn compare(&self, other: &Self) -> std::cmp::Ordering {
        (self.physical_ms, self.logical, &self.node_id).cmp(&(
            other.physical_ms,
            other.logical,
            &other.node_id,
        ))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipboardEvent {
    pub event_id: String,
    pub origin_node_id: String,
    pub clock: Hlc,
    pub text: Option<String>,
    pub image_png: Option<String>,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipboardSnapshot {
    pub event_id: String,
    pub source_node_id: String,
    pub source_name: String,
    pub clock: Hlc,
    pub text: Option<String>,
    pub image_png: Option<String>,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferItem {
    pub item_id: String,
    pub name: String,
    pub relative_path: String,
    pub size: u64,
    pub sha256: String,
    pub media_type: String,
    pub directory: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferOffer {
    pub transfer_id: String,
    pub source_node_id: String,
    pub source_name: String,
    pub target_node_id: String,
    pub items: Vec<TransferItem>,
    pub total_bytes: u64,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransferStatus {
    Waiting,
    Accepted,
    Rejected,
    Uploading,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferProgress {
    pub transfer_id: String,
    pub status: TransferStatus,
    pub completed_bytes: u64,
    pub total_bytes: u64,
    pub completed_items: usize,
    pub total_items: usize,
    pub destination: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum WireMessage {
    Hello {
        node: NodeInfo,
    },
    Ping {
        at_ms: i64,
    },
    Pong {
        at_ms: i64,
    },
    ClipboardUpdate {
        event: ClipboardEvent,
    },
    TransferOffer {
        offer: TransferOffer,
    },
    TransferAction {
        transfer_id: String,
        action: String,
        destination: Option<String>,
        reason: Option<String>,
    },
    TransferProgress {
        progress: TransferProgress,
    },
}
