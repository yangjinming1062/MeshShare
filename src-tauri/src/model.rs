use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;
pub const CLIPBOARD_IMAGE_LIMIT: usize = 16 * 1024 * 1024;
pub const MAX_TRANSFER_BYTES: u64 = 5 * 1024 * 1024 * 1024;
pub const METRICS_SYSTEM_CAPABILITY: &str = "metrics.system";
pub const METRICS_GPU_CAPABILITY: &str = "metrics.gpu";
pub const METRICS_STALE_AFTER_MS: i64 = 5_000;

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
    pub protocol_version: u32,
    pub platform: String,
    pub port: u16,
    pub addresses: Vec<AddressInfo>,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MobileSession {
    pub session_id: String,
    pub node_id: String,
    pub node_name: String,
    pub label: String,
    pub created_at_ms: i64,
    pub last_seen_ms: i64,
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
    #[serde(default)]
    pub metrics: Option<HardwareMetrics>,
    #[serde(default)]
    pub metrics_received_at_ms: Option<i64>,
    #[serde(default)]
    pub metrics_stale: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GpuMemoryKind {
    Dedicated,
    Unified,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GpuMetrics {
    pub name: String,
    pub utilization_percent: Option<f32>,
    pub memory_used_bytes: Option<u64>,
    pub memory_total_bytes: Option<u64>,
    pub memory_kind: GpuMemoryKind,
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HardwareMetrics {
    pub node_id: String,
    pub sampled_at_ms: i64,
    pub cpu_usage_percent: Option<f32>,
    pub memory_used_bytes: Option<u64>,
    pub memory_total_bytes: Option<u64>,
    pub network_down_bytes_per_sec: Option<u64>,
    pub network_up_bytes_per_sec: Option<u64>,
    pub gpus: Vec<GpuMetrics>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum MonitorDisplayMode {
    #[default]
    Floating,
    Tray,
    Disabled,
}

impl MonitorDisplayMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Floating => "floating",
            Self::Tray => "tray",
            Self::Disabled => "disabled",
        }
    }
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
    Uploading,
    Ready,
    Rejected,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offer: Option<TransferOffer>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MobileTransferView {
    pub session_id: String,
    pub progress: TransferProgress,
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
    ClipboardSnapshot {
        snapshot: ClipboardSnapshot,
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
    MetricsUpdate {
        metrics: HardwareMetrics,
    },
}

#[cfg(test)]
mod tests {
    use super::{
        GpuMemoryKind, GpuMetrics, HardwareMetrics, TransferItem, TransferOffer, TransferProgress,
        TransferStatus, WireMessage,
    };
    use serde_json::Value;

    #[test]
    fn metrics_message_round_trips_through_json() {
        let message = WireMessage::MetricsUpdate {
            metrics: HardwareMetrics {
                node_id: "node-a".to_string(),
                sampled_at_ms: 42,
                cpu_usage_percent: Some(12.5),
                memory_used_bytes: Some(10),
                memory_total_bytes: Some(20),
                network_down_bytes_per_sec: Some(30),
                network_up_bytes_per_sec: Some(40),
                gpus: vec![GpuMetrics {
                    name: "GPU".to_string(),
                    utilization_percent: None,
                    memory_used_bytes: None,
                    memory_total_bytes: None,
                    memory_kind: GpuMemoryKind::Unknown,
                    unavailable_reason: Some("unavailable".to_string()),
                }],
            },
        };
        let encoded = serde_json::to_string(&message).expect("serialize metrics");
        let decoded: WireMessage = serde_json::from_str(&encoded).expect("deserialize metrics");
        match decoded {
            WireMessage::MetricsUpdate { metrics } => {
                assert_eq!(metrics.node_id, "node-a");
                assert_eq!(
                    metrics.gpus[0].unavailable_reason.as_deref(),
                    Some("unavailable")
                );
            }
            _ => panic!("unexpected wire message"),
        }
    }

    #[test]
    fn tagged_messages_keep_the_nested_payload_contract() {
        let offer = TransferOffer {
            transfer_id: "transfer-1".to_string(),
            source_node_id: "node-a".to_string(),
            source_name: "A".to_string(),
            target_node_id: "node-b".to_string(),
            items: vec![TransferItem {
                item_id: "item-1".to_string(),
                name: "hello.txt".to_string(),
                relative_path: "hello.txt".to_string(),
                size: 5,
                sha256: "hash".to_string(),
                media_type: "text/plain".to_string(),
                directory: false,
            }],
            total_bytes: 5,
            created_at_ms: 1,
        };
        let cases = [
            (
                serde_json::to_value(WireMessage::TransferOffer {
                    offer: offer.clone(),
                })
                .expect("serialize offer"),
                "offer",
            ),
            (
                serde_json::to_value(WireMessage::TransferProgress {
                    progress: TransferProgress {
                        transfer_id: "transfer-1".to_string(),
                        status: TransferStatus::Waiting,
                        completed_bytes: 0,
                        total_bytes: 5,
                        completed_items: 0,
                        total_items: 1,
                        destination: None,
                        error: None,
                        offer: Some(offer),
                        direction: Some("receive".to_string()),
                    },
                })
                .expect("serialize progress"),
                "progress",
            ),
            (
                serde_json::to_value(WireMessage::MetricsUpdate {
                    metrics: HardwareMetrics {
                        node_id: "node-a".to_string(),
                        sampled_at_ms: 1,
                        cpu_usage_percent: None,
                        memory_used_bytes: None,
                        memory_total_bytes: None,
                        network_down_bytes_per_sec: None,
                        network_up_bytes_per_sec: None,
                        gpus: Vec::new(),
                    },
                })
                .expect("serialize metrics"),
                "metrics",
            ),
        ];
        for (value, key) in cases {
            assert!(value.get("payload").and_then(Value::as_object).is_some());
            assert!(value["payload"].get(key).is_some());
        }
    }
}
