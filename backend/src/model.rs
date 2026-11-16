use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;
pub const CLIPBOARD_TEXT_LIMIT: usize = 16 * 1024 * 1024;
pub const CLIPBOARD_IMAGE_LIMIT: usize = 16 * 1024 * 1024;
pub const CLIPBOARD_VIEW_CAPABILITY: &str = "clipboard.view";
pub const DEFAULT_CLIPBOARD_HISTORY_COUNT: usize = 50;
pub const DEFAULT_CLIPBOARD_HISTORY_DAYS: u32 = 30;
pub const MAX_CLIPBOARD_HISTORY_COUNT: usize = 500;
pub const MAX_CLIPBOARD_HISTORY_DAYS: u32 = 365;
pub const MAX_TRANSFER_RECORDS: usize = 512;
pub const MAX_MOBILE_SESSIONS: usize = 256;
pub const METRICS_SYSTEM_CAPABILITY: &str = "metrics.system";
pub const METRICS_GPU_CAPABILITY: &str = "metrics.gpu";
pub const METRICS_NETWORK_CAPABILITY: &str = "metrics.network";
pub const METRICS_SYSTEM_VIEW_CAPABILITY: &str = "metrics.view.system";
pub const METRICS_GPU_VIEW_CAPABILITY: &str = "metrics.view.gpu";
pub const REMOTE_MANAGEMENT_CAPABILITY: &str = "management.remote.v1";
pub const REMOTE_FILES_CAPABILITY: &str = "management.files.v1";
pub const METRICS_STALE_AFTER_MS: i64 = 5_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AddressInfo {
    pub address: String,
    pub url: String,
    pub label: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MobileAccessInfo {
    pub bootstrap_urls: Vec<AddressInfo>,
    pub secure_urls: Vec<AddressInfo>,
    pub ca_url: String,
    pub ca_fingerprint: String,
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
    #[serde(default)]
    pub mobile_addresses: Vec<AddressInfo>,
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
    #[serde(default)]
    pub mobile_addresses: Vec<AddressInfo>,
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
pub struct LoadAverage {
    pub one: f64,
    pub five: f64,
    pub fifteen: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiskMetrics {
    pub name: String,
    pub mount_point: String,
    pub file_system: String,
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub removable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct HardwareMetrics {
    pub node_id: String,
    pub sampled_at_ms: i64,
    pub cpu_usage_percent: Option<f32>,
    #[serde(default)]
    pub cpu_model: Option<String>,
    #[serde(default)]
    pub cpu_logical_cores: Option<u32>,
    #[serde(default)]
    pub cpu_physical_cores: Option<u32>,
    #[serde(default)]
    pub cpu_frequency_mhz: Option<u64>,
    pub memory_used_bytes: Option<u64>,
    pub memory_total_bytes: Option<u64>,
    #[serde(default)]
    pub memory_available_bytes: Option<u64>,
    #[serde(default)]
    pub swap_used_bytes: Option<u64>,
    #[serde(default)]
    pub swap_total_bytes: Option<u64>,
    #[serde(default)]
    pub load_average: Option<LoadAverage>,
    #[serde(default)]
    pub uptime_seconds: Option<u64>,
    pub network_down_bytes_per_sec: Option<u64>,
    pub network_up_bytes_per_sec: Option<u64>,
    pub gpus: Vec<GpuMetrics>,
    #[serde(default)]
    pub disks: Vec<DiskMetrics>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MonitorPosition {
    pub x: i32,
    pub y: i32,
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardSession {
    Wayland,
    X11,
    Other,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClipboardStatus {
    pub session: ClipboardSession,
    pub available: bool,
    pub retrying: bool,
    pub message: Option<String>,
}

impl Default for ClipboardStatus {
    fn default() -> Self {
        Self {
            session: ClipboardSession::Unknown,
            available: false,
            retrying: true,
            message: Some("正在连接系统剪贴板…".to_string()),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardHistoryMode {
    #[default]
    Count,
    Days,
}

impl ClipboardHistoryMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "count" => Some(Self::Count),
            "days" => Some(Self::Days),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClipboardHistorySettings {
    pub mode: ClipboardHistoryMode,
    pub count: usize,
    pub days: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClipboardHistoryItem {
    pub event_id: String,
    pub source_node_id: String,
    pub source_name: String,
    pub updated_at_ms: i64,
    pub recorded_at_ms: i64,
    pub has_text: bool,
    pub has_image: bool,
    pub text_preview: Option<String>,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClipboardHistoryList {
    pub settings: ClipboardHistorySettings,
    pub items: Vec<ClipboardHistoryItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipboardHistoryDetail {
    pub item: ClipboardHistoryItem,
    pub snapshot: ClipboardSnapshot,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReceiveMode {
    #[default]
    Confirm,
    Auto,
}

impl ReceiveMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "confirm" => Some(Self::Confirm),
            "auto" => Some(Self::Auto),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReceiveSettings {
    pub mode: ReceiveMode,
    pub default_path: Option<String>,
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
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub retry_count: u32,
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

fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MobileTransferView {
    pub session_id: String,
    pub progress: TransferProgress,
    #[serde(default)]
    pub downloaded_item_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retryable_download_item_ids: Vec<String>,
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
        reason: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retry_count: Option<u32>,
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
        DiskMetrics, GpuMemoryKind, GpuMetrics, HardwareMetrics, LoadAverage, TransferItem,
        TransferOffer, TransferProgress, TransferStatus, WireMessage,
    };
    use serde_json::Value;

    #[test]
    fn metrics_message_round_trips_through_json() {
        let message = WireMessage::MetricsUpdate {
            metrics: HardwareMetrics {
                node_id: "node-a".to_string(),
                sampled_at_ms: 42,
                cpu_usage_percent: Some(12.5),
                cpu_model: Some("Example CPU".to_string()),
                cpu_logical_cores: Some(16),
                cpu_physical_cores: Some(8),
                cpu_frequency_mhz: Some(4_200),
                memory_used_bytes: Some(10),
                memory_total_bytes: Some(20),
                memory_available_bytes: Some(10),
                swap_used_bytes: Some(2),
                swap_total_bytes: Some(8),
                load_average: Some(LoadAverage {
                    one: 0.5,
                    five: 0.4,
                    fifteen: 0.3,
                }),
                uptime_seconds: Some(123),
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
                disks: vec![DiskMetrics {
                    name: "System".to_string(),
                    mount_point: "/".to_string(),
                    file_system: "ext4".to_string(),
                    total_bytes: 100,
                    available_bytes: 40,
                    removable: false,
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
    fn legacy_metrics_payload_defaults_new_fields() {
        let value = serde_json::json!({
            "type": "MetricsUpdate",
            "payload": {
                "metrics": {
                    "node_id": "old-node",
                    "sampled_at_ms": 1,
                    "cpu_usage_percent": 10.0,
                    "memory_used_bytes": 1,
                    "memory_total_bytes": 2,
                    "network_down_bytes_per_sec": 3,
                    "network_up_bytes_per_sec": 4,
                    "gpus": []
                }
            }
        });
        let decoded: WireMessage = serde_json::from_value(value).expect("decode legacy metrics");
        match decoded {
            WireMessage::MetricsUpdate { metrics } => {
                assert_eq!(metrics.cpu_model, None);
                assert_eq!(metrics.swap_total_bytes, None);
                assert!(metrics.disks.is_empty());
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
                        retry_count: 0,
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
                        ..Default::default()
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
