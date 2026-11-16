use crate::clock::{now_millis, LogicalClock};
use crate::model::{
    AddressInfo, ClipboardEvent, ClipboardHistoryDetail, ClipboardHistoryItem,
    ClipboardHistoryList, ClipboardHistoryMode, ClipboardHistorySettings, ClipboardSession,
    ClipboardSnapshot, ClipboardStatus, HardwareMetrics, MobileAccessInfo, MobileSession,
    MobileTransferView, MonitorPosition, NodeInfo, PeerInfo, ReceiveMode, ReceiveSettings,
    TransferOffer, TransferProgress, TransferStatus, WireMessage, CLIPBOARD_IMAGE_LIMIT,
    CLIPBOARD_TEXT_LIMIT, CLIPBOARD_VIEW_CAPABILITY, DEFAULT_CLIPBOARD_HISTORY_COUNT,
    DEFAULT_CLIPBOARD_HISTORY_DAYS, MAX_CLIPBOARD_HISTORY_COUNT, MAX_CLIPBOARD_HISTORY_DAYS,
    MAX_MOBILE_SESSIONS, MAX_TRANSFER_RECORDS, METRICS_GPU_CAPABILITY, METRICS_GPU_VIEW_CAPABILITY,
    METRICS_NETWORK_CAPABILITY, METRICS_SYSTEM_CAPABILITY, METRICS_SYSTEM_VIEW_CAPABILITY,
    REMOTE_MANAGEMENT_CAPABILITY, TRANSFER_RELAY_CAPABILITY,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};
use uuid::Uuid;

fn default_autostart_enabled() -> bool {
    true
}

fn default_monitor_visible() -> bool {
    true
}

fn default_clipboard_history_count() -> usize {
    DEFAULT_CLIPBOARD_HISTORY_COUNT
}

fn default_clipboard_history_days() -> u32 {
    DEFAULT_CLIPBOARD_HISTORY_DAYS
}

fn default_receive_mode() -> ReceiveMode {
    ReceiveMode::Confirm
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateFrequency {
    Daily,
    #[default]
    Weekly,
    Disabled,
}

impl UpdateFrequency {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::Weekly => "weekly",
            Self::Disabled => "disabled",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "daily" => Some(Self::Daily),
            "weekly" => Some(Self::Weekly),
            "disabled" => Some(Self::Disabled),
            _ => None,
        }
    }

    pub fn period_ms(self) -> Option<i64> {
        match self {
            Self::Daily => Some(24 * 60 * 60 * 1000),
            Self::Weekly => Some(7 * 24 * 60 * 60 * 1000),
            Self::Disabled => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedConfig {
    node_id: String,
    node_name: String,
    #[serde(default = "default_monitor_visible")]
    monitor_visible: bool,
    #[serde(default)]
    monitor_position: Option<MonitorPosition>,
    #[serde(default)]
    monitor_locked: bool,
    #[serde(default = "default_autostart_enabled")]
    autostart_enabled: bool,
    #[serde(default)]
    update_frequency: UpdateFrequency,
    #[serde(default)]
    update_last_checked_at_ms: Option<i64>,
    #[serde(default)]
    pending_update_version: Option<String>,
    #[serde(default)]
    clipboard_history_mode: ClipboardHistoryMode,
    #[serde(default = "default_clipboard_history_count")]
    clipboard_history_count: usize,
    #[serde(default = "default_clipboard_history_days")]
    clipboard_history_days: u32,
    #[serde(default = "default_receive_mode")]
    receive_mode: ReceiveMode,
    #[serde(default)]
    receive_directory: Option<String>,
    /// macOS security-scoped bookmark for the selected receive directory.
    /// Other platforms leave this unset and continue using the path field.
    #[serde(default)]
    receive_directory_bookmark: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ClipboardHistoryRecord {
    event_id: String,
    source_node_id: String,
    source_name: String,
    clock: crate::model::Hlc,
    updated_at_ms: i64,
    recorded_at_ms: i64,
    content_hash: String,
    text_path: Option<String>,
    image_path: Option<String>,
    text_len: usize,
    image_len: usize,
}

#[derive(Debug, Clone)]
pub struct TransferRecord {
    pub offer: TransferOffer,
    pub progress: TransferProgress,
    pub accepted_destination: Option<PathBuf>,
    pub received_items: HashSet<String>,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone)]
pub struct MobileTransferRecord {
    pub session_id: String,
    pub offer: TransferOffer,
    pub progress: TransferProgress,
    pub staged_directory: PathBuf,
    pub staged_items: HashSet<String>,
    pub downloaded_items: HashSet<String>,
    pub retryable_downloads: HashSet<String>,
    pub updated_at_ms: i64,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TransferRetryError {
    InstallingUpdate,
    NotRetryable,
}

pub(crate) const UPDATE_INSTALLING_MESSAGE: &str = "应用正在安装更新，请稍后重试";

struct MobileCa {
    path: PathBuf,
    fingerprint: String,
}

pub struct NodeRuntime {
    profile: NodeProfile,
    pub node: RwLock<NodeInfo>,
    pub clock: LogicalClock,
    pub clipboard: RwLock<Option<ClipboardSnapshot>>,
    clipboard_status: RwLock<ClipboardStatus>,
    clipboard_history: Mutex<Vec<ClipboardHistoryRecord>>,
    pub peers: RwLock<HashMap<String, PeerInfo>>,
    pub transfers: RwLock<HashMap<String, TransferRecord>>,
    pub mobile_sessions: RwLock<HashMap<String, MobileSession>>,
    pub mobile_transfers: RwLock<HashMap<String, MobileTransferRecord>>,
    mobile_manifest_bytes: AtomicU64,
    pub local_metrics: RwLock<Option<HardwareMetrics>>,
    pub monitor_visible: AtomicBool,
    pub monitor_locked: AtomicBool,
    mobile_ca: RwLock<Option<MobileCa>>,
    config: Mutex<PersistedConfig>,
    data_dir: PathBuf,
    clipboard_update: Mutex<()>,
    seen_events: Mutex<HashMap<String, i64>>,
    suppression: Mutex<Option<(String, i64)>>,
    peer_channels: Mutex<HashMap<String, mpsc::Sender<WireMessage>>>,
    pending_clipboard_resync: Mutex<HashSet<String>>,
    connecting_peers: Mutex<HashSet<String>>,
    transfer_locks: Mutex<HashSet<(String, String)>>,
    desktop_temp_files: Mutex<HashSet<PathBuf>>,
    pub events: broadcast::Sender<WireMessage>,
    pub sync_enabled: AtomicBool,
    pub stopping: AtomicBool,
    update_installing: AtomicBool,
    pub http: reqwest::Client,
    pub proxy_http: reqwest::Client,
    transfer_update_gate: Mutex<()>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeProfile {
    Desktop,
    Android,
}

impl NodeProfile {
    pub const fn current() -> Self {
        if cfg!(target_os = "android") {
            Self::Android
        } else {
            Self::Desktop
        }
    }

    pub const fn is_android(self) -> bool {
        matches!(self, Self::Android)
    }

    pub fn capabilities(self) -> Vec<String> {
        match self {
            Self::Desktop => vec![
                "clipboard.text".to_string(),
                "clipboard.image/png".to_string(),
                "transfer.file".to_string(),
                "transfer.folder".to_string(),
                "web.mobile".to_string(),
                METRICS_SYSTEM_CAPABILITY.to_string(),
                METRICS_GPU_CAPABILITY.to_string(),
                METRICS_NETWORK_CAPABILITY.to_string(),
                REMOTE_MANAGEMENT_CAPABILITY.to_string(),
                TRANSFER_RELAY_CAPABILITY.to_string(),
            ],
            Self::Android => vec![
                "transfer.file".to_string(),
                "transfer.folder".to_string(),
                CLIPBOARD_VIEW_CAPABILITY.to_string(),
                METRICS_NETWORK_CAPABILITY.to_string(),
                METRICS_SYSTEM_VIEW_CAPABILITY.to_string(),
                METRICS_GPU_VIEW_CAPABILITY.to_string(),
            ],
        }
    }
}

fn transfer_progress(offer: &TransferOffer, direction: &str) -> TransferProgress {
    TransferProgress {
        transfer_id: offer.transfer_id.clone(),
        retry_count: 0,
        status: TransferStatus::Waiting,
        completed_bytes: 0,
        total_bytes: offer.total_bytes,
        completed_items: 0,
        total_items: offer.items.len(),
        destination: None,
        error: None,
        offer: Some(offer.clone()),
        direction: Some(direction.to_string()),
    }
}

const MOBILE_SESSION_TTL_MS: i64 = 90_000;
const TRANSFER_RETENTION_MS: i64 = 10 * 60 * 1000;
const FAILED_TRANSFER_RETENTION_MS: i64 = 24 * 60 * 60 * 1000;
const TRANSFER_ACTIVITY_TIMEOUT_MS: i64 = 5 * 60 * 1000;
const MAX_TERMINAL_TRANSFERS: usize = 256;
const MAX_FAILED_TRANSFERS: usize = 20;
const MAX_MOBILE_TERMINAL_TRANSFERS_PER_SESSION: usize = 128;
const MAX_MOBILE_FAILED_TRANSFERS_PER_SESSION: usize = 20;
const MAX_MOBILE_TRANSFER_RECORDS: usize = 512;
const MAX_MOBILE_MANIFEST_BYTES: u64 = 128 * 1024 * 1024;
const MAX_MOBILE_CA_BYTES: usize = 64 * 1024;
const MAX_SEEN_CLIPBOARD_EVENTS: usize = 4_096;
const DESKTOP_TEMP_REGISTRY_FILE: &str = "desktop-transfer-temp-files.json";
const MAX_DESKTOP_TEMP_FILES: usize = 512;
const CLIPBOARD_HISTORY_FILE: &str = "clipboard-history.json";
const CLIPBOARD_CURRENT_FILE: &str = "clipboard-current.json";
const CLIPBOARD_PAYLOAD_DIR: &str = "clipboard-history-payloads";
const CLIPBOARD_HISTORY_PREVIEW_BYTES: usize = 256;
const CLIPBOARD_DAY_MS: i64 = 24 * 60 * 60 * 1000;
const MAX_CLIPBOARD_HISTORY_BYTES: u64 = 512 * 1024 * 1024;

fn mobile_view(record: &MobileTransferRecord) -> MobileTransferView {
    let mut downloaded_item_ids = record.downloaded_items.iter().cloned().collect::<Vec<_>>();
    downloaded_item_ids.sort();
    let mut retryable_download_item_ids = record
        .retryable_downloads
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    retryable_download_item_ids.sort();
    MobileTransferView {
        session_id: record.session_id.clone(),
        progress: record.progress.clone(),
        downloaded_item_ids,
        retryable_download_item_ids,
    }
}

fn is_active_transfer(status: &TransferStatus) -> bool {
    matches!(
        status,
        TransferStatus::Waiting
            | TransferStatus::Accepted
            | TransferStatus::Uploading
            | TransferStatus::Ready
    )
}

fn mobile_manifest_size(offer: &TransferOffer) -> u64 {
    serde_json::to_vec(offer)
        .map(|value| value.len() as u64)
        .unwrap_or(u64::MAX)
}

fn is_clipboard_message(message: &WireMessage) -> bool {
    matches!(
        message,
        WireMessage::ClipboardUpdate { .. } | WireMessage::ClipboardSnapshot { .. }
    )
}

impl NodeRuntime {
    pub fn new(data_dir: PathBuf, port: u16, version: String) -> anyhow::Result<Arc<Self>> {
        Self::new_with_profile(data_dir, port, version, NodeProfile::current())
    }

    pub fn new_with_profile(
        data_dir: PathBuf,
        port: u16,
        version: String,
        profile: NodeProfile,
    ) -> anyhow::Result<Arc<Self>> {
        std::fs::create_dir_all(&data_dir)?;
        let staging_root = data_dir.join("mobile-transfers");
        match std::fs::remove_dir_all(&staging_root) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let desktop_temp_files = load_and_cleanup_desktop_temp_files(&data_dir)?;
        cleanup_clipboard_temp_files(&data_dir)?;
        let config = load_or_create_config(&data_dir, profile)?;
        #[cfg(target_os = "macos")]
        let mut config = config;
        #[cfg(target_os = "macos")]
        {
            // Configurations written before bookmark support only contain a
            // path.  Try to migrate that path without opening a new panel;
            // if macOS still requires a user grant, the next explicit folder
            // selection will create the bookmark.
            if config.receive_directory_bookmark.is_none() {
                if let Some(path) = config.receive_directory.as_deref().map(Path::new) {
                    if let Ok(bookmark) = crate::macos_security_scope::create_bookmark(path) {
                        config.receive_directory_bookmark = Some(bookmark);
                        if let Err(error) = persist_config(&data_dir, &config) {
                            eprintln!("保存接收目录授权失败：{error}");
                        }
                    }
                }
            }
            if let Some(path) = activate_receive_directory_access(&config) {
                let resolved = path.to_string_lossy().into_owned();
                if config.receive_directory.as_deref() != Some(resolved.as_str()) {
                    config.receive_directory = Some(resolved);
                    if let Err(error) = persist_config(&data_dir, &config) {
                        eprintln!("更新接收目录路径失败：{error}");
                    }
                }
            }
        }
        let mut clipboard_history = load_clipboard_history(&data_dir)?;
        prune_clipboard_history_records(&mut clipboard_history, &config, now_millis());
        persist_clipboard_history(&data_dir, &clipboard_history)?;
        let clipboard = load_clipboard_current(&data_dir).or_else(|| {
            clipboard_history
                .first()
                .and_then(|record| load_history_snapshot(&data_dir, record).ok())
        });
        let clock = LogicalClock::new(config.node_id.clone());
        if let Some(snapshot) = clipboard.as_ref() {
            clock.observe(&snapshot.clock);
        }
        let node = NodeInfo {
            node_id: config.node_id.clone(),
            node_name: config.node_name.clone(),
            version,
            protocol_version: crate::model::PROTOCOL_VERSION,
            platform: std::env::consts::OS.to_string(),
            port,
            addresses: Vec::new(),
            mobile_addresses: Vec::new(),
            capabilities: profile.capabilities(),
        };
        let (events, _) = broadcast::channel(256);
        Ok(Arc::new(Self {
            profile,
            clock,
            node: RwLock::new(node),
            clipboard: RwLock::new(clipboard),
            clipboard_status: RwLock::new(ClipboardStatus::default()),
            clipboard_history: Mutex::new(clipboard_history),
            peers: RwLock::new(HashMap::new()),
            transfers: RwLock::new(HashMap::new()),
            mobile_sessions: RwLock::new(HashMap::new()),
            mobile_transfers: RwLock::new(HashMap::new()),
            mobile_manifest_bytes: AtomicU64::new(0),
            local_metrics: RwLock::new(None),
            monitor_visible: AtomicBool::new(config.monitor_visible),
            monitor_locked: AtomicBool::new(config.monitor_locked),
            mobile_ca: RwLock::new(None),
            config: Mutex::new(config),
            data_dir,
            clipboard_update: Mutex::new(()),
            seen_events: Mutex::new(HashMap::new()),
            suppression: Mutex::new(None),
            peer_channels: Mutex::new(HashMap::new()),
            pending_clipboard_resync: Mutex::new(HashSet::new()),
            connecting_peers: Mutex::new(HashSet::new()),
            transfer_locks: Mutex::new(HashSet::new()),
            desktop_temp_files: Mutex::new(desktop_temp_files),
            events,
            sync_enabled: AtomicBool::new(true),
            stopping: AtomicBool::new(false),
            update_installing: AtomicBool::new(false),
            http: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(5))
                .timeout(std::time::Duration::from_secs(15))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            proxy_http: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(5))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            transfer_update_gate: Mutex::new(()),
        }))
    }

    pub fn is_android_profile(&self) -> bool {
        self.profile.is_android()
    }

    pub fn supports_capability(&self, capability: &str) -> bool {
        self.node
            .read()
            .capabilities
            .iter()
            .any(|item| item == capability)
    }

    pub fn node_info(&self) -> NodeInfo {
        self.node.read().clone()
    }

    pub fn clipboard_status(&self) -> ClipboardStatus {
        self.clipboard_status.read().clone()
    }

    pub fn set_clipboard_status(&self, status: ClipboardStatus) {
        *self.clipboard_status.write() = status;
    }

    pub fn set_clipboard_available(&self, session: ClipboardSession) {
        self.set_clipboard_status(ClipboardStatus {
            session,
            available: true,
            retrying: false,
            message: None,
        });
    }

    pub fn set_clipboard_unavailable(
        &self,
        session: ClipboardSession,
        error: impl Into<String>,
        retrying: bool,
    ) {
        let detail = error.into();
        let message = match session {
            ClipboardSession::Wayland => format!(
                "Wayland 系统剪贴板不可用：{detail}。请启用 XWayland，或使用提供 data-control 协议的合成器"
            ),
            ClipboardSession::X11 => format!("X11 系统剪贴板不可用：{detail}"),
            ClipboardSession::Other => format!("系统剪贴板不可用：{detail}"),
            ClipboardSession::Unknown => format!("无法连接系统剪贴板：{detail}"),
        };
        self.set_clipboard_status(ClipboardStatus {
            session,
            available: false,
            retrying,
            message: Some(message),
        });
    }

    pub fn node_id(&self) -> String {
        self.node.read().node_id.clone()
    }

    pub fn mobile_access_info(&self) -> MobileAccessInfo {
        let node = self.node.read();
        let ca = self.mobile_ca.read();
        MobileAccessInfo {
            bootstrap_urls: node.addresses.clone(),
            secure_urls: node.mobile_addresses.clone(),
            ca_url: ca
                .as_ref()
                .map_or_else(String::new, |_| "/api/v1/mobile/ca".to_string()),
            ca_fingerprint: ca
                .as_ref()
                .map_or_else(String::new, |ca| ca.fingerprint.clone()),
        }
    }

    pub fn set_mobile_ca(&self, path: PathBuf, fingerprint: String) {
        *self.mobile_ca.write() = Some(MobileCa { path, fingerprint });
    }

    pub fn mobile_ca_path(&self) -> Option<PathBuf> {
        self.mobile_ca.read().as_ref().map(|ca| ca.path.clone())
    }

    /// 只从节点私网地址取 CA；命令层收到的 URL 来自渲染进程，不能让它指向公网或任意资源。
    pub async fn fetch_mobile_ca(&self, raw_url: &str) -> Result<Vec<u8>, String> {
        let url = mobile_ca_url(raw_url)?;
        let mut response = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|error| format!("无法连接节点 CA 地址：{error}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "节点返回 CA 证书失败 ({})",
                response.status().as_u16()
            ));
        }
        if response.content_length().unwrap_or_default() > MAX_MOBILE_CA_BYTES as u64 {
            return Err("节点返回的 CA 证书超出大小限制".to_string());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| format!("读取 CA 证书失败：{error}"))?
        {
            if chunk.len() > MAX_MOBILE_CA_BYTES - bytes.len() {
                return Err("节点返回的 CA 证书超出大小限制".to_string());
            }
            bytes.extend_from_slice(&chunk);
        }
        if bytes.is_empty() {
            return Err("节点返回的 CA 证书为空".to_string());
        }
        Ok(bytes)
    }

    fn mobile_staging_root(&self) -> PathBuf {
        self.data_dir.join("mobile-transfers")
    }

    /// Only return directories that are direct children of the staging root.
    ///
    /// The path is kept in the in-memory record, so this check is intentionally
    /// lexical and rejects the root itself, parent components, and nested paths.
    /// It is the final guard before a cleanup caller performs recursive removal.
    fn is_mobile_staging_directory(&self, directory: &Path) -> bool {
        let Ok(relative) = directory.strip_prefix(self.mobile_staging_root()) else {
            return false;
        };
        let mut components = relative.components();
        matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
    }

    pub fn set_listening_addresses(
        &self,
        http: &BTreeSet<Ipv4Addr>,
        https: impl Iterator<Item = Ipv4Addr>,
        https_port: u16,
    ) {
        let mut node = self.node.write();
        node.addresses = network_addresses_for_bound(node.port, http);
        node.mobile_addresses = https
            .filter(|ip| http.contains(ip) && is_network_ipv4(*ip))
            .map(|ip| AddressInfo {
                address: ip.to_string(),
                url: format!("https://{ip}:{https_port}"),
                label: ip.to_string(),
                kind: "mobile_https".to_string(),
            })
            .collect();
    }

    pub fn publish_event(&self, message: WireMessage) {
        let _ = self.events.send(message);
    }

    pub fn publish_metrics(&self, metrics: HardwareMetrics) {
        let _ = self.events.send(WireMessage::MetricsUpdate {
            metrics: metrics.clone(),
        });
        let peer_capabilities = self
            .peers
            .read()
            .iter()
            .filter(|(_, peer)| peer.online)
            .map(|(node_id, peer)| {
                (
                    node_id.clone(),
                    (
                        peer.capabilities.iter().any(|capability| {
                            capability == METRICS_SYSTEM_CAPABILITY
                                || capability == METRICS_SYSTEM_VIEW_CAPABILITY
                        }),
                        peer.capabilities.iter().any(|capability| {
                            capability == METRICS_GPU_CAPABILITY
                                || capability == METRICS_GPU_VIEW_CAPABILITY
                        }),
                        peer.capabilities
                            .iter()
                            .any(|capability| capability == METRICS_NETWORK_CAPABILITY),
                    ),
                )
            })
            .collect::<HashMap<_, _>>();
        let channels = self
            .peer_channels
            .lock()
            .iter()
            .filter_map(|(node_id, channel)| {
                let (supports_system, supports_gpu, supports_network) =
                    peer_capabilities.get(node_id)?;
                if !*supports_system && !*supports_network {
                    return None;
                }
                let mut outgoing = metrics.clone();
                if !*supports_system {
                    outgoing.cpu_usage_percent = None;
                    outgoing.memory_used_bytes = None;
                    outgoing.memory_total_bytes = None;
                }
                if !*supports_gpu {
                    outgoing.gpus.clear();
                }
                if !*supports_network {
                    outgoing.network_down_bytes_per_sec = None;
                    outgoing.network_up_bytes_per_sec = None;
                }
                Some((
                    channel.clone(),
                    WireMessage::MetricsUpdate { metrics: outgoing },
                ))
            })
            .collect::<Vec<_>>();
        for (channel, message) in channels {
            let _ = channel.try_send(message);
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<WireMessage> {
        self.events.subscribe()
    }

    pub fn add_peer_channel(&self, node_id: String, channel: mpsc::Sender<WireMessage>) -> bool {
        let mut channels = self.peer_channels.lock();
        if channels.contains_key(&node_id) {
            return false;
        }
        channels.insert(node_id.clone(), channel);
        self.connecting_peers.lock().remove(&node_id);
        true
    }

    pub fn has_peer_channel(&self, node_id: &str) -> bool {
        self.peer_channels.lock().contains_key(node_id)
    }

    pub fn remove_peer_channel(&self, node_id: &str) {
        self.peer_channels.lock().remove(node_id);
        self.connecting_peers.lock().remove(node_id);
        self.pending_clipboard_resync.lock().remove(node_id);
    }

    /// Remove a dialer's current channel while retaining its connection slot.
    ///
    /// The discovery task owns that slot across its reconnect backoff.  Keeping
    /// it reserved prevents a burst of mDNS resolutions from starting parallel
    /// sockets and bypassing the exponential retry delay after a disconnect.
    pub fn remove_peer_for_retry(&self, node_id: &str) {
        self.peers.write().remove(node_id);
        self.peer_channels.lock().remove(node_id);
        self.pending_clipboard_resync.lock().remove(node_id);
        self.connecting_peers.lock().insert(node_id.to_string());
    }

    pub fn cancel_peer_connection(&self, node_id: &str) {
        self.connecting_peers.lock().remove(node_id);
    }

    pub fn begin_peer_connection(&self, node_id: &str) -> bool {
        let channels = self.peer_channels.lock();
        if channels.contains_key(node_id) {
            return false;
        }
        drop(channels);
        self.connecting_peers.lock().insert(node_id.to_string())
    }

    pub fn reserve_peer_connection(&self, node_id: &str) {
        self.connecting_peers.lock().insert(node_id.to_string());
    }

    pub fn send_to_peer(&self, node_id: &str, message: WireMessage) -> bool {
        let is_clipboard = is_clipboard_message(&message);
        if is_clipboard && !self.peer_supports_clipboard(node_id) {
            self.pending_clipboard_resync.lock().remove(node_id);
            return true;
        }
        let channel = self.peer_channels.lock().get(node_id).cloned();
        let Some(channel) = channel else {
            return false;
        };
        match channel.try_send(message) {
            Ok(()) => true,
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                if is_clipboard {
                    self.pending_clipboard_resync
                        .lock()
                        .insert(node_id.to_string());
                }
                false
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => false,
        }
    }

    pub async fn send_to_peer_with_retry(&self, node_id: &str, message: WireMessage) -> bool {
        const RETRY_DELAYS: [Duration; 4] = [
            Duration::from_millis(50),
            Duration::from_millis(100),
            Duration::from_millis(250),
            Duration::from_millis(500),
        ];
        for (attempt, delay) in RETRY_DELAYS.iter().enumerate() {
            if attempt > 0 {
                tokio::time::sleep(*delay).await;
            }
            if self.send_to_peer(node_id, message.clone()) {
                return true;
            }
            if !self.has_peer_channel(node_id) {
                return false;
            }
        }
        false
    }

    pub fn send_to_peers_except(&self, except: Option<&str>, message: WireMessage) {
        let is_clipboard = is_clipboard_message(&message);
        let channels = self
            .peer_channels
            .lock()
            .iter()
            .filter(|(node_id, _)| {
                Some(node_id.as_str()) != except
                    && (!is_clipboard || self.peer_supports_clipboard(node_id))
            })
            .map(|(node_id, channel)| (node_id.clone(), channel.clone()))
            .collect::<Vec<_>>();
        for (node_id, channel) in channels {
            match channel.try_send(message.clone()) {
                Ok(()) => {}
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) if is_clipboard => {
                    self.pending_clipboard_resync.lock().insert(node_id);
                }
                Err(_) => {}
            }
        }
    }

    pub fn flush_clipboard_resync(&self, node_id: &str) -> bool {
        if !self.pending_clipboard_resync.lock().contains(node_id) {
            return false;
        }
        if !self.peer_supports_clipboard(node_id) {
            self.pending_clipboard_resync.lock().remove(node_id);
            return false;
        }
        let Some(snapshot) = self.clipboard_snapshot() else {
            self.pending_clipboard_resync.lock().remove(node_id);
            return false;
        };
        let Some(channel) = self.peer_channels.lock().get(node_id).cloned() else {
            return false;
        };
        match channel.try_send(WireMessage::ClipboardSnapshot { snapshot }) {
            Ok(()) => {
                self.pending_clipboard_resync.lock().remove(node_id);
                true
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                self.pending_clipboard_resync.lock().remove(node_id);
                false
            }
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => false,
        }
    }

    pub fn set_peer(&self, peer: PeerInfo) {
        let mut peers = self.peers.write();
        let mut peer = peer;
        if let Some(previous) = peers.get(&peer.node_id) {
            peer.metrics = peer.metrics.or_else(|| previous.metrics.clone());
            peer.metrics_received_at_ms = peer
                .metrics_received_at_ms
                .or(previous.metrics_received_at_ms);
            peer.metrics_stale = previous.metrics_stale;
            // A Hello refreshes advertised addresses, but must not discard
            // the endpoint that carried the currently verified WebSocket
            // channel. Keeping it lets HTTP management follow the interface
            // that was actually reachable.
            if peer.source == "websocket" && previous.endpoint.is_some() {
                peer.endpoint = previous.endpoint.clone();
            }
        }
        peers.insert(peer.node_id.clone(), peer);
        let _ = self.events.send(WireMessage::Hello {
            node: self.node_info(),
        });
    }

    pub fn remove_peer(&self, node_id: &str) {
        self.peers.write().remove(node_id);
        self.remove_peer_channel(node_id);
    }

    pub fn touch_peer(&self, node_id: &str) {
        if let Some(peer) = self.peers.write().get_mut(node_id) {
            peer.online = true;
            peer.last_seen_ms = now_millis();
        }
    }

    pub fn peer_supports(&self, node_id: &str, capability: &str) -> bool {
        self.peers
            .read()
            .get(node_id)
            .is_some_and(|peer| peer.capabilities.iter().any(|item| item == capability))
    }

    /// Empty capability lists belong to legacy peers that predate capability
    /// negotiation. Keep clipboard delivery compatible with those peers while
    /// treating an explicit capability list without `clipboard.text` or
    /// `clipboard.view` as an intentional opt-out.
    pub fn peer_supports_clipboard(&self, node_id: &str) -> bool {
        self.peers.read().get(node_id).is_none_or(|peer| {
            peer.capabilities.is_empty()
                || peer
                    .capabilities
                    .iter()
                    .any(|item| item == "clipboard.text" || item == CLIPBOARD_VIEW_CAPABILITY)
        })
    }

    pub fn peer_name(&self, node_id: &str) -> Option<String> {
        self.peers
            .read()
            .get(node_id)
            .map(|peer| peer.node_name.clone())
    }

    pub fn peer_http_base(&self, node_id: &str) -> Option<String> {
        self.peer_http_bases(node_id).into_iter().next()
    }

    /// Return HTTP bases in the order in which this node has evidence that
    /// they are usable. An endpoint is recorded when a WebSocket channel was
    /// established, so it outranks the peer's advertised interface order.
    pub fn peer_http_bases(&self, node_id: &str) -> Vec<String> {
        let Some(peer) = self.peers.read().get(node_id).cloned() else {
            return Vec::new();
        };
        let mut bases = Vec::new();
        if let Some(endpoint) = peer
            .endpoint
            .as_deref()
            .and_then(http_base_from_peer_endpoint)
        {
            bases.push(endpoint);
        }
        for address in peer.addresses {
            if let Some(base) = normalize_peer_http_base(&address.url) {
                if !bases.iter().any(|candidate| candidate == &base) {
                    bases.push(base);
                }
            }
        }
        bases
    }

    pub fn update_peer_metrics(&self, node_id: &str, metrics: HardwareMetrics) -> bool {
        if metrics.node_id != node_id {
            return false;
        }
        let mut peers = self.peers.write();
        let Some(peer) = peers.get_mut(node_id) else {
            return false;
        };
        if peer
            .metrics
            .as_ref()
            .is_some_and(|current| metrics.sampled_at_ms < current.sampled_at_ms)
        {
            return false;
        }
        peer.metrics = Some(metrics);
        peer.metrics_received_at_ms = Some(now_millis());
        peer.metrics_stale = false;
        peer.online = true;
        true
    }

    pub fn local_metrics(&self) -> Option<HardwareMetrics> {
        self.local_metrics.read().clone()
    }

    pub fn set_local_metrics(&self, metrics: HardwareMetrics) {
        *self.local_metrics.write() = Some(metrics);
    }

    pub fn monitor_visible(&self) -> bool {
        self.monitor_visible.load(Ordering::SeqCst)
    }

    pub fn set_monitor_visible(&self, visible: bool) -> anyhow::Result<()> {
        let mut config = self.config.lock();
        let mut next = config.clone();
        next.monitor_visible = visible;
        persist_config(&self.data_dir, &next)?;
        *config = next;
        self.monitor_visible.store(visible, Ordering::SeqCst);
        Ok(())
    }

    pub fn monitor_locked(&self) -> bool {
        self.monitor_locked.load(Ordering::SeqCst)
    }

    pub fn set_monitor_locked(&self, locked: bool) -> anyhow::Result<()> {
        let mut config = self.config.lock();
        let mut next = config.clone();
        next.monitor_locked = locked;
        persist_config(&self.data_dir, &next)?;
        *config = next;
        self.monitor_locked.store(locked, Ordering::SeqCst);
        Ok(())
    }

    pub fn monitor_position(&self) -> Option<MonitorPosition> {
        self.config.lock().monitor_position.clone()
    }

    pub fn set_monitor_position(&self, position: MonitorPosition) -> anyhow::Result<()> {
        let mut config = self.config.lock();
        let mut next = config.clone();
        next.monitor_position = Some(position);
        persist_config(&self.data_dir, &next)?;
        *config = next;
        Ok(())
    }

    pub fn autostart_enabled(&self) -> bool {
        self.config.lock().autostart_enabled
    }

    pub fn set_autostart_enabled(&self, enabled: bool) -> anyhow::Result<()> {
        let mut config = self.config.lock();
        let mut next = config.clone();
        next.autostart_enabled = enabled;
        persist_config(&self.data_dir, &next)?;
        *config = next;
        Ok(())
    }

    pub fn receive_settings(&self) -> ReceiveSettings {
        let config = self.config.lock();
        ReceiveSettings {
            mode: config.receive_mode,
            default_path: config.receive_directory.clone(),
        }
    }

    pub fn set_receive_settings(
        &self,
        mode: ReceiveMode,
        directory: Option<PathBuf>,
    ) -> anyhow::Result<ReceiveSettings> {
        let canonical_directory = directory
            .as_deref()
            .map(validate_receive_directory)
            .transpose()?;
        if mode == ReceiveMode::Auto && canonical_directory.is_none() {
            anyhow::bail!("自动接收必须设置默认目录");
        }
        let mut config = self.config.lock();
        let mut next = config.clone();
        next.receive_mode = mode;
        next.receive_directory = canonical_directory
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned());
        #[cfg(target_os = "macos")]
        {
            next.receive_directory_bookmark = canonical_directory
                .as_deref()
                .and_then(|path| crate::macos_security_scope::create_bookmark(path).ok());
            if let Some(bookmark) = next.receive_directory_bookmark.as_deref() {
                if let Err(error) = crate::macos_security_scope::activate_bookmark(bookmark) {
                    eprintln!("激活接收目录授权失败：{error}");
                }
            }
        }
        persist_config(&self.data_dir, &next)?;
        *config = next;
        drop(config);
        Ok(self.receive_settings())
    }

    fn default_receive_directory(&self) -> Option<PathBuf> {
        let config = self.config.lock().clone();
        if config.receive_mode != ReceiveMode::Auto {
            return None;
        }
        let path = config.receive_directory.as_deref().map(Path::new)?;
        validate_receive_directory(path).ok()
    }

    pub fn update_settings(&self) -> (UpdateFrequency, Option<i64>, Option<String>) {
        let config = self.config.lock();
        (
            config.update_frequency,
            config.update_last_checked_at_ms,
            config.pending_update_version.clone(),
        )
    }

    pub fn set_update_frequency(&self, frequency: UpdateFrequency) -> anyhow::Result<()> {
        let mut config = self.config.lock();
        let mut next = config.clone();
        next.update_frequency = frequency;
        persist_config(&self.data_dir, &next)?;
        *config = next;
        Ok(())
    }

    pub fn set_update_check_state(
        &self,
        checked_at_ms: Option<i64>,
        pending_version: Option<String>,
    ) -> anyhow::Result<()> {
        let mut config = self.config.lock();
        let mut next = config.clone();
        next.update_last_checked_at_ms = checked_at_ms;
        next.pending_update_version = pending_version;
        persist_config(&self.data_dir, &next)?;
        *config = next;
        Ok(())
    }

    pub fn clipboard_history_settings(&self) -> ClipboardHistorySettings {
        let config = self.config.lock();
        ClipboardHistorySettings {
            mode: config.clipboard_history_mode,
            count: config
                .clipboard_history_count
                .clamp(1, MAX_CLIPBOARD_HISTORY_COUNT),
            days: config
                .clipboard_history_days
                .clamp(1, MAX_CLIPBOARD_HISTORY_DAYS),
        }
    }

    pub fn set_clipboard_history_settings(
        &self,
        mode: ClipboardHistoryMode,
        value: u64,
    ) -> anyhow::Result<ClipboardHistorySettings> {
        let mut config = self.config.lock();
        let mut next = config.clone();
        match mode {
            ClipboardHistoryMode::Count => {
                let count = usize::try_from(value)
                    .ok()
                    .filter(|value| (1..=MAX_CLIPBOARD_HISTORY_COUNT).contains(value))
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "剪贴板历史条数必须在 1 到 {MAX_CLIPBOARD_HISTORY_COUNT} 之间"
                        )
                    })?;
                next.clipboard_history_count = count;
            }
            ClipboardHistoryMode::Days => {
                let days = u32::try_from(value)
                    .ok()
                    .filter(|value| (1..=MAX_CLIPBOARD_HISTORY_DAYS).contains(value))
                    .ok_or_else(|| {
                        anyhow::anyhow!(
                            "剪贴板历史天数必须在 1 到 {MAX_CLIPBOARD_HISTORY_DAYS} 之间"
                        )
                    })?;
                next.clipboard_history_days = days;
            }
        }
        next.clipboard_history_count = next
            .clipboard_history_count
            .clamp(1, MAX_CLIPBOARD_HISTORY_COUNT);
        next.clipboard_history_days = next
            .clipboard_history_days
            .clamp(1, MAX_CLIPBOARD_HISTORY_DAYS);
        next.clipboard_history_mode = mode;
        persist_config(&self.data_dir, &next)?;
        *config = next;
        drop(config);
        self.prune_clipboard_history();
        Ok(self.clipboard_history_settings())
    }

    pub fn prune_clipboard_history(&self) -> usize {
        let _update_guard = self.clipboard_update.lock();
        let mut history = self.clipboard_history.lock();
        let before = history.len();
        let previous = history.clone();
        let config = self.config.lock().clone();
        prune_clipboard_history_records(&mut history, &config, now_millis());
        if history.len() != before {
            if let Err(error) = persist_clipboard_history(&self.data_dir, &history) {
                *history = previous;
                eprintln!("保存剪贴板历史清理结果失败：{error}");
                return 0;
            }
        }
        before.saturating_sub(history.len())
    }

    pub fn clipboard_history_list(&self) -> ClipboardHistoryList {
        let _update_guard = self.clipboard_update.lock();
        let mut history = self.clipboard_history.lock();
        let config = self.config.lock().clone();
        let before = history.len();
        let previous = history.clone();
        prune_clipboard_history_records(&mut history, &config, now_millis());
        if history.len() != before {
            if let Err(error) = persist_clipboard_history(&self.data_dir, &history) {
                *history = previous;
                eprintln!("保存剪贴板历史清理结果失败：{error}");
            }
        }
        ClipboardHistoryList {
            settings: ClipboardHistorySettings {
                mode: config.clipboard_history_mode,
                count: config
                    .clipboard_history_count
                    .clamp(1, MAX_CLIPBOARD_HISTORY_COUNT),
                days: config
                    .clipboard_history_days
                    .clamp(1, MAX_CLIPBOARD_HISTORY_DAYS),
            },
            items: history
                .iter()
                .map(|record| self.history_item(record))
                .collect(),
        }
    }

    pub fn clipboard_history_detail(&self, event_id: &str) -> Option<ClipboardHistoryDetail> {
        let _update_guard = self.clipboard_update.lock();
        let history = self.clipboard_history.lock();
        history
            .iter()
            .find(|record| record.event_id == event_id)
            .and_then(|record| {
                load_history_snapshot(&self.data_dir, record)
                    .ok()
                    .map(|snapshot| ClipboardHistoryDetail {
                        item: self.history_item(record),
                        snapshot,
                    })
            })
    }

    fn history_item(&self, record: &ClipboardHistoryRecord) -> ClipboardHistoryItem {
        let text_preview = record.text_path.as_ref().and_then(|path| {
            let path = self.data_dir.join(CLIPBOARD_PAYLOAD_DIR).join(path);
            let bytes = std::fs::read(path).ok()?;
            let bytes = &bytes[..bytes.len().min(CLIPBOARD_HISTORY_PREVIEW_BYTES)];
            Some(String::from_utf8_lossy(bytes).into_owned())
        });
        ClipboardHistoryItem {
            event_id: record.event_id.clone(),
            source_node_id: record.source_node_id.clone(),
            source_name: record.source_name.clone(),
            updated_at_ms: record.updated_at_ms,
            recorded_at_ms: record.recorded_at_ms,
            has_text: record.text_path.is_some(),
            has_image: record.image_path.is_some(),
            text_preview,
            size_bytes: (record.text_len as u64).saturating_add(record.image_len as u64),
        }
    }

    pub fn clear_clipboard_history(&self) -> anyhow::Result<usize> {
        let _update_guard = self.clipboard_update.lock();
        let mut history = self.clipboard_history.lock();
        let count = history.len();
        let previous = history.clone();
        history.clear();
        if let Err(error) = persist_clipboard_history(&self.data_dir, &history) {
            *history = previous;
            return Err(error);
        }
        Ok(count)
    }

    pub fn has_active_transfers(&self) -> bool {
        self.transfers
            .read()
            .values()
            .any(|record| is_active_transfer(&record.progress.status))
            || self
                .mobile_transfers
                .read()
                .values()
                .any(|record| is_active_transfer(&record.progress.status))
    }

    pub fn begin_update_install(&self) -> bool {
        let _update_gate = self.transfer_update_gate.lock();
        if self.update_installing.load(Ordering::SeqCst) || self.has_active_transfers() {
            return false;
        }
        self.update_installing.store(true, Ordering::SeqCst);
        true
    }

    pub fn end_update_install(&self) {
        let _update_gate = self.transfer_update_gate.lock();
        self.update_installing.store(false, Ordering::SeqCst);
    }

    pub fn peers_snapshot(&self) -> Vec<PeerInfo> {
        let now = now_millis();
        let mut peers = self
            .peers
            .read()
            .values()
            .cloned()
            .map(|mut peer| {
                peer.metrics_stale = peer.metrics_received_at_ms.is_some_and(|received_at| {
                    now.saturating_sub(received_at) > crate::model::METRICS_STALE_AFTER_MS
                });
                peer
            })
            .collect::<Vec<_>>();
        peers.sort_by(|left, right| {
            left.node_name
                .to_lowercase()
                .cmp(&right.node_name.to_lowercase())
        });
        peers
    }

    pub fn accept_clipboard(&self, event: ClipboardEvent, source_name: String) -> bool {
        let _update_guard = self.clipboard_update.lock();
        let now = now_millis();
        let mut seen = self.seen_events.lock();
        seen.retain(|_, timestamp| now.saturating_sub(*timestamp) < 10 * 60 * 1000);
        if seen.contains_key(&event.event_id) {
            return false;
        }
        seen.insert(event.event_id.clone(), now);
        while seen.len() > MAX_SEEN_CLIPBOARD_EVENTS {
            let Some(oldest) = seen
                .iter()
                .min_by_key(|(_, timestamp)| **timestamp)
                .map(|(event_id, _)| event_id.clone())
            else {
                break;
            };
            seen.remove(&oldest);
        }
        let snapshot = ClipboardSnapshot {
            event_id: event.event_id,
            source_node_id: event.origin_node_id,
            source_name,
            clock: event.clock,
            text: event.text,
            image_png: event.image_png,
            updated_at_ms: event.updated_at_ms,
        };
        if let Err(error) = clipboard_snapshot_hash(&snapshot) {
            eprintln!("忽略无效剪贴板内容：{error}");
            return false;
        }
        let current = self.clipboard.read().clone();
        let replayed_current = current
            .as_ref()
            .is_some_and(|current| current.event_id == snapshot.event_id);
        if !replayed_current {
            if let Err(error) = self.upsert_clipboard_history_locked(&snapshot, now) {
                eprintln!("保存剪贴板历史失败：{error}");
            }
        }
        let newer = current
            .as_ref()
            .map(|current| snapshot.clock.compare(&current.clock).is_gt())
            .unwrap_or(true);
        if !newer {
            return false;
        }
        self.clock.observe(&snapshot.clock);
        if let Err(error) = persist_clipboard_current(&self.data_dir, &snapshot) {
            eprintln!("保存当前剪贴板失败：{error}");
        }
        *self.clipboard.write() = Some(snapshot);
        true
    }

    fn upsert_clipboard_history_locked(
        &self,
        snapshot: &ClipboardSnapshot,
        recorded_at_ms: i64,
    ) -> anyhow::Result<()> {
        let content_hash = clipboard_snapshot_hash(snapshot)?;
        let mut history = self.clipboard_history.lock();
        let duplicate_index = history.iter().position(|record| {
            record.event_id == snapshot.event_id || record.content_hash == content_hash
        });
        // Write the replacement payload before changing the index or deleting the
        // old record. A disk or permission failure therefore leaves the old
        // history entry and its payload available for restore.
        let record =
            write_clipboard_history_record(&self.data_dir, snapshot, recorded_at_ms, content_hash)?;
        let new_record = record.clone();
        let mut next = history.clone();
        if let Some(index) = duplicate_index {
            next.remove(index);
        }
        next.insert(0, record);
        let config = self.config.lock().clone();
        prune_clipboard_history_records(&mut next, &config, recorded_at_ms);
        if let Err(error) = persist_clipboard_history(&self.data_dir, &next) {
            // The new payload is not referenced by the previous index. Remove it
            // after the failed index write while preserving the old entry.
            remove_clipboard_record_payloads(&self.data_dir, &new_record);
            return Err(error);
        }
        *history = next;
        Ok(())
    }

    pub fn create_local_clipboard(
        &self,
        text: Option<String>,
        image_png: Option<Vec<u8>>,
    ) -> Option<ClipboardEvent> {
        let source_name = self.node.read().node_name.clone();
        self.create_local_clipboard_named(text, image_png, source_name)
    }

    pub fn create_local_clipboard_named(
        &self,
        text: Option<String>,
        image_png: Option<Vec<u8>>,
        source_name: String,
    ) -> Option<ClipboardEvent> {
        if !self.sync_enabled.load(Ordering::SeqCst) {
            return None;
        }
        if source_name.len() > 256
            || text
                .as_ref()
                .is_some_and(|value| value.len() > crate::model::CLIPBOARD_TEXT_LIMIT)
        {
            return None;
        }
        let image_png = image_png
            .filter(|value| value.len() <= crate::model::CLIPBOARD_IMAGE_LIMIT)
            .map(|value| BASE64.encode(value));
        if text.is_none() && image_png.is_none() {
            return None;
        }
        let clock = self.clock.tick();
        let event = ClipboardEvent {
            event_id: Uuid::new_v4().to_string(),
            origin_node_id: self.node_id(),
            clock,
            text,
            image_png,
            updated_at_ms: now_millis(),
        };
        self.accept_clipboard(event.clone(), source_name)
            .then_some(event)
    }

    pub fn restore_clipboard_history(&self, event_id: &str) -> Option<ClipboardEvent> {
        let detail = self.clipboard_history_detail(event_id)?;
        let image = detail
            .snapshot
            .image_png
            .as_deref()
            .and_then(|value| BASE64.decode(value).ok());
        self.create_local_clipboard_named(
            detail.snapshot.text,
            image,
            self.node.read().node_name.clone(),
        )
    }

    pub fn clipboard_snapshot(&self) -> Option<ClipboardSnapshot> {
        self.clipboard.read().clone()
    }

    pub fn set_clipboard_suppression(&self, hash: String) {
        *self.suppression.lock() = Some((hash, now_millis() + 2_000));
    }

    pub fn is_clipboard_suppressed(&self, hash: &str) -> bool {
        let now = now_millis();
        let mut suppression = self.suppression.lock();
        if let Some((expected, expires_at)) = suppression.as_ref() {
            if *expires_at >= now {
                return expected == hash;
            }
        }
        *suppression = None;
        false
    }

    pub fn add_transfer(&self, offer: TransferOffer) -> bool {
        let _update_gate = self.transfer_update_gate.lock();
        if self.update_installing.load(Ordering::SeqCst) {
            return false;
        }
        let mut progress = transfer_progress(&offer, "receive");
        let accepted_destination = self.default_receive_directory();
        if let Some(destination) = accepted_destination.as_ref() {
            progress.destination = Some(destination.to_string_lossy().into_owned());
            progress.status = TransferStatus::Accepted;
        }
        let mut transfers = self.transfers.write();
        if transfers.contains_key(&offer.transfer_id) {
            return false;
        }
        if !evict_terminal_transfer_if_needed(&mut transfers) {
            return false;
        }
        transfers.insert(
            offer.transfer_id.clone(),
            TransferRecord {
                offer: offer.clone(),
                progress: progress.clone(),
                accepted_destination,
                received_items: HashSet::new(),
                updated_at_ms: now_millis(),
            },
        );
        drop(transfers);
        // Progress carries the offer as well. Publish it first so an
        // auto-accepted transfer cannot briefly appear as a new confirmation
        // request in a UI that receives the two events at different times.
        self.publish_event(WireMessage::TransferProgress { progress });
        self.publish_event(WireMessage::TransferOffer { offer });
        true
    }

    pub fn add_outgoing_transfer(&self, offer: TransferOffer) -> TransferProgress {
        let _update_gate = self.transfer_update_gate.lock();
        if self.update_installing.load(Ordering::SeqCst) {
            let mut failed = transfer_progress(&offer, "send");
            failed.status = TransferStatus::Failed;
            failed.error = Some(UPDATE_INSTALLING_MESSAGE.to_string());
            return failed;
        }
        let mut transfers = self.transfers.write();
        if let Some(record) = transfers.get(&offer.transfer_id) {
            return record.progress.clone();
        }
        let progress = transfer_progress(&offer, "send");
        if !evict_terminal_transfer_if_needed(&mut transfers) {
            let mut failed = progress;
            failed.status = TransferStatus::Failed;
            failed.error = Some("节点传输记录资源暂时不足，请稍后重试".to_string());
            return failed;
        }
        transfers.insert(
            offer.transfer_id.clone(),
            TransferRecord {
                offer,
                progress: progress.clone(),
                accepted_destination: None,
                received_items: HashSet::new(),
                updated_at_ms: now_millis(),
            },
        );
        drop(transfers);
        self.publish_event(WireMessage::TransferProgress {
            progress: progress.clone(),
        });
        progress
    }

    pub fn update_transfer(&self, transfer_id: &str, progress: TransferProgress) {
        let _update_gate = self.transfer_update_gate.lock();
        if is_active_transfer(&progress.status) && self.update_installing.load(Ordering::SeqCst) {
            return;
        }
        if let Some(record) = self.transfers.write().get_mut(transfer_id) {
            let mut progress = progress;
            progress.offer = Some(record.offer.clone());
            progress.direction = if record.progress.direction.as_deref() == Some("send") {
                Some("send".to_string())
            } else {
                record.progress.direction.clone()
            };
            record.progress = progress.clone();
            record.updated_at_ms = now_millis();
            self.publish_event(WireMessage::TransferProgress { progress });
            return;
        }
        self.publish_event(WireMessage::TransferProgress { progress });
    }

    pub fn transfer(&self, transfer_id: &str) -> Option<TransferRecord> {
        self.transfers.read().get(transfer_id).cloned()
    }

    pub fn set_transfer_acceptance(
        &self,
        transfer_id: &str,
        destination: PathBuf,
    ) -> Option<TransferProgress> {
        let mut transfers = self.transfers.write();
        let record = transfers.get_mut(transfer_id)?;
        if !matches!(record.progress.status, TransferStatus::Waiting) {
            return None;
        }
        record.accepted_destination = Some(destination.clone());
        record.updated_at_ms = now_millis();
        record.progress.destination = Some(destination.to_string_lossy().to_string());
        record.progress.status = TransferStatus::Accepted;
        record.progress.offer = Some(record.offer.clone());
        Some(record.progress.clone())
    }

    pub fn mark_item_received(
        &self,
        transfer_id: &str,
        item_id: &str,
        bytes: u64,
    ) -> Option<TransferProgress> {
        let mut transfers = self.transfers.write();
        let record = transfers.get_mut(transfer_id)?;
        if matches!(
            record.progress.status,
            TransferStatus::Completed
                | TransferStatus::Ready
                | TransferStatus::Rejected
                | TransferStatus::Failed
                | TransferStatus::Cancelled
        ) {
            return None;
        }
        let item = record
            .offer
            .items
            .iter()
            .find(|item| item.item_id == item_id)?;
        if item.size != bytes {
            return None;
        }
        if record.received_items.insert(item_id.to_string()) {
            record.progress.completed_items = record.progress.completed_items.saturating_add(1);
            record.progress.completed_bytes = record.progress.completed_bytes.saturating_add(bytes);
        }
        record.updated_at_ms = now_millis();
        if record.progress.completed_items >= record.progress.total_items {
            record.progress.status = TransferStatus::Completed;
        } else {
            record.progress.status = TransferStatus::Uploading;
        }
        record.progress.offer = Some(record.offer.clone());
        Some(record.progress.clone())
    }

    /// Return the current progress when an upload item was already committed.
    ///
    /// A sender may lose the HTTP response after the receiver has committed a
    /// file and then retry the same PUT. Keeping this check in the runtime
    /// makes that retry observable without incrementing transfer progress a
    /// second time.
    pub fn transfer_item_already_received(
        &self,
        transfer_id: &str,
        item_id: &str,
    ) -> Option<TransferProgress> {
        let record = self.transfers.read().get(transfer_id)?.clone();
        record
            .received_items
            .contains(item_id)
            .then_some(record.progress)
    }

    pub fn fail_transfer(
        &self,
        transfer_id: &str,
        message: String,
        status: TransferStatus,
    ) -> Option<TransferProgress> {
        let mut transfers = self.transfers.write();
        let record = transfers.get_mut(transfer_id)?;
        if matches!(
            record.progress.status,
            TransferStatus::Completed
                | TransferStatus::Ready
                | TransferStatus::Rejected
                | TransferStatus::Failed
                | TransferStatus::Cancelled
        ) {
            return Some(record.progress.clone());
        }
        record.progress.status = status;
        record.progress.error = Some(message);
        record.updated_at_ms = now_millis();
        record.progress.offer = Some(record.offer.clone());
        Some(record.progress.clone())
    }

    pub fn retry_transfer(
        &self,
        transfer_id: &str,
        status: TransferStatus,
        expected_retry_count: Option<u32>,
    ) -> Result<TransferProgress, TransferRetryError> {
        if !matches!(status, TransferStatus::Waiting | TransferStatus::Accepted) {
            return Err(TransferRetryError::NotRetryable);
        }
        let _update_gate = self.transfer_update_gate.lock();
        if self.update_installing.load(Ordering::SeqCst) {
            return Err(TransferRetryError::InstallingUpdate);
        }
        let mut transfers = self.transfers.write();
        let record = transfers
            .get_mut(transfer_id)
            .ok_or(TransferRetryError::NotRetryable)?;
        if record.progress.status != TransferStatus::Failed {
            return Err(TransferRetryError::NotRetryable);
        }
        let retry_count = record
            .progress
            .retry_count
            .checked_add(1)
            .ok_or(TransferRetryError::NotRetryable)?;
        if expected_retry_count.is_some_and(|expected| expected != retry_count) {
            return Err(TransferRetryError::NotRetryable);
        }
        if status == TransferStatus::Waiting {
            record.accepted_destination = None;
            record.progress.destination = None;
        }
        record.progress.retry_count = retry_count;
        record.progress.status = status;
        record.progress.error = None;
        record.updated_at_ms = now_millis();
        record.progress.offer = Some(record.offer.clone());
        Ok(record.progress.clone())
    }

    /// Remove a failed transfer record. Non-failed records are retained and
    /// rejected so an active task cannot be dropped without cancelling it.
    pub fn remove_failed_transfer(&self, transfer_id: &str) -> Option<TransferProgress> {
        let mut transfers = self.transfers.write();
        if transfers.get(transfer_id)?.progress.status != TransferStatus::Failed {
            return None;
        }
        transfers.remove(transfer_id).map(|record| record.progress)
    }

    pub fn transfers_snapshot(&self, status: Option<TransferStatus>) -> Vec<TransferProgress> {
        self.prune_transfers();
        let mut transfers = self
            .transfers
            .read()
            .values()
            .filter(|record| {
                status
                    .as_ref()
                    .is_none_or(|value| &record.progress.status == value)
            })
            .map(|record| record.progress.clone())
            .collect::<Vec<_>>();
        transfers.sort_by_key(|progress| progress.transfer_id.clone());
        transfers
    }

    pub fn prune_transfers(&self) {
        let now = now_millis();
        let mut transfers = self.transfers.write();
        let terminal = |status: &TransferStatus| {
            matches!(
                status,
                TransferStatus::Completed
                    | TransferStatus::Rejected
                    | TransferStatus::Failed
                    | TransferStatus::Cancelled
            )
        };
        let mut stale = Vec::new();
        for record in transfers.values_mut() {
            if !terminal(&record.progress.status)
                && now.saturating_sub(record.updated_at_ms) > TRANSFER_ACTIVITY_TIMEOUT_MS
            {
                record.progress.status = TransferStatus::Failed;
                record.progress.error = Some("传输长时间没有进展，已自动失败".to_string());
                record.progress.offer = Some(record.offer.clone());
                record.updated_at_ms = now;
                stale.push(record.progress.clone());
            }
        }
        transfers.retain(|_, record| {
            !terminal(&record.progress.status)
                || now.saturating_sub(record.updated_at_ms)
                    <= if record.progress.status == TransferStatus::Failed {
                        FAILED_TRANSFER_RETENTION_MS
                    } else {
                        TRANSFER_RETENTION_MS
                    }
        });
        let mut terminal_ids = transfers
            .iter()
            .filter(|(_, record)| terminal(&record.progress.status))
            .map(|(id, record)| (id.clone(), record.updated_at_ms))
            .collect::<Vec<_>>();
        terminal_ids.sort_by_key(|(_, created_at)| *created_at);
        let remove_count = terminal_ids.len().saturating_sub(MAX_TERMINAL_TRANSFERS);
        for (transfer_id, _) in terminal_ids.into_iter().take(remove_count) {
            transfers.remove(&transfer_id);
        }
        let mut failed_ids = transfers
            .iter()
            .filter(|(_, record)| record.progress.status == TransferStatus::Failed)
            .map(|(id, record)| (id.clone(), record.updated_at_ms))
            .collect::<Vec<_>>();
        failed_ids.sort_by_key(|(_, updated_at)| *updated_at);
        let failed_remove_count = failed_ids.len().saturating_sub(MAX_FAILED_TRANSFERS);
        for (transfer_id, _) in failed_ids.into_iter().take(failed_remove_count) {
            if transfers
                .get(&transfer_id)
                .is_some_and(|record| record.progress.status == TransferStatus::Failed)
            {
                transfers.remove(&transfer_id);
            }
        }
        drop(transfers);
        for progress in stale {
            self.publish_event(WireMessage::TransferProgress { progress });
        }
    }

    pub fn transfer_source_id(&self, transfer_id: &str) -> Option<String> {
        self.transfers
            .read()
            .get(transfer_id)
            .map(|record| record.offer.source_node_id.clone())
    }

    pub fn transfer_peer_ids(&self, transfer_id: &str) -> Option<(String, String)> {
        self.transfers.read().get(transfer_id).map(|record| {
            (
                record.offer.source_node_id.clone(),
                record.offer.target_node_id.clone(),
            )
        })
    }

    pub fn mark_transfer_accepted(&self, transfer_id: &str) -> Option<TransferProgress> {
        let mut transfers = self.transfers.write();
        let record = transfers.get_mut(transfer_id)?;
        if !matches!(record.progress.status, TransferStatus::Waiting) {
            return Some(record.progress.clone());
        }
        record.progress.status = TransferStatus::Accepted;
        record.updated_at_ms = now_millis();
        record.progress.offer = Some(record.offer.clone());
        Some(record.progress.clone())
    }

    pub fn try_lock_transfer_item(&self, transfer_id: &str, item_id: &str) -> bool {
        self.transfer_locks
            .lock()
            .insert((transfer_id.to_string(), item_id.to_string()))
    }

    pub fn unlock_transfer_item(&self, transfer_id: &str, item_id: &str) {
        self.transfer_locks
            .lock()
            .remove(&(transfer_id.to_string(), item_id.to_string()));
    }

    pub fn register_desktop_temp_file(&self, path: &Path) -> anyhow::Result<()> {
        if !is_desktop_temp_file_path(path) {
            return Err(anyhow::anyhow!("桌面临时文件路径无效"));
        }
        let mut files = self.desktop_temp_files.lock();
        if files.contains(path) {
            return Ok(());
        }
        if files.len() >= MAX_DESKTOP_TEMP_FILES {
            return Err(anyhow::anyhow!("桌面临时文件登记数量已达到上限"));
        }
        let mut next = files.clone();
        next.insert(path.to_path_buf());
        persist_desktop_temp_files(&self.data_dir, &next)?;
        *files = next;
        Ok(())
    }

    pub fn unregister_desktop_temp_file(&self, path: &Path) {
        let mut files = self.desktop_temp_files.lock();
        if !files.remove(path) {
            return;
        }
        if let Err(error) = persist_desktop_temp_files(&self.data_dir, &files) {
            eprintln!("保存桌面临时文件登记失败：{error}");
        }
    }

    pub fn transfer_upload_active(&self, transfer_id: &str) -> bool {
        self.transfers
            .read()
            .get(transfer_id)
            .is_some_and(|record| {
                matches!(
                    record.progress.status,
                    TransferStatus::Accepted | TransferStatus::Uploading
                )
            })
    }

    pub fn begin_transfer_upload(&self, transfer_id: &str) -> Option<TransferProgress> {
        let mut transfers = self.transfers.write();
        let record = transfers.get_mut(transfer_id)?;
        if !matches!(
            record.progress.status,
            TransferStatus::Accepted | TransferStatus::Uploading
        ) {
            return None;
        }
        record.progress.status = TransferStatus::Uploading;
        record.updated_at_ms = now_millis();
        record.progress.offer = Some(record.offer.clone());
        Some(record.progress.clone())
    }

    pub fn touch_transfer_activity(&self, transfer_id: &str) -> bool {
        let mut transfers = self.transfers.write();
        let Some(record) = transfers.get_mut(transfer_id) else {
            return false;
        };
        if !matches!(
            record.progress.status,
            TransferStatus::Accepted | TransferStatus::Uploading
        ) {
            return false;
        }
        record.updated_at_ms = now_millis();
        true
    }

    pub fn create_mobile_session(&self, label: String) -> Result<MobileSession, String> {
        let now = now_millis();
        self.prune_expired_mobile_sessions_at(now);
        let mut sessions = self.mobile_sessions.write();
        if sessions.len() >= MAX_MOBILE_SESSIONS {
            return Err("手机会话数量已达到上限".to_string());
        }
        let session = MobileSession {
            session_id: Uuid::new_v4().to_string(),
            node_id: self.node_id(),
            node_name: self.node_info().node_name,
            label,
            created_at_ms: now,
            last_seen_ms: now,
        };
        sessions.insert(session.session_id.clone(), session.clone());
        Ok(session)
    }

    pub fn touch_mobile_session(&self, session_id: &str) -> Option<MobileSession> {
        let now = now_millis();
        self.prune_expired_mobile_sessions_at(now);
        let mut sessions = self.mobile_sessions.write();
        let session = sessions.get_mut(session_id)?;
        session.last_seen_ms = now;
        Some(session.clone())
    }

    pub fn prune_expired_mobile_sessions(&self) {
        self.prune_expired_mobile_sessions_at(now_millis());
    }

    fn prune_expired_mobile_sessions_at(&self, now: i64) {
        self.mobile_sessions
            .write()
            .retain(|_, session| now.saturating_sub(session.last_seen_ms) <= MOBILE_SESSION_TTL_MS);
    }

    pub fn mobile_sessions_snapshot(&self) -> Vec<MobileSession> {
        self.prune_expired_mobile_sessions();
        let sessions = self.mobile_sessions.read();
        let mut result = sessions.values().cloned().collect::<Vec<_>>();
        result.sort_by_key(|session| session.created_at_ms);
        result
    }

    pub fn create_mobile_transfer(
        &self,
        session_id: &str,
        offer: TransferOffer,
    ) -> Result<MobileTransferView, String> {
        if self.touch_mobile_session(session_id).is_none() {
            return Err("手机会话已过期".to_string());
        }
        let _update_gate = self.transfer_update_gate.lock();
        if self.update_installing.load(Ordering::SeqCst) {
            return Err(UPDATE_INSTALLING_MESSAGE.to_string());
        }
        let manifest_bytes = mobile_manifest_size(&offer);
        if self
            .mobile_manifest_bytes
            .load(Ordering::SeqCst)
            .checked_add(manifest_bytes)
            .is_none_or(|total| total > MAX_MOBILE_MANIFEST_BYTES)
        {
            return Err("手机传输清单资源暂时不足，请稍后重试".to_string());
        }
        let mut transfers = self.mobile_transfers.write();
        if transfers
            .values()
            .any(|record| record.offer.transfer_id == offer.transfer_id)
        {
            return Err("手机传输任务已存在".to_string());
        }
        if transfers.len() >= MAX_MOBILE_TRANSFER_RECORDS {
            return Err("手机传输记录资源暂时不足，请稍后重试".to_string());
        }
        let progress = transfer_progress(&offer, "receive");
        // `transfer_id` is supplied by the peer and is also exposed in the
        // HTTP API.  Keep it as the protocol key, but never use it as a path
        // component: a fresh UUID gives every transfer an opaque staging
        // directory with no traversal or root-directory collision risk.
        let staged_directory = self.mobile_staging_root().join(Uuid::new_v4().to_string());
        let record = MobileTransferRecord {
            session_id: session_id.to_string(),
            offer,
            progress,
            staged_directory,
            staged_items: HashSet::new(),
            downloaded_items: HashSet::new(),
            retryable_downloads: HashSet::new(),
            updated_at_ms: now_millis(),
        };
        let view = mobile_view(&record);
        transfers.insert(record.offer.transfer_id.clone(), record);
        self.mobile_manifest_bytes
            .fetch_add(manifest_bytes, Ordering::SeqCst);
        Ok(view)
    }

    pub fn mobile_transfers_snapshot(&self, session_id: Option<&str>) -> Vec<MobileTransferView> {
        let mut transfers = self
            .mobile_transfers
            .read()
            .values()
            .filter(|record| {
                session_id
                    .map(|value| value == record.session_id)
                    .unwrap_or(true)
            })
            .map(mobile_view)
            .collect::<Vec<_>>();
        transfers.sort_by_key(|view| view.progress.transfer_id.clone());
        transfers
    }

    pub fn touch_mobile_session_transfers(&self, session_id: &str) {
        let now = now_millis();
        let mut transfers = self.mobile_transfers.write();
        for record in transfers.values_mut() {
            if record.session_id == session_id
                && matches!(
                    record.progress.status,
                    TransferStatus::Waiting
                        | TransferStatus::Accepted
                        | TransferStatus::Uploading
                        | TransferStatus::Ready
                )
            {
                record.updated_at_ms = now;
            }
        }
    }

    pub fn touch_mobile_transfer_activity(&self, transfer_id: &str) -> bool {
        let mut transfers = self.mobile_transfers.write();
        let Some(record) = transfers.get_mut(transfer_id) else {
            return false;
        };
        let downloading = matches!(
            record.progress.status,
            TransferStatus::Accepted | TransferStatus::Uploading | TransferStatus::Ready
        ) || (record.progress.status == TransferStatus::Completed
            && !record.retryable_downloads.is_empty());
        if !downloading {
            return false;
        }
        record.updated_at_ms = now_millis();
        true
    }

    pub fn prune_mobile_transfers(&self) -> Vec<PathBuf> {
        let sessions = self.mobile_sessions.read();
        let mut transfers = self.mobile_transfers.write();
        let now = now_millis();
        let terminal = |status: &TransferStatus| {
            matches!(
                status,
                TransferStatus::Completed
                    | TransferStatus::Rejected
                    | TransferStatus::Failed
                    | TransferStatus::Cancelled
            )
        };
        let mut cleanup = transfers
            .values_mut()
            .filter(|record| {
                sessions.contains_key(&record.session_id)
                    && !terminal(&record.progress.status)
                    && now.saturating_sub(record.updated_at_ms) > TRANSFER_ACTIVITY_TIMEOUT_MS
            })
            .map(|record| {
                record.progress.status = TransferStatus::Failed;
                record.progress.error = Some("手机传输长时间没有进展，已自动失败".to_string());
                record.updated_at_ms = now;
                record.staged_directory.clone()
            })
            .collect::<Vec<_>>();
        let mut remove = transfers
            .iter()
            .filter(|(_, record)| {
                (!sessions.contains_key(&record.session_id)
                    && record.progress.status != TransferStatus::Failed)
                    || (terminal(&record.progress.status)
                        && now.saturating_sub(record.updated_at_ms)
                            > if record.progress.status == TransferStatus::Failed {
                                FAILED_TRANSFER_RETENTION_MS
                            } else {
                                TRANSFER_RETENTION_MS
                            })
            })
            .map(|(transfer_id, record)| (transfer_id.clone(), record.staged_directory.clone()))
            .collect::<Vec<_>>();
        let mut terminal_by_session = HashMap::<String, Vec<(String, i64, PathBuf)>>::new();
        for (transfer_id, record) in transfers.iter() {
            if sessions.contains_key(&record.session_id) && terminal(&record.progress.status) {
                terminal_by_session
                    .entry(record.session_id.clone())
                    .or_default()
                    .push((
                        transfer_id.clone(),
                        record.updated_at_ms,
                        record.staged_directory.clone(),
                    ));
            }
        }
        for records in terminal_by_session.values_mut() {
            records.sort_by_key(|(_, created_at, _)| *created_at);
            let count = records
                .len()
                .saturating_sub(MAX_MOBILE_TERMINAL_TRANSFERS_PER_SESSION);
            remove.extend(
                records
                    .iter()
                    .take(count)
                    .map(|(transfer_id, _, directory)| (transfer_id.clone(), directory.clone())),
            );
        }
        for records in terminal_by_session.values_mut() {
            records.retain(|(transfer_id, _, _)| {
                transfers
                    .get(transfer_id)
                    .is_some_and(|record| record.progress.status == TransferStatus::Failed)
            });
            records.sort_by_key(|(_, updated_at, _)| *updated_at);
            let count = records
                .len()
                .saturating_sub(MAX_MOBILE_FAILED_TRANSFERS_PER_SESSION);
            remove.extend(
                records
                    .iter()
                    .take(count)
                    .map(|(transfer_id, _, directory)| (transfer_id.clone(), directory.clone())),
            );
        }
        let mut failed_ids = transfers
            .iter()
            .filter(|(_, record)| record.progress.status == TransferStatus::Failed)
            .map(|(transfer_id, record)| {
                (
                    transfer_id.clone(),
                    record.updated_at_ms,
                    record.staged_directory.clone(),
                )
            })
            .collect::<Vec<_>>();
        failed_ids.sort_by_key(|(_, updated_at, _)| *updated_at);
        let failed_remove_count = failed_ids
            .len()
            .saturating_sub(MAX_MOBILE_FAILED_TRANSFERS_PER_SESSION);
        remove.extend(
            failed_ids
                .into_iter()
                .take(failed_remove_count)
                .map(|(transfer_id, _, directory)| (transfer_id, directory)),
        );
        remove.sort_by(|left, right| left.0.cmp(&right.0));
        remove.dedup_by(|left, right| left.0 == right.0);
        let mut released_manifest_bytes = 0u64;
        for (transfer_id, _) in &remove {
            if let Some(record) = transfers.remove(transfer_id) {
                released_manifest_bytes =
                    released_manifest_bytes.saturating_add(mobile_manifest_size(&record.offer));
            }
        }
        if released_manifest_bytes > 0 {
            self.mobile_manifest_bytes
                .fetch_sub(released_manifest_bytes, Ordering::SeqCst);
        }
        cleanup.extend(remove.into_iter().map(|(_, directory)| directory));
        cleanup
            .into_iter()
            .filter_map(|directory| {
                self.is_mobile_staging_directory(&directory)
                    .then_some(directory)
            })
            .collect()
    }

    pub fn mobile_transfer(&self, transfer_id: &str) -> Option<MobileTransferRecord> {
        self.mobile_transfers.read().get(transfer_id).cloned()
    }

    /// Remove a failed mobile transfer record and release its manifest budget.
    pub fn remove_failed_mobile_transfer(&self, transfer_id: &str) -> Option<MobileTransferRecord> {
        let mut transfers = self.mobile_transfers.write();
        if transfers.get(transfer_id)?.progress.status != TransferStatus::Failed {
            return None;
        }
        let record = transfers.remove(transfer_id)?;
        let manifest_bytes = mobile_manifest_size(&record.offer);
        if manifest_bytes > 0 {
            self.mobile_manifest_bytes
                .fetch_sub(manifest_bytes, Ordering::SeqCst);
        }
        Some(record)
    }

    pub fn set_mobile_transfer_status(
        &self,
        transfer_id: &str,
        session_id: &str,
        status: TransferStatus,
    ) -> Option<MobileTransferView> {
        let mut transfers = self.mobile_transfers.write();
        let record = transfers.get_mut(transfer_id)?;
        if record.session_id != session_id {
            return None;
        }
        if !matches!(status, TransferStatus::Accepted | TransferStatus::Rejected) {
            return None;
        }
        if !matches!(record.progress.status, TransferStatus::Waiting) {
            return Some(mobile_view(record));
        }
        record.progress.status = status.clone();
        record.updated_at_ms = now_millis();
        if status == TransferStatus::Rejected {
            record.progress.error = Some("手机端拒绝了传输".to_string());
        }
        Some(mobile_view(record))
    }

    pub fn cancel_mobile_transfer(
        &self,
        transfer_id: &str,
        session_id: &str,
    ) -> Option<MobileTransferView> {
        let mut transfers = self.mobile_transfers.write();
        let record = transfers.get_mut(transfer_id)?;
        if record.session_id != session_id {
            return None;
        }
        if matches!(
            record.progress.status,
            TransferStatus::Completed
                | TransferStatus::Rejected
                | TransferStatus::Failed
                | TransferStatus::Cancelled
        ) {
            return Some(mobile_view(record));
        }
        record.progress.status = TransferStatus::Cancelled;
        record.progress.error = Some("手机传输已取消".to_string());
        record.updated_at_ms = now_millis();
        Some(mobile_view(record))
    }

    pub fn stage_mobile_item(
        &self,
        transfer_id: &str,
        item_id: &str,
        bytes: u64,
    ) -> Option<MobileTransferView> {
        let mut transfers = self.mobile_transfers.write();
        let record = transfers.get_mut(transfer_id)?;
        if !matches!(
            record.progress.status,
            TransferStatus::Accepted | TransferStatus::Uploading
        ) {
            return None;
        }
        let item = record
            .offer
            .items
            .iter()
            .find(|item| item.item_id == item_id)?;
        if item.size != bytes {
            return None;
        }
        if record.staged_items.insert(item_id.to_string()) {
            record.progress.completed_items = record.progress.completed_items.saturating_add(1);
            record.progress.completed_bytes = record.progress.completed_bytes.saturating_add(bytes);
        }
        record.updated_at_ms = now_millis();
        record.progress.status = if record.progress.completed_items >= record.progress.total_items {
            TransferStatus::Ready
        } else {
            TransferStatus::Uploading
        };
        Some(mobile_view(record))
    }

    pub fn begin_mobile_upload(&self, transfer_id: &str) -> Option<MobileTransferView> {
        let mut transfers = self.mobile_transfers.write();
        let record = transfers.get_mut(transfer_id)?;
        if !matches!(
            record.progress.status,
            TransferStatus::Accepted | TransferStatus::Uploading
        ) {
            return None;
        }
        record.progress.status = TransferStatus::Uploading;
        record.updated_at_ms = now_millis();
        Some(mobile_view(record))
    }

    pub fn mark_mobile_item_downloaded(
        &self,
        transfer_id: &str,
        item_id: &str,
        keep_for_retry: bool,
    ) -> Option<MobileTransferView> {
        let mut transfers = self.mobile_transfers.write();
        let record = transfers.get_mut(transfer_id)?;
        if !matches!(
            record.progress.status,
            TransferStatus::Ready | TransferStatus::Completed
        ) || !record.staged_items.contains(item_id)
        {
            return None;
        }
        let first_download = record.downloaded_items.insert(item_id.to_string());
        if keep_for_retry {
            if first_download {
                record.retryable_downloads.insert(item_id.to_string());
            }
        } else {
            record.retryable_downloads.remove(item_id);
        }
        record.updated_at_ms = now_millis();
        if record.downloaded_items.len() >= record.progress.total_items {
            record.progress.status = TransferStatus::Completed;
        }
        Some(mobile_view(record))
    }

    pub fn fail_mobile_transfer(
        &self,
        transfer_id: &str,
        message: String,
    ) -> Option<MobileTransferView> {
        let mut transfers = self.mobile_transfers.write();
        let record = transfers.get_mut(transfer_id)?;
        if matches!(
            record.progress.status,
            TransferStatus::Ready
                | TransferStatus::Completed
                | TransferStatus::Rejected
                | TransferStatus::Failed
                | TransferStatus::Cancelled
        ) {
            return Some(mobile_view(record));
        }
        record.progress.status = TransferStatus::Failed;
        record.progress.error = Some(message);
        record.updated_at_ms = now_millis();
        Some(mobile_view(record))
    }

    pub fn mobile_item_path(&self, transfer_id: &str, item_id: &str) -> Option<PathBuf> {
        let record = self.mobile_transfers.read().get(transfer_id)?.clone();
        if !record.staged_items.contains(item_id) {
            return None;
        }
        let item = record
            .offer
            .items
            .iter()
            .find(|item| item.item_id == item_id)?;
        Some(record.staged_directory.join(&item.relative_path))
    }

    pub fn mobile_item_already_staged(
        &self,
        transfer_id: &str,
        item_id: &str,
    ) -> Option<MobileTransferView> {
        let record = self.mobile_transfers.read().get(transfer_id)?.clone();
        record
            .staged_items
            .contains(item_id)
            .then(|| mobile_view(&record))
    }
}

fn evict_terminal_transfer_if_needed(transfers: &mut HashMap<String, TransferRecord>) -> bool {
    if transfers.len() < MAX_TRANSFER_RECORDS {
        return true;
    }
    let terminal_id = transfers
        .iter()
        .filter(|(_, record)| {
            matches!(
                record.progress.status,
                TransferStatus::Completed
                    | TransferStatus::Rejected
                    | TransferStatus::Failed
                    | TransferStatus::Cancelled
            )
        })
        .min_by_key(|(_, record)| record.updated_at_ms)
        .map(|(transfer_id, _)| transfer_id.clone());
    if let Some(transfer_id) = terminal_id {
        transfers.remove(&transfer_id);
        true
    } else {
        false
    }
}

const ANDROID_DEFAULT_NODE_NAME: &str = "移动设备";
const DESKTOP_DEFAULT_NODE_NAME: &str = "MeshShare 节点";

fn system_hostname() -> Option<String> {
    hostname::get()
        .ok()
        .and_then(|value| value.into_string().ok())
}

/// 主机名只是系统占位值时不能当设备名：Android 的 net.hostname 默认就是 localhost，
/// 照抄会让每台手机都以同一个名字出现在其他节点的设备列表里。
fn is_placeholder_node_name(name: &str) -> bool {
    let name = name.trim().to_ascii_lowercase();
    name.is_empty()
        || matches!(
            name.as_str(),
            "localhost" | "localhost.localdomain" | "ip6-localhost" | "ip6-loopback"
        )
}

fn default_node_name(profile: NodeProfile, hostname: Option<String>) -> String {
    if let Some(name) = hostname.filter(|value| !is_placeholder_node_name(value)) {
        return name.trim().to_string();
    }
    if profile.is_android() {
        ANDROID_DEFAULT_NODE_NAME.to_string()
    } else {
        DESKTOP_DEFAULT_NODE_NAME.to_string()
    }
}

/// 返回是否改写了名称；调用方负责把结果落盘。
fn normalize_node_name(name: &mut String, profile: NodeProfile, hostname: Option<String>) -> bool {
    if !is_placeholder_node_name(name) {
        return false;
    }
    *name = default_node_name(profile, hostname);
    true
}

fn load_or_create_config(data_dir: &Path, profile: NodeProfile) -> anyhow::Result<PersistedConfig> {
    let path = data_dir.join("node.json");
    match std::fs::read_to_string(&path) {
        Ok(contents) => {
            let value: serde_json::Value = serde_json::from_str(&contents)
                .map_err(|error| anyhow::anyhow!("节点配置文件损坏：{error}"))?;
            let mut config: PersistedConfig = serde_json::from_value(value.clone())
                .map_err(|error| anyhow::anyhow!("节点配置文件损坏：{error}"))?;
            if value.get("monitor_visible").is_none() {
                config.monitor_visible = !matches!(
                    value.get("monitor_display").and_then(|item| item.as_str()),
                    Some("tray" | "disabled")
                );
            }
            config.clipboard_history_count = config
                .clipboard_history_count
                .clamp(1, MAX_CLIPBOARD_HISTORY_COUNT);
            config.clipboard_history_days = config
                .clipboard_history_days
                .clamp(1, MAX_CLIPBOARD_HISTORY_DAYS);
            if normalize_node_name(&mut config.node_name, profile, system_hostname()) {
                if let Err(error) = persist_config(data_dir, &config) {
                    eprintln!("保存节点名称失败：{error}");
                }
            }
            return Ok(config);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let config = PersistedConfig {
        node_id: Uuid::new_v4().to_string(),
        node_name: default_node_name(profile, system_hostname()),
        monitor_visible: true,
        monitor_position: None,
        monitor_locked: false,
        autostart_enabled: default_autostart_enabled(),
        update_frequency: UpdateFrequency::default(),
        update_last_checked_at_ms: None,
        pending_update_version: None,
        clipboard_history_mode: ClipboardHistoryMode::default(),
        clipboard_history_count: default_clipboard_history_count(),
        clipboard_history_days: default_clipboard_history_days(),
        receive_mode: default_receive_mode(),
        receive_directory: None,
        receive_directory_bookmark: None,
    };
    persist_config(data_dir, &config)?;
    Ok(config)
}

#[cfg(target_os = "macos")]
fn activate_receive_directory_access(config: &PersistedConfig) -> Option<PathBuf> {
    let bookmark = config.receive_directory_bookmark.as_deref()?;
    match crate::macos_security_scope::activate_bookmark(bookmark) {
        Ok(path) => Some(path),
        Err(error) => {
            eprintln!("恢复接收目录授权失败：{error}");
            None
        }
    }
}

pub fn validate_receive_directory(path: &Path) -> anyhow::Result<PathBuf> {
    if !path.is_absolute() {
        anyhow::bail!("接收目录必须是绝对路径");
    }
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        anyhow::bail!("接收目录必须是实际目录，不能是符号链接");
    }
    Ok(std::fs::canonicalize(path)?)
}

fn desktop_temp_registry_path(data_dir: &Path) -> PathBuf {
    data_dir.join(DESKTOP_TEMP_REGISTRY_FILE)
}

fn is_desktop_temp_file_path(path: &Path) -> bool {
    if !path.is_absolute() {
        return false;
    }
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    name.contains(".meshshare-") && name.ends_with(".part")
}

fn load_and_cleanup_desktop_temp_files(data_dir: &Path) -> anyhow::Result<HashSet<PathBuf>> {
    let registry_path = desktop_temp_registry_path(data_dir);
    let contents = match std::fs::read(&registry_path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    let registered = if contents.is_empty() {
        Vec::new()
    } else {
        serde_json::from_slice::<Vec<PathBuf>>(&contents)
            .map_err(|error| anyhow::anyhow!("桌面临时文件登记损坏：{error}"))?
    };
    if registered.len() > MAX_DESKTOP_TEMP_FILES {
        return Err(anyhow::anyhow!("桌面临时文件登记超过数量上限"));
    }
    let mut remaining = HashSet::new();
    for path in registered {
        if !is_desktop_temp_file_path(&path) {
            eprintln!("忽略无效的桌面临时文件登记：{}", path.display());
            continue;
        }
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                eprintln!("读取桌面临时文件失败 {}：{error}", path.display());
                remaining.insert(path);
                continue;
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            eprintln!("跳过非普通桌面临时文件：{}", path.display());
            remaining.insert(path);
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) => {
                eprintln!("清理桌面临时文件失败 {}：{error}", path.display());
                remaining.insert(path);
            }
        }
    }
    persist_desktop_temp_files(data_dir, &remaining)?;
    Ok(remaining)
}

fn persist_desktop_temp_files(data_dir: &Path, files: &HashSet<PathBuf>) -> anyhow::Result<()> {
    let path = desktop_temp_registry_path(data_dir);
    let temporary = data_dir.join(format!(
        "{DESKTOP_TEMP_REGISTRY_FILE}.tmp-{}",
        std::process::id()
    ));
    let mut entries = files.iter().cloned().collect::<Vec<_>>();
    entries.sort();
    std::fs::write(&temporary, serde_json::to_vec_pretty(&entries)?)?;
    if let Err(error) = std::fs::rename(&temporary, &path) {
        #[cfg(windows)]
        {
            if path.exists() {
                std::fs::remove_file(&path)?;
                std::fs::rename(&temporary, &path)?;
            } else {
                let _ = std::fs::remove_file(&temporary);
                return Err(error.into());
            }
        }
        #[cfg(not(windows))]
        {
            if let Err(cleanup_error) = std::fs::remove_file(&temporary) {
                if cleanup_error.kind() != std::io::ErrorKind::NotFound {
                    eprintln!(
                        "清理桌面临时文件登记临时文件失败 {}：{cleanup_error}",
                        temporary.display()
                    );
                }
            }
            return Err(error.into());
        }
    }
    Ok(())
}

fn persist_config(data_dir: &Path, config: &PersistedConfig) -> anyhow::Result<()> {
    let path = data_dir.join("node.json");
    let temporary = data_dir.join(format!("node.json.tmp-{}", std::process::id()));
    std::fs::write(&temporary, serde_json::to_vec_pretty(config)?)?;
    if let Err(error) = std::fs::rename(&temporary, &path) {
        #[cfg(windows)]
        {
            if path.exists() {
                std::fs::remove_file(&path)?;
                std::fs::rename(&temporary, &path)?;
            } else {
                let _ = std::fs::remove_file(&temporary);
                return Err(error.into());
            }
        }
        #[cfg(not(windows))]
        {
            if let Err(cleanup_error) = std::fs::remove_file(&temporary) {
                if cleanup_error.kind() != std::io::ErrorKind::NotFound {
                    eprintln!(
                        "清理配置临时文件失败 {}：{cleanup_error}",
                        temporary.display()
                    );
                }
            }
            return Err(error.into());
        }
    }
    Ok(())
}

fn clipboard_payload_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(CLIPBOARD_PAYLOAD_DIR)
}

fn is_clipboard_index_temp_name(name: &str, base: &str) -> bool {
    name.strip_prefix(base)
        .is_some_and(|suffix| suffix.starts_with(".tmp-"))
}

fn remove_clipboard_temp_file(path: &Path) {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return;
    };
    if !(metadata.is_file() || metadata.file_type().is_symlink()) {
        return;
    }
    if let Err(error) = std::fs::remove_file(path) {
        eprintln!("清理剪贴板临时文件失败 {}：{error}", path.display());
    }
}

fn cleanup_clipboard_temp_files(data_dir: &Path) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(data_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if is_clipboard_index_temp_name(name, CLIPBOARD_HISTORY_FILE)
            || is_clipboard_index_temp_name(name, CLIPBOARD_CURRENT_FILE)
        {
            remove_clipboard_temp_file(&entry.path());
        }
    }
    let payload_dir = clipboard_payload_dir(data_dir);
    let entries = match std::fs::read_dir(&payload_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_str().is_some_and(|name| name.contains(".tmp-")) {
            remove_clipboard_temp_file(&entry.path());
        }
    }
    Ok(())
}

fn safe_clipboard_payload_name(name: &str, extension: &str) -> bool {
    let path = Path::new(name);
    path.components().count() == 1
        && path.file_name().and_then(|value| value.to_str()) == Some(name)
        && path.extension().and_then(|value| value.to_str()) == Some(extension)
        && name.len() <= 128
}

fn clipboard_history_record_valid(data_dir: &Path, record: &ClipboardHistoryRecord) -> bool {
    if record.event_id.is_empty()
        || record.event_id.len() > 256
        || record.source_node_id.is_empty()
        || record.source_node_id.len() > 256
        || record.source_name.len() > 256
        || record.content_hash.len() != 64
        || (record.text_path.is_none() && record.image_path.is_none())
    {
        return false;
    }
    let payload_dir = clipboard_payload_dir(data_dir);
    if let Some(path) = record.text_path.as_deref() {
        let full = payload_dir.join(path);
        if !safe_clipboard_payload_name(path, "txt")
            || std::fs::symlink_metadata(&full)
                .map(|metadata| {
                    !metadata.is_file()
                        || metadata.file_type().is_symlink()
                        || metadata.len() as usize != record.text_len
                })
                .unwrap_or(true)
            || record.text_len > CLIPBOARD_TEXT_LIMIT
        {
            return false;
        }
    }
    if let Some(path) = record.image_path.as_deref() {
        let full = payload_dir.join(path);
        let valid_png = std::fs::read(&full)
            .ok()
            .is_some_and(|bytes| crate::clipboard::is_valid_png(&bytes));
        if !safe_clipboard_payload_name(path, "png")
            || std::fs::symlink_metadata(&full)
                .map(|metadata| {
                    !metadata.is_file()
                        || metadata.file_type().is_symlink()
                        || metadata.len() as usize != record.image_len
                })
                .unwrap_or(true)
            || record.image_len > CLIPBOARD_IMAGE_LIMIT
            || !valid_png
        {
            return false;
        }
    }
    true
}

fn load_clipboard_history(data_dir: &Path) -> anyhow::Result<Vec<ClipboardHistoryRecord>> {
    let path = data_dir.join(CLIPBOARD_HISTORY_FILE);
    let contents = match std::fs::read(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let records = match serde_json::from_slice::<Vec<ClipboardHistoryRecord>>(&contents) {
        Ok(records) => records,
        Err(error) => {
            eprintln!("剪贴板历史索引损坏，已忽略：{error}");
            return Ok(Vec::new());
        }
    };
    Ok(records
        .into_iter()
        .take(MAX_CLIPBOARD_HISTORY_COUNT)
        .filter(|record| clipboard_history_record_valid(data_dir, record))
        .collect())
}

fn persist_clipboard_history(
    data_dir: &Path,
    history: &[ClipboardHistoryRecord],
) -> anyhow::Result<()> {
    std::fs::create_dir_all(clipboard_payload_dir(data_dir))?;
    let path = data_dir.join(CLIPBOARD_HISTORY_FILE);
    let temporary = data_dir.join(format!(
        "{CLIPBOARD_HISTORY_FILE}.tmp-{}",
        std::process::id()
    ));
    let contents = serde_json::to_vec_pretty(history)?;
    if let Err(error) =
        std::fs::write(&temporary, contents).and_then(|()| std::fs::rename(&temporary, &path))
    {
        remove_clipboard_temp_file(&temporary);
        return Err(error.into());
    }
    let referenced = history
        .iter()
        .flat_map(|record| [record.text_path.as_deref(), record.image_path.as_deref()])
        .flatten()
        .collect::<HashSet<_>>();
    let entries = match std::fs::read_dir(clipboard_payload_dir(data_dir)) {
        Ok(entries) => entries,
        Err(error) => {
            // The index has already been atomically replaced. A cleanup
            // failure must not be reported as a failed transaction, otherwise
            // callers could roll back memory while disk points at the new
            // index. Stale unreferenced files are safe to retry next startup.
            eprintln!("清理剪贴板历史 payload 失败：{error}");
            return Ok(());
        }
    };
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.contains(".tmp-") {
            remove_clipboard_temp_file(&entry.path());
        } else if !referenced.contains(name) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    Ok(())
}

fn persist_clipboard_current(data_dir: &Path, snapshot: &ClipboardSnapshot) -> anyhow::Result<()> {
    let path = data_dir.join(CLIPBOARD_CURRENT_FILE);
    let temporary = data_dir.join(format!(
        "{CLIPBOARD_CURRENT_FILE}.tmp-{}",
        std::process::id()
    ));
    std::fs::write(&temporary, serde_json::to_vec(snapshot)?)?;
    if let Err(error) = std::fs::rename(&temporary, &path) {
        #[cfg(windows)]
        {
            if path.exists() {
                std::fs::remove_file(&path)?;
                std::fs::rename(&temporary, &path)?;
            } else {
                let _ = std::fs::remove_file(&temporary);
                return Err(error.into());
            }
        }
        #[cfg(not(windows))]
        {
            let _ = std::fs::remove_file(&temporary);
            return Err(error.into());
        }
    }
    Ok(())
}

fn load_clipboard_current(data_dir: &Path) -> Option<ClipboardSnapshot> {
    let contents = std::fs::read(data_dir.join(CLIPBOARD_CURRENT_FILE)).ok()?;
    let snapshot = serde_json::from_slice::<ClipboardSnapshot>(&contents).ok()?;
    clipboard_snapshot_hash(&snapshot).ok().map(|_| snapshot)
}

fn write_clipboard_payload(
    data_dir: &Path,
    storage_id: &str,
    extension: &str,
    bytes: &[u8],
) -> anyhow::Result<String> {
    let directory = clipboard_payload_dir(data_dir);
    std::fs::create_dir_all(&directory)?;
    let name = format!("{storage_id}.{extension}");
    let path = directory.join(&name);
    let temporary = directory.join(format!("{name}.tmp-{}", std::process::id()));
    std::fs::write(&temporary, bytes)?;
    match std::fs::rename(&temporary, &path) {
        Ok(()) => {}
        Err(error) => {
            #[cfg(windows)]
            {
                if path.exists() {
                    std::fs::remove_file(&path)?;
                    std::fs::rename(&temporary, &path)?;
                } else {
                    let _ = std::fs::remove_file(&temporary);
                    return Err(error.into());
                }
            }
            #[cfg(not(windows))]
            {
                let _ = std::fs::remove_file(&temporary);
                return Err(error.into());
            }
        }
    }
    Ok(name)
}

fn write_clipboard_history_record(
    data_dir: &Path,
    snapshot: &ClipboardSnapshot,
    recorded_at_ms: i64,
    content_hash: String,
) -> anyhow::Result<ClipboardHistoryRecord> {
    let storage_id = Uuid::new_v4().to_string();
    let text_path = match snapshot
        .text
        .as_ref()
        .map(|text| write_clipboard_payload(data_dir, &storage_id, "txt", text.as_bytes()))
        .transpose()
    {
        Ok(path) => path,
        Err(error) => {
            remove_clipboard_payload_set(data_dir, &storage_id);
            return Err(error);
        }
    };
    let image_bytes = match snapshot
        .image_png
        .as_deref()
        .map(|value| BASE64.decode(value))
        .transpose()
    {
        Ok(bytes) => bytes,
        Err(error) => {
            remove_clipboard_payload_set(data_dir, &storage_id);
            return Err(error.into());
        }
    };
    let image_path = match image_bytes
        .as_deref()
        .map(|bytes| write_clipboard_payload(data_dir, &storage_id, "png", bytes))
        .transpose()
    {
        Ok(path) => path,
        Err(error) => {
            remove_clipboard_payload_set(data_dir, &storage_id);
            return Err(error);
        }
    };
    Ok(ClipboardHistoryRecord {
        event_id: snapshot.event_id.clone(),
        source_node_id: snapshot.source_node_id.clone(),
        source_name: snapshot.source_name.clone(),
        clock: snapshot.clock.clone(),
        updated_at_ms: snapshot.updated_at_ms,
        recorded_at_ms,
        content_hash,
        text_path,
        image_path,
        text_len: snapshot.text.as_ref().map_or(0, String::len),
        image_len: image_bytes.map_or(0, |bytes| bytes.len()),
    })
}

fn remove_clipboard_payload_set(data_dir: &Path, storage_id: &str) {
    let directory = clipboard_payload_dir(data_dir);
    for extension in ["txt", "png"] {
        let _ = std::fs::remove_file(directory.join(format!("{storage_id}.{extension}")));
    }
    for entry in [
        directory.join(format!("{storage_id}.txt.tmp-{}", std::process::id())),
        directory.join(format!("{storage_id}.png.tmp-{}", std::process::id())),
    ] {
        remove_clipboard_temp_file(&entry);
    }
}

fn load_history_snapshot(
    data_dir: &Path,
    record: &ClipboardHistoryRecord,
) -> anyhow::Result<ClipboardSnapshot> {
    let directory = clipboard_payload_dir(data_dir);
    let text = record
        .text_path
        .as_deref()
        .map(|path| std::fs::read(directory.join(path)))
        .transpose()?
        .map(|bytes| String::from_utf8(bytes).map_err(anyhow::Error::from))
        .transpose()?;
    let image_png = record
        .image_path
        .as_deref()
        .map(|path| {
            std::fs::read(directory.join(path))
                .map(|bytes| BASE64.encode(bytes))
                .map_err(anyhow::Error::from)
        })
        .transpose()?;
    Ok(ClipboardSnapshot {
        event_id: record.event_id.clone(),
        source_node_id: record.source_node_id.clone(),
        source_name: record.source_name.clone(),
        clock: record.clock.clone(),
        text,
        image_png,
        updated_at_ms: record.updated_at_ms,
    })
}

fn remove_clipboard_record_payloads(data_dir: &Path, record: &ClipboardHistoryRecord) {
    let directory = clipboard_payload_dir(data_dir);
    for path in [record.text_path.as_deref(), record.image_path.as_deref()]
        .into_iter()
        .flatten()
    {
        let _ = std::fs::remove_file(directory.join(path));
    }
}

fn clipboard_snapshot_hash(snapshot: &ClipboardSnapshot) -> anyhow::Result<String> {
    let image = snapshot
        .image_png
        .as_deref()
        .map(|value| BASE64.decode(value))
        .transpose()?;
    if snapshot
        .text
        .as_ref()
        .is_some_and(|value| value.len() > CLIPBOARD_TEXT_LIMIT)
        || image
            .as_ref()
            .is_some_and(|value| value.len() > CLIPBOARD_IMAGE_LIMIT)
    {
        return Err(anyhow::anyhow!("剪贴板内容超过大小限制"));
    }
    if image
        .as_deref()
        .is_some_and(|bytes| !crate::clipboard::is_valid_png(bytes))
    {
        return Err(anyhow::anyhow!("剪贴板图片不是有效的 PNG"));
    }
    Ok(crate::clipboard::payload_hash(
        snapshot.text.as_deref(),
        image.as_deref(),
    ))
}

fn prune_clipboard_history_records(
    history: &mut Vec<ClipboardHistoryRecord>,
    config: &PersistedConfig,
    now: i64,
) {
    prune_clipboard_history_records_with_limit(history, config, now, MAX_CLIPBOARD_HISTORY_BYTES);
}

fn prune_clipboard_history_records_with_limit(
    history: &mut Vec<ClipboardHistoryRecord>,
    config: &PersistedConfig,
    now: i64,
    max_bytes: u64,
) {
    match config.clipboard_history_mode {
        ClipboardHistoryMode::Count => history.truncate(
            config
                .clipboard_history_count
                .clamp(1, MAX_CLIPBOARD_HISTORY_COUNT),
        ),
        ClipboardHistoryMode::Days => {
            let age = i64::from(
                config
                    .clipboard_history_days
                    .clamp(1, MAX_CLIPBOARD_HISTORY_DAYS),
            )
            .saturating_mul(CLIPBOARD_DAY_MS);
            let threshold = now.saturating_sub(age);
            history.retain(|record| record.recorded_at_ms >= threshold);
            history.truncate(MAX_CLIPBOARD_HISTORY_COUNT);
        }
    }
    let mut total_bytes = history
        .iter()
        .map(|record| (record.text_len as u64).saturating_add(record.image_len as u64))
        .sum::<u64>();
    while total_bytes > max_bytes && history.len() > 1 {
        let Some(record) = history.pop() else { break };
        total_bytes = total_bytes
            .saturating_sub((record.text_len as u64).saturating_add(record.image_len as u64));
    }
}

fn is_tailscale_ipv4(address: Ipv4Addr) -> bool {
    (u32::from(address) & 0xffc00000) == 0x64400000
}

pub(crate) fn is_network_ipv4(address: Ipv4Addr) -> bool {
    address.is_private() || address.is_link_local() || is_tailscale_ipv4(address)
}

fn normalize_peer_http_base(raw: &str) -> Option<String> {
    let mut url = reqwest::Url::parse(raw).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let host = url.host_str()?;
    let allowed_host = host.eq_ignore_ascii_case("localhost")
        || host.ends_with(".local")
        || host
            .parse::<Ipv4Addr>()
            .is_ok_and(|address| address.is_loopback() || is_network_ipv4(address));
    if !allowed_host {
        return None;
    }
    url.set_path("");
    Some(url.to_string().trim_end_matches('/').to_string())
}

fn http_base_from_peer_endpoint(raw: &str) -> Option<String> {
    let mut url = reqwest::Url::parse(raw).ok()?;
    let scheme = match url.scheme() {
        "ws" => "http",
        "wss" => "https",
        "http" | "https" => url.scheme(),
        _ => return None,
    }
    .to_string();
    url.set_scheme(&scheme).ok()?;
    if !url.path().ends_with("/api/v1/ws") {
        return None;
    }
    url.set_path("");
    url.set_query(None);
    url.set_fragment(None);
    normalize_peer_http_base(url.as_str())
}

fn mobile_ca_url(raw: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(raw).map_err(|_| "CA 地址无效".to_string())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("CA 地址只支持 HTTP 或 HTTPS".to_string());
    }
    let host = url.host_str().unwrap_or_default();
    let Some(address) = host.parse::<Ipv4Addr>().ok() else {
        return Err("CA 地址必须是节点的私网 IPv4 地址".to_string());
    };
    if !(address.is_loopback() || is_network_ipv4(address)) {
        return Err("CA 地址必须是节点的私网 IPv4 地址".to_string());
    }
    Ok(url)
}

fn is_cellular_interface(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    // CLAT 把蜂窝接口包装成 v4-/v6- 前缀的同名接口，判定前先剥掉包装。
    // 宁可放过未知的运营商网卡名，也不用白名单漏掉手机 Wi-Fi 导致节点不可达。
    let name = name
        .strip_prefix("v4-")
        .or_else(|| name.strip_prefix("v6-"))
        .unwrap_or(&name);
    ["rmnet", "ccmni", "radio", "clat", "ppp", "pdp", "wwan"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

/// 物理以太网和无线地址更可能是对端可达的入口；虚拟网卡与蜂窝地址排在后面，
/// 让公布顺序与手机、桌面互相直连时实际能用的地址一致。
fn interface_rank(name: &str) -> u8 {
    let name = name.to_ascii_lowercase();
    if is_cellular_interface(&name) {
        return 2;
    }
    if [
        "vethernet",
        "veth",
        "vmnet",
        "docker",
        "bridge",
        "br-",
        "utun",
        "tun",
        "tap",
        "wg",
        "wireguard",
        "vpn",
        "ipsec",
        "tailscale",
        "zt",
        "dummy",
        "ifb",
    ]
    .iter()
    .any(|prefix| name.starts_with(prefix))
    {
        return 2;
    }
    if ["eth", "en", "wl", "wlan", "wi-fi", "ethernet"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
    {
        return 0;
    }
    1
}

fn interface_names() -> HashMap<Ipv4Addr, String> {
    let mut names = HashMap::new();
    match if_addrs::get_if_addrs() {
        Ok(interfaces) => {
            for interface in interfaces {
                if let IpAddr::V4(address) = interface.ip() {
                    names.entry(address).or_insert(interface.name);
                }
            }
        }
        Err(error) => eprintln!("读取本机网卡名称失败：{error}"),
    }
    names
}

fn network_addresses_for_bound(port: u16, bound: &BTreeSet<Ipv4Addr>) -> Vec<AddressInfo> {
    network_addresses(port, bound, &interface_names())
}

fn network_addresses(
    port: u16,
    bound: &BTreeSet<Ipv4Addr>,
    names: &HashMap<Ipv4Addr, String>,
) -> Vec<AddressInfo> {
    let mut addresses = bound
        .iter()
        .copied()
        .filter(|address| is_network_ipv4(*address))
        .map(|address| {
            let kind = if is_tailscale_ipv4(address) {
                "tailscale"
            } else if address.is_private() {
                "lan"
            } else {
                "network"
            };
            AddressInfo {
                address: address.to_string(),
                url: format!("http://{address}:{port}"),
                label: address.to_string(),
                kind: kind.to_string(),
            }
        })
        .collect::<Vec<_>>();
    addresses.sort_by_key(|address| {
        let kind_rank = match address.kind.as_str() {
            "lan" => 0,
            "tailscale" => 1,
            _ => 2,
        };
        let parsed = address.address.parse::<Ipv4Addr>().ok();
        let interface = parsed
            .and_then(|ip| names.get(&ip).map(|name| interface_rank(name)))
            .unwrap_or(1);
        (
            kind_rank,
            interface,
            parsed.map(u32::from).unwrap_or(u32::MAX),
        )
    });
    addresses
}

pub fn bind_addresses() -> BTreeSet<Ipv4Addr> {
    let mut addresses = BTreeSet::from([Ipv4Addr::LOCALHOST]);
    match if_addrs::get_if_addrs() {
        Ok(interfaces) => {
            addresses.extend(bindable_addresses(
                NodeProfile::current(),
                interfaces.into_iter().map(|interface| {
                    let ip = interface.ip();
                    (interface.name, ip)
                }),
            ));
        }
        Err(error) => eprintln!("读取本机网卡失败：{error}"),
    }
    addresses
}

fn bindable_addresses(
    profile: NodeProfile,
    interfaces: impl IntoIterator<Item = (String, IpAddr)>,
) -> BTreeSet<Ipv4Addr> {
    interfaces
        .into_iter()
        // 蜂窝地址在局域网内没有可达意义，Android 上也不应让节点绑定到运营商地址。
        .filter(|(name, _)| !(profile.is_android() && is_cellular_interface(name)))
        .filter_map(|(_, ip)| match ip {
            IpAddr::V4(address) if is_network_ipv4(address) => Some(address),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        bind_addresses, bindable_addresses, default_node_name, interface_rank,
        is_cellular_interface, is_placeholder_node_name, load_or_create_config, mobile_ca_url,
        network_addresses, network_addresses_for_bound, normalize_node_name,
        prune_clipboard_history_records_with_limit, NodeProfile, NodeRuntime, UpdateFrequency,
        CLIPBOARD_HISTORY_FILE, DESKTOP_TEMP_REGISTRY_FILE, MAX_MOBILE_TRANSFER_RECORDS,
        TRANSFER_ACTIVITY_TIMEOUT_MS,
    };
    use crate::model::{
        AddressInfo, ClipboardEvent, ClipboardHistoryMode, GpuMemoryKind, GpuMetrics,
        HardwareMetrics, Hlc, PeerInfo, ReceiveMode, TransferItem, TransferOffer, TransferStatus,
        WireMessage, CLIPBOARD_VIEW_CAPABILITY, MAX_MOBILE_SESSIONS, MAX_TRANSFER_RECORDS,
        METRICS_GPU_VIEW_CAPABILITY, METRICS_NETWORK_CAPABILITY, METRICS_SYSTEM_CAPABILITY,
        METRICS_SYSTEM_VIEW_CAPABILITY,
    };
    use std::collections::HashMap;
    use std::net::{IpAddr, Ipv4Addr};
    use std::path::PathBuf;
    use tokio::sync::mpsc;
    use uuid::Uuid;

    fn test_peer(node_id: &str, capabilities: Vec<String>) -> PeerInfo {
        PeerInfo {
            node_id: node_id.to_string(),
            node_name: "peer".to_string(),
            platform: "test".to_string(),
            addresses: Vec::new(),
            mobile_addresses: Vec::new(),
            capabilities,
            online: true,
            last_seen_ms: crate::clock::now_millis(),
            endpoint: None,
            source: "test".to_string(),
            metrics: None,
            metrics_received_at_ms: None,
            metrics_stale: false,
        }
    }

    fn temp_data_dir() -> PathBuf {
        let path = std::env::temp_dir().join(format!("meshshare-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("create test directory");
        path
    }

    #[test]
    fn peer_http_base_prefers_verified_websocket_endpoint() {
        let data_dir = temp_data_dir();
        let runtime = NodeRuntime::new_with_profile(
            data_dir.clone(),
            37880,
            "test".to_string(),
            NodeProfile::Desktop,
        )
        .expect("create runtime");
        let mut peer = test_peer("peer", Vec::new());
        peer.endpoint = Some("ws://100.64.0.2:37880/api/v1/ws".to_string());
        peer.addresses = vec![
            AddressInfo {
                address: "192.168.1.20".to_string(),
                url: "http://192.168.1.20:37880".to_string(),
                label: "Wi-Fi".to_string(),
                kind: "lan".to_string(),
            },
            AddressInfo {
                address: "100.64.0.2".to_string(),
                url: "http://100.64.0.2:37880".to_string(),
                label: "Tailscale".to_string(),
                kind: "tailscale".to_string(),
            },
        ];
        runtime.set_peer(peer);
        assert_eq!(
            runtime.peer_http_base("peer").as_deref(),
            Some("http://100.64.0.2:37880")
        );
        assert_eq!(
            runtime.peer_http_bases("peer"),
            vec![
                "http://100.64.0.2:37880".to_string(),
                "http://192.168.1.20:37880".to_string(),
            ]
        );
        let mut hello = test_peer("peer", Vec::new());
        hello.source = "websocket".to_string();
        hello.endpoint = Some("http://192.168.1.20:37880/api/v1/ws".to_string());
        hello.addresses = runtime
            .peers_snapshot()
            .into_iter()
            .next()
            .expect("peer metadata")
            .addresses;
        runtime.set_peer(hello);
        assert_eq!(
            runtime.peer_http_base("peer").as_deref(),
            Some("http://100.64.0.2:37880")
        );
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[test]
    fn bound_addresses_publish_only_network_ipv4_in_access_order() {
        let bound = [
            Ipv4Addr::LOCALHOST,
            Ipv4Addr::new(127, 0, 0, 2),
            Ipv4Addr::new(192, 168, 1, 10),
            Ipv4Addr::new(100, 64, 0, 2),
            Ipv4Addr::new(169, 254, 2, 3),
            Ipv4Addr::new(8, 8, 8, 8),
        ]
        .into_iter()
        .collect();
        let addresses = network_addresses_for_bound(37880, &bound);
        assert_eq!(
            addresses
                .iter()
                .map(|address| address.address.as_str())
                .collect::<Vec<_>>(),
            ["192.168.1.10", "100.64.0.2", "169.254.2.3"]
        );
    }

    #[test]
    fn cellular_interfaces_are_recognized_by_name() {
        for name in [
            "rmnet_data0",
            "ccmni0",
            "radio0",
            "clat4",
            "ppp0",
            "wwan0",
            "pdp0",
            "v4-rmnet_data0",
            "v6-rmnet_data0",
        ] {
            assert!(is_cellular_interface(name), "{name} should be cellular");
        }
        for name in ["wlan0", "eth0", "en0", "vEthernet (WSL)"] {
            assert!(
                !is_cellular_interface(name),
                "{name} should not be cellular"
            );
        }
    }

    #[test]
    fn android_skips_cellular_interfaces_when_binding() {
        let interfaces = vec![
            (
                "wlan0".to_string(),
                "192.168.1.20".parse::<IpAddr>().expect("wlan address"),
            ),
            (
                "rmnet_data0".to_string(),
                "10.136.144.220"
                    .parse::<IpAddr>()
                    .expect("cellular address"),
            ),
            (
                "vEthernet (WSL)".to_string(),
                "172.20.0.1".parse::<IpAddr>().expect("virtual address"),
            ),
        ];
        let android = bindable_addresses(NodeProfile::Android, interfaces.clone());
        assert!(android.contains(&Ipv4Addr::new(192, 168, 1, 20)));
        assert!(!android.contains(&Ipv4Addr::new(10, 136, 144, 220)));
        let desktop = bindable_addresses(NodeProfile::Desktop, interfaces);
        assert!(desktop.contains(&Ipv4Addr::new(192, 168, 1, 20)));
        assert!(desktop.contains(&Ipv4Addr::new(10, 136, 144, 220)));
        assert!(desktop.contains(&Ipv4Addr::new(172, 20, 0, 1)));
    }

    #[test]
    fn physical_address_outranks_cellular_address() {
        let physical = Ipv4Addr::new(192, 168, 1, 20);
        let cellular = Ipv4Addr::new(10, 136, 144, 220);
        let bound = [physical, cellular].into_iter().collect();
        let names = HashMap::from([
            (physical, "wlan0".to_string()),
            (cellular, "rmnet_data0".to_string()),
        ]);
        let addresses = network_addresses(37880, &bound, &names);
        assert_eq!(
            addresses
                .iter()
                .map(|address| address.address.as_str())
                .collect::<Vec<_>>(),
            ["192.168.1.20", "10.136.144.220"]
        );
    }

    #[test]
    fn mobile_ca_url_only_accepts_private_node_addresses() {
        for allowed in [
            "http://192.168.1.10:37880/api/v1/mobile/ca",
            "http://100.64.0.2:37880/api/v1/mobile/ca",
            "http://169.254.2.3:37880/api/v1/mobile/ca",
            "https://127.0.0.1:37881/api/v1/mobile/ca",
        ] {
            assert!(
                mobile_ca_url(allowed).is_ok(),
                "{allowed} should be allowed"
            );
        }
        for rejected in [
            "http://8.8.8.8:37880/api/v1/mobile/ca",
            "http://example.com/api/v1/mobile/ca",
            "file:///etc/passwd",
            "not a url",
        ] {
            assert!(
                mobile_ca_url(rejected).is_err(),
                "{rejected} should be rejected"
            );
        }
    }

    #[test]
    fn physical_networks_rank_before_virtual_and_cellular_interfaces() {
        for name in [
            "wlan0",
            "eth0",
            "Ethernet",
            "Ethernet 2",
            "Wi-Fi",
            "en0",
            "wlp2s0",
        ] {
            assert_eq!(interface_rank(name), 0, "{name}");
        }
        assert_eq!(interface_rank("rmnet_data0"), 2);
        assert_eq!(interface_rank("vEthernet (WSL)"), 2);
        assert_eq!(interface_rank("docker0"), 2);
        assert_eq!(interface_rank("utun4"), 2);
        assert_eq!(interface_rank("vpn0"), 2);
        assert_eq!(interface_rank("ipsec0"), 2);
        assert_eq!(interface_rank("wireguard0"), 2);
        assert_eq!(interface_rank("unknown0"), 1);
    }

    #[test]
    fn actual_bind_addresses_are_explicit_allowed_ipv4() {
        let addresses = bind_addresses();
        assert!(!addresses.is_empty());
        assert!(addresses.iter().all(|address| {
            !address.is_unspecified()
                && (address.is_loopback()
                    || address.is_private()
                    || address.is_link_local()
                    || (u32::from(*address) & 0xffc0_0000) == 0x6440_0000)
        }));
    }

    #[test]
    fn actual_bind_addresses_accept_ephemeral_listeners_without_wildcards() {
        for address in bind_addresses() {
            let listener = std::net::TcpListener::bind((address, 0))
                .expect("allowed interface address must be bindable");
            assert_eq!(
                listener.local_addr().expect("listener address").ip(),
                address
            );
        }
    }

    fn mobile_offer(runtime: &NodeRuntime, transfer_id: &str, total_bytes: u64) -> TransferOffer {
        TransferOffer {
            transfer_id: transfer_id.to_string(),
            source_node_id: runtime.node_id(),
            source_name: "desktop".to_string(),
            target_node_id: runtime.node_id(),
            items: vec![TransferItem {
                item_id: format!("{transfer_id}-item"),
                name: "file.bin".to_string(),
                relative_path: "file.bin".to_string(),
                size: total_bytes,
                sha256: "hash".to_string(),
                media_type: "application/octet-stream".to_string(),
                directory: false,
            }],
            total_bytes,
            created_at_ms: crate::clock::now_millis(),
        }
    }

    #[test]
    fn placeholder_hostnames_are_not_node_names() {
        for name in ["", "  ", "localhost", "LOCALHOST", "localhost.localdomain"] {
            assert!(
                is_placeholder_node_name(name),
                "{name} should be a placeholder"
            );
        }
        for name in ["demo-desktop", "demo-phone", "demo-workstation.local"] {
            assert!(
                !is_placeholder_node_name(name),
                "{name} should be a usable name"
            );
        }
    }

    #[test]
    fn android_nodes_default_to_mobile_name() {
        assert_eq!(
            default_node_name(NodeProfile::Android, Some("localhost".to_string())),
            "移动设备"
        );
        assert_eq!(default_node_name(NodeProfile::Android, None), "移动设备");
        assert_eq!(
            default_node_name(NodeProfile::Android, Some("Redmi-K70".to_string())),
            "Redmi-K70"
        );
    }

    #[test]
    fn desktop_nodes_keep_hostname_or_fallback() {
        assert_eq!(
            default_node_name(NodeProfile::Desktop, Some("YJM-MI-PC".to_string())),
            "YJM-MI-PC"
        );
        assert_eq!(
            default_node_name(NodeProfile::Desktop, Some(" localhost ".to_string())),
            "MeshShare 节点"
        );
        assert_eq!(
            default_node_name(NodeProfile::Desktop, None),
            "MeshShare 节点"
        );
    }

    #[test]
    fn placeholder_name_migration_uses_platform_default() {
        let mut name = "localhost".to_string();
        assert!(normalize_node_name(
            &mut name,
            NodeProfile::Android,
            Some("localhost".to_string())
        ));
        assert_eq!(name, "移动设备");

        let mut kept = "Redmi-K70".to_string();
        assert!(!normalize_node_name(
            &mut kept,
            NodeProfile::Android,
            Some("localhost".to_string())
        ));
        assert_eq!(kept, "Redmi-K70");
    }

    #[test]
    fn placeholder_name_is_replaced_and_persisted() {
        let path = temp_data_dir();
        std::fs::write(
            path.join("node.json"),
            r#"{"node_id":"node-a","node_name":"localhost"}"#,
        )
        .expect("write placeholder config");
        let config = load_or_create_config(&path, NodeProfile::Android).expect("load config");
        assert!(!is_placeholder_node_name(&config.node_name));
        let persisted: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(path.join("node.json")).expect("read config file"),
        )
        .expect("parse config file");
        assert_eq!(persisted["node_name"], config.node_name.as_str());
        assert_eq!(persisted["node_id"], "node-a");

        std::fs::write(
            path.join("node.json"),
            r#"{"node_id":"node-b","node_name":"Redmi-K70"}"#,
        )
        .expect("write named config");
        let named = load_or_create_config(&path, NodeProfile::Android).expect("reload config");
        assert_eq!(named.node_name, "Redmi-K70");
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn old_config_shows_monitor_by_default() {
        let path = temp_data_dir();
        std::fs::write(
            path.join("node.json"),
            r#"{"node_id":"node-a","node_name":"test"}"#,
        )
        .expect("write old config");
        let config = load_or_create_config(&path, NodeProfile::Desktop).expect("load config");
        assert!(config.monitor_visible);
        assert_eq!(config.monitor_position, None);
        assert!(!config.monitor_locked);
        assert!(config.autostart_enabled);
        assert_eq!(config.update_frequency, UpdateFrequency::Weekly);
        assert_eq!(config.update_last_checked_at_ms, None);
        assert_eq!(config.pending_update_version, None);
        assert_eq!(
            config.clipboard_history_mode,
            crate::model::ClipboardHistoryMode::Count
        );
        assert_eq!(
            config.clipboard_history_count,
            crate::model::DEFAULT_CLIPBOARD_HISTORY_COUNT
        );
        assert_eq!(
            config.clipboard_history_days,
            crate::model::DEFAULT_CLIPBOARD_HISTORY_DAYS
        );
        assert_eq!(config.receive_mode, ReceiveMode::Confirm);
        assert_eq!(config.receive_directory, None);
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn legacy_monitor_display_maps_to_visibility() {
        let path = temp_data_dir();
        for (legacy, expected) in [
            (
                r#"{"node_id":"node-a","node_name":"test","monitor_display":"tray"}"#,
                false,
            ),
            (
                r#"{"node_id":"node-a","node_name":"test","monitor_display":"disabled"}"#,
                false,
            ),
            (
                r#"{"node_id":"node-a","node_name":"test","monitor_display":"floating"}"#,
                true,
            ),
            (
                r#"{"node_id":"node-a","node_name":"test","monitor_visible":true,"monitor_display":"tray"}"#,
                true,
            ),
        ] {
            std::fs::write(path.join("node.json"), legacy).expect("write legacy config");
            let config =
                load_or_create_config(&path, NodeProfile::Desktop).expect("load legacy config");
            assert_eq!(config.monitor_visible, expected, "legacy: {legacy}");
        }
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn auto_receive_settings_are_persisted_and_used_for_new_transfers() {
        let path = temp_data_dir();
        let destination = path.join("received");
        std::fs::create_dir_all(&destination).expect("create receive directory");
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        runtime
            .set_receive_settings(ReceiveMode::Auto, Some(destination.clone()))
            .expect("save receive settings");
        assert_eq!(runtime.receive_settings().mode, ReceiveMode::Auto);
        assert_eq!(
            runtime.receive_settings().default_path,
            Some(
                std::fs::canonicalize(&destination)
                    .expect("canonical destination")
                    .to_string_lossy()
                    .into_owned()
            )
        );
        let offer = mobile_offer(&runtime, "auto-receive", 1);
        assert!(runtime.add_transfer(offer));
        assert_eq!(
            runtime
                .transfer("auto-receive")
                .expect("auto transfer")
                .progress
                .status,
            TransferStatus::Accepted
        );
        std::fs::remove_dir_all(&destination).expect("remove receive directory");
        let offer = mobile_offer(&runtime, "fallback-confirm", 1);
        assert!(runtime.add_transfer(offer));
        assert_eq!(
            runtime
                .transfer("fallback-confirm")
                .expect("fallback transfer")
                .progress
                .status,
            TransferStatus::Waiting
        );
        let _ = std::fs::remove_dir_all(path);
    }

    #[tokio::test]
    async fn auto_receive_publishes_progress_before_offer() {
        let path = temp_data_dir();
        let destination = path.join("received");
        std::fs::create_dir_all(&destination).expect("create receive directory");
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        runtime
            .set_receive_settings(ReceiveMode::Auto, Some(destination))
            .expect("save receive settings");
        let mut events = runtime.subscribe();

        assert!(runtime.add_transfer(mobile_offer(&runtime, "auto-order", 1)));
        let first = events.recv().await.expect("receive progress event");
        let second = events.recv().await.expect("receive offer event");
        assert!(matches!(
            first,
            WireMessage::TransferProgress { progress }
                if progress.status == TransferStatus::Accepted
        ));
        assert!(matches!(second, WireMessage::TransferOffer { .. }));
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn corrupted_config_is_reported_without_replacing_node_identity() {
        let path = temp_data_dir();
        let config_path = path.join("node.json");
        std::fs::write(&config_path, "{\"node_id\":").expect("write corrupt config");

        assert!(load_or_create_config(&path, NodeProfile::Desktop).is_err());
        assert_eq!(
            std::fs::read_to_string(config_path).expect("read corrupt config"),
            "{\"node_id\":"
        );
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn autostart_preference_is_persisted() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        assert!(runtime.autostart_enabled());
        runtime
            .set_autostart_enabled(false)
            .expect("disable autostart");

        let restored = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("restore runtime");
        assert!(!restored.autostart_enabled());
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn update_preference_and_pending_state_are_persisted() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        runtime
            .set_update_frequency(UpdateFrequency::Daily)
            .expect("save update frequency");
        runtime
            .set_update_check_state(Some(123), Some("1.2.3".to_string()))
            .expect("save update state");

        let restored = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("restore runtime");
        assert_eq!(
            restored.update_settings(),
            (UpdateFrequency::Daily, Some(123), Some("1.2.3".to_string()))
        );
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn update_frequency_parser_rejects_unknown_values() {
        assert_eq!(
            UpdateFrequency::parse("daily"),
            Some(UpdateFrequency::Daily)
        );
        assert_eq!(
            UpdateFrequency::parse("weekly"),
            Some(UpdateFrequency::Weekly)
        );
        assert_eq!(
            UpdateFrequency::parse("disabled"),
            Some(UpdateFrequency::Disabled)
        );
        assert_eq!(UpdateFrequency::parse("monthly"), None);
    }

    #[test]
    fn active_transfers_block_update_installation() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        assert!(!runtime.has_active_transfers());
        let offer = mobile_offer(&runtime, "active", 1);
        runtime.add_outgoing_transfer(offer);
        assert!(runtime.has_active_transfers());
        runtime
            .fail_transfer(
                "active",
                "test complete".to_string(),
                TransferStatus::Completed,
            )
            .expect("finish transfer");
        assert!(!runtime.has_active_transfers());

        let session = runtime
            .create_mobile_session("phone".to_string())
            .expect("create mobile session");
        runtime
            .create_mobile_transfer(
                &session.session_id,
                mobile_offer(&runtime, "mobile-active", 1),
            )
            .expect("create mobile transfer");
        assert!(runtime.has_active_transfers());
        runtime
            .fail_mobile_transfer("mobile-active", "test complete".to_string())
            .expect("finish mobile transfer");
        runtime.add_transfer(mobile_offer(&runtime, "retry-during-update", 1));
        let failed = runtime
            .fail_transfer(
                "retry-during-update",
                "test failure".to_string(),
                TransferStatus::Failed,
            )
            .expect("mark transfer failed");
        assert!(!runtime.has_active_transfers());
        assert!(runtime.begin_update_install());
        let blocked = runtime.add_outgoing_transfer(mobile_offer(&runtime, "blocked", 1));
        assert_eq!(blocked.status, TransferStatus::Failed);
        assert!(runtime
            .create_mobile_transfer(
                &session.session_id,
                mobile_offer(&runtime, "blocked-mobile", 1)
            )
            .is_err());
        assert!(matches!(
            runtime.retry_transfer("retry-during-update", TransferStatus::Accepted, None),
            Err(super::TransferRetryError::InstallingUpdate)
        ));
        runtime.update_transfer(
            "retry-during-update",
            super::TransferProgress {
                status: TransferStatus::Accepted,
                retry_count: 1,
                ..failed
            },
        );
        assert!(!runtime.has_active_transfers());
        runtime.end_update_install();
        assert!(runtime
            .retry_transfer("retry-during-update", TransferStatus::Accepted, None)
            .is_ok());
        assert!(runtime.has_active_transfers());
        let _ = std::fs::remove_dir_all(path);
    }

    #[tokio::test]
    async fn ca_download_rejects_oversized_chunked_body_before_eof() {
        use futures_util::StreamExt;

        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let app = axum::Router::new().route(
            "/ca.crt",
            axum::routing::get(|| async {
                let first = futures_util::stream::once(async {
                    Ok::<_, std::io::Error>(axum::body::Bytes::from(vec![
                        b'x';
                        super::MAX_MOBILE_CA_BYTES
                            + 1
                    ]))
                });
                axum::body::Body::from_stream(first.chain(futures_util::stream::pending()))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let address = listener.local_addr().expect("listener address");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve CA fixture");
        });
        let url = format!("http://{address}/ca.crt");
        let error = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            runtime.fetch_mobile_ca(&url),
        )
        .await
        .expect("reject before the sender reaches EOF")
        .expect_err("reject oversized CA response");
        assert!(error.contains("超出大小限制"));
        server.abort();
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn monitor_position_lock_and_visibility_are_persisted() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        runtime
            .set_monitor_position(crate::model::MonitorPosition { x: 123, y: 456 })
            .expect("save monitor position");
        runtime.set_monitor_locked(true).expect("save monitor lock");
        runtime
            .set_monitor_visible(false)
            .expect("save monitor visibility");

        let restored = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("restore runtime");
        assert_eq!(
            restored.monitor_position(),
            Some(crate::model::MonitorPosition { x: 123, y: 456 })
        );
        assert!(restored.monitor_locked());
        assert!(!restored.monitor_visible());
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn failed_monitor_settings_write_does_not_mutate_runtime_state() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        std::fs::remove_dir_all(&path).expect("remove config directory");

        assert!(runtime.set_monitor_visible(false).is_err());
        assert!(runtime.monitor_visible());
        assert!(runtime.set_monitor_locked(true).is_err());
        assert!(!runtime.monitor_locked());
        assert!(runtime
            .set_monitor_position(crate::model::MonitorPosition { x: 10, y: 20 })
            .is_err());
        assert_eq!(runtime.monitor_position(), None);
        assert!(runtime.set_autostart_enabled(false).is_err());
        assert!(runtime.autostart_enabled());
        assert!(runtime
            .set_update_frequency(UpdateFrequency::Daily)
            .is_err());
        assert_eq!(runtime.update_settings().0, UpdateFrequency::Weekly);
        assert!(runtime
            .set_update_check_state(Some(42), Some("1.2.3".to_string()))
            .is_err());
        assert_eq!(runtime.update_settings().1, None);
        assert_eq!(runtime.update_settings().2, None);
    }

    #[test]
    fn metrics_are_only_sent_to_capable_peers() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let (tx, mut rx) = mpsc::channel(8);
        runtime.set_peer(test_peer(
            "capable",
            vec![METRICS_SYSTEM_CAPABILITY.to_string()],
        ));
        runtime.add_peer_channel("capable".to_string(), tx);
        runtime.set_peer(test_peer("legacy", Vec::new()));
        let (legacy_tx, mut legacy_rx) = mpsc::channel(8);
        runtime.add_peer_channel("legacy".to_string(), legacy_tx);
        runtime.publish_metrics(HardwareMetrics {
            node_id: runtime.node_id(),
            sampled_at_ms: 1,
            cpu_usage_percent: Some(1.0),
            memory_used_bytes: None,
            memory_total_bytes: None,
            network_down_bytes_per_sec: None,
            network_up_bytes_per_sec: None,
            gpus: vec![GpuMetrics {
                name: "GPU".to_string(),
                utilization_percent: Some(50.0),
                memory_used_bytes: Some(1),
                memory_total_bytes: Some(2),
                memory_kind: GpuMemoryKind::Dedicated,
                unavailable_reason: None,
            }],
        });
        let message = rx.try_recv().expect("capable peer receives metrics");
        match message {
            crate::model::WireMessage::MetricsUpdate { metrics } => {
                assert!(metrics.gpus.is_empty());
            }
            _ => panic!("unexpected message"),
        }
        assert!(legacy_rx.try_recv().is_err());
        runtime
            .stopping
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn network_metric_peers_receive_only_network_fields() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let (tx, mut rx) = mpsc::channel(8);
        runtime.set_peer(test_peer(
            "network-only",
            vec![METRICS_NETWORK_CAPABILITY.to_string()],
        ));
        runtime.add_peer_channel("network-only".to_string(), tx);
        runtime.publish_metrics(HardwareMetrics {
            node_id: runtime.node_id(),
            sampled_at_ms: 1,
            cpu_usage_percent: Some(12.0),
            memory_used_bytes: Some(10),
            memory_total_bytes: Some(20),
            network_down_bytes_per_sec: Some(30),
            network_up_bytes_per_sec: Some(40),
            gpus: vec![GpuMetrics {
                name: "GPU".to_string(),
                utilization_percent: Some(50.0),
                memory_used_bytes: Some(1),
                memory_total_bytes: Some(2),
                memory_kind: GpuMemoryKind::Dedicated,
                unavailable_reason: None,
            }],
        });
        let message = rx.try_recv().expect("network peer receives metrics");
        match message {
            WireMessage::MetricsUpdate { metrics } => {
                assert_eq!(metrics.cpu_usage_percent, None);
                assert_eq!(metrics.memory_used_bytes, None);
                assert_eq!(metrics.network_down_bytes_per_sec, Some(30));
                assert_eq!(metrics.network_up_bytes_per_sec, Some(40));
                assert!(metrics.gpus.is_empty());
            }
            _ => panic!("unexpected message"),
        }
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn android_view_peers_receive_desktop_system_and_gpu_metrics() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let (tx, mut rx) = mpsc::channel(8);
        runtime.set_peer(test_peer(
            "android",
            vec![
                METRICS_NETWORK_CAPABILITY.to_string(),
                METRICS_SYSTEM_VIEW_CAPABILITY.to_string(),
                METRICS_GPU_VIEW_CAPABILITY.to_string(),
            ],
        ));
        runtime.add_peer_channel("android".to_string(), tx);
        runtime.publish_metrics(HardwareMetrics {
            node_id: runtime.node_id(),
            sampled_at_ms: 1,
            cpu_usage_percent: Some(12.0),
            memory_used_bytes: Some(10),
            memory_total_bytes: Some(20),
            network_down_bytes_per_sec: Some(30),
            network_up_bytes_per_sec: Some(40),
            gpus: vec![GpuMetrics {
                name: "GPU".to_string(),
                utilization_percent: Some(50.0),
                memory_used_bytes: Some(1),
                memory_total_bytes: Some(2),
                memory_kind: GpuMemoryKind::Dedicated,
                unavailable_reason: None,
            }],
        });
        match rx.try_recv().expect("Android view peer receives metrics") {
            WireMessage::MetricsUpdate { metrics } => {
                assert_eq!(metrics.cpu_usage_percent, Some(12.0));
                assert_eq!(metrics.memory_used_bytes, Some(10));
                assert_eq!(metrics.memory_total_bytes, Some(20));
                assert_eq!(metrics.network_down_bytes_per_sec, Some(30));
                assert_eq!(metrics.network_up_bytes_per_sec, Some(40));
                assert_eq!(metrics.gpus.len(), 1);
            }
            _ => panic!("unexpected message"),
        }
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn android_profile_advertises_transfer_network_and_view_metrics() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new_with_profile(
            path.clone(),
            0,
            env!("CARGO_PKG_VERSION").into(),
            super::NodeProfile::Android,
        )
        .expect("create Android runtime");
        let node = runtime.node_info();
        assert!(runtime.is_android_profile());
        assert!(node.capabilities.iter().any(|item| item == "transfer.file"));
        assert!(node
            .capabilities
            .iter()
            .any(|item| item == METRICS_NETWORK_CAPABILITY));
        assert!(!node
            .capabilities
            .iter()
            .any(|item| item == METRICS_SYSTEM_CAPABILITY));
        assert!(node
            .capabilities
            .iter()
            .any(|item| item == METRICS_SYSTEM_VIEW_CAPABILITY));
        assert!(node
            .capabilities
            .iter()
            .any(|item| item == METRICS_GPU_VIEW_CAPABILITY));
        assert!(!node
            .capabilities
            .iter()
            .any(|item| item == "clipboard.text"));
        assert!(node
            .capabilities
            .iter()
            .any(|item| item == CLIPBOARD_VIEW_CAPABILITY));
        assert!(!node.capabilities.iter().any(|item| item == "web.mobile"));
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn duplicate_transfer_offer_does_not_reset_progress() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let offer = mobile_offer(&runtime, "duplicate", 1);
        runtime.add_transfer(offer.clone());
        runtime
            .mark_transfer_accepted("duplicate")
            .expect("accept transfer");
        runtime.add_transfer(offer);
        assert_eq!(
            runtime
                .transfer("duplicate")
                .expect("find transfer")
                .progress
                .status,
            TransferStatus::Accepted
        );
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn active_transfer_records_have_a_bounded_count() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        for index in 0..MAX_TRANSFER_RECORDS {
            runtime.add_transfer(mobile_offer(&runtime, &format!("transfer-{index}"), 1));
        }
        runtime.add_transfer(mobile_offer(&runtime, "overflow", 1));
        assert_eq!(runtime.transfers.read().len(), MAX_TRANSFER_RECORDS);
        assert!(runtime.transfer("overflow").is_none());
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn disconnect_removes_peer_and_channel_state() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        runtime.set_peer(test_peer("peer", Vec::new()));
        let (tx, _rx) = mpsc::channel(1);
        runtime.add_peer_channel("peer".to_string(), tx);
        runtime.remove_peer("peer");
        assert!(runtime.peers_snapshot().is_empty());
        assert!(!runtime.send_to_peer("peer", WireMessage::Ping { at_ms: 1 }));
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn duplicate_connection_guard_can_be_released_after_handshake_race() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        assert!(runtime.begin_peer_connection("peer"));
        assert!(!runtime.begin_peer_connection("peer"));
        runtime.cancel_peer_connection("peer");
        assert!(runtime.begin_peer_connection("peer"));
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn retry_cleanup_keeps_connection_slot_reserved_until_backoff_ends() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        assert!(runtime.begin_peer_connection("peer"));
        let (tx, _rx) = mpsc::channel(1);
        assert!(runtime.add_peer_channel("peer".to_string(), tx));

        runtime.remove_peer_for_retry("peer");

        assert!(!runtime.has_peer_channel("peer"));
        assert!(!runtime.begin_peer_connection("peer"));
        runtime.cancel_peer_connection("peer");
        assert!(runtime.begin_peer_connection("peer"));
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn peer_metrics_become_stale_after_timeout() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let mut peer = test_peer("peer", vec![METRICS_SYSTEM_CAPABILITY.to_string()]);
        peer.metrics = Some(HardwareMetrics {
            node_id: "peer".to_string(),
            sampled_at_ms: 1,
            cpu_usage_percent: Some(1.0),
            memory_used_bytes: None,
            memory_total_bytes: None,
            network_down_bytes_per_sec: None,
            network_up_bytes_per_sec: None,
            gpus: Vec::new(),
        });
        peer.metrics_received_at_ms = Some(crate::clock::now_millis() - 6_000);
        runtime.set_peer(peer);
        assert!(runtime.peers_snapshot()[0].metrics_stale);
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn mobile_transfer_requires_acceptance_and_becomes_ready_after_staging() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let session = runtime
            .create_mobile_session("test phone".to_string())
            .expect("create mobile session");
        let offer = TransferOffer {
            transfer_id: "mobile-transfer".to_string(),
            source_node_id: runtime.node_id(),
            source_name: "desktop".to_string(),
            target_node_id: runtime.node_id(),
            items: vec![TransferItem {
                item_id: "item".to_string(),
                name: "hello.txt".to_string(),
                relative_path: "hello.txt".to_string(),
                size: 3,
                sha256: "hash".to_string(),
                media_type: "text/plain".to_string(),
                directory: false,
            }],
            total_bytes: 3,
            created_at_ms: crate::clock::now_millis(),
        };
        let waiting = runtime
            .create_mobile_transfer(&session.session_id, offer)
            .expect("create mobile transfer");
        assert_eq!(waiting.progress.status, TransferStatus::Waiting);
        runtime
            .set_mobile_transfer_status(
                "mobile-transfer",
                &session.session_id,
                TransferStatus::Accepted,
            )
            .expect("accept mobile transfer");
        let ready = runtime
            .stage_mobile_item("mobile-transfer", "item", 3)
            .expect("stage item");
        assert_eq!(ready.progress.status, TransferStatus::Ready);
        let completed = runtime
            .mark_mobile_item_downloaded("mobile-transfer", "item", false)
            .expect("mark downloaded");
        assert_eq!(completed.progress.status, TransferStatus::Completed);
        assert!(runtime
            .mobile_item_path("mobile-transfer", "item")
            .is_some());
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn mobile_transfer_staging_directory_is_internal_and_cleanup_is_bounded() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let session = runtime
            .create_mobile_session("test phone".to_string())
            .expect("create mobile session");
        let transfer_id = "../outside";
        runtime
            .create_mobile_transfer(&session.session_id, mobile_offer(&runtime, transfer_id, 1))
            .expect("create mobile transfer");
        let record = runtime
            .mobile_transfer(transfer_id)
            .expect("find mobile transfer");
        runtime
            .create_mobile_transfer(&session.session_id, mobile_offer(&runtime, "", 1))
            .expect("create empty-id mobile transfer");
        let empty_id_record = runtime
            .mobile_transfer("")
            .expect("find empty-id mobile transfer");
        let staging_root = path.join("mobile-transfers");
        assert_eq!(
            record.staged_directory.parent(),
            Some(staging_root.as_path())
        );
        assert_eq!(
            empty_id_record.staged_directory.parent(),
            Some(staging_root.as_path())
        );
        assert!(record.staged_directory != staging_root);
        assert!(empty_id_record.staged_directory != staging_root);
        assert!(record
            .staged_directory
            .file_name()
            .is_some_and(|name| { Uuid::parse_str(&name.to_string_lossy()).is_ok() }));

        let outside = path.join("outside");
        std::fs::create_dir_all(&outside).expect("create outside directory");
        std::fs::write(outside.join("keep.txt"), b"keep").expect("write outside marker");
        runtime.mobile_sessions.write().clear();
        runtime
            .mobile_transfers
            .write()
            .get_mut(transfer_id)
            .expect("find transfer record")
            .staged_directory = staging_root.join("..").join("outside");
        let directories = runtime.prune_mobile_transfers();
        assert!(directories.iter().all(|directory| directory != &outside));
        assert!(outside.join("keep.txt").exists());
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn startup_removes_stale_mobile_staging_directory() {
        let path = temp_data_dir();
        let staging = path.join("mobile-transfers");
        std::fs::create_dir_all(&staging).expect("create stale staging directory");
        std::fs::write(staging.join("stale.part"), b"stale").expect("write stale file");
        let _runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        assert!(!staging.exists());
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn startup_removes_registered_desktop_temp_files() {
        let path = temp_data_dir();
        let received = path.join("received");
        std::fs::create_dir_all(&received).expect("create received directory");
        let temp = received.join("payload.meshshare-test.part");
        std::fs::write(&temp, b"stale").expect("write desktop temp file");
        {
            let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
                .expect("create runtime");
            runtime
                .register_desktop_temp_file(&temp)
                .expect("register desktop temp file");
        }
        let _restored = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("restore runtime");
        assert!(!temp.exists());
        let registry = std::fs::read_to_string(path.join(DESKTOP_TEMP_REGISTRY_FILE))
            .expect("read desktop temp registry");
        assert_eq!(registry.trim(), "[]");
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn mobile_sessions_have_a_bounded_active_count() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        for index in 0..MAX_MOBILE_SESSIONS {
            runtime
                .create_mobile_session(format!("phone-{index}"))
                .expect("create mobile session");
        }
        assert!(runtime
            .create_mobile_session("overflow".to_string())
            .is_err());
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn mobile_transfer_records_have_a_bounded_resource_count() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let session = runtime
            .create_mobile_session("test phone".to_string())
            .expect("create mobile session");
        for index in 0..MAX_MOBILE_TRANSFER_RECORDS {
            runtime
                .create_mobile_transfer(
                    &session.session_id,
                    mobile_offer(&runtime, &format!("mobile-record-{index}"), 1),
                )
                .expect("create mobile transfer record");
        }
        let error = runtime
            .create_mobile_transfer(
                &session.session_id,
                mobile_offer(&runtime, "mobile-record-overflow", 1),
            )
            .expect_err("resource guard must bound mobile records");
        assert!(error.contains("资源"));
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn stalled_mobile_transfer_fails_and_allows_another_large_transfer() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let session = runtime
            .create_mobile_session("test phone".to_string())
            .expect("create mobile session");
        runtime
            .create_mobile_transfer(
                &session.session_id,
                mobile_offer(&runtime, "stalled", u64::MAX - 1),
            )
            .expect("create mobile transfer");
        runtime
            .set_mobile_transfer_status("stalled", &session.session_id, TransferStatus::Accepted)
            .expect("accept mobile transfer");
        let record = runtime.mobile_transfer("stalled").expect("find transfer");
        std::fs::create_dir_all(&record.staged_directory).expect("create staging directory");
        runtime
            .mobile_transfers
            .write()
            .get_mut("stalled")
            .expect("find mutable transfer")
            .updated_at_ms = crate::clock::now_millis() - TRANSFER_ACTIVITY_TIMEOUT_MS - 1;

        let cleanup = runtime.prune_mobile_transfers();
        assert!(cleanup.contains(&record.staged_directory));
        assert_eq!(
            runtime
                .mobile_transfer("stalled")
                .expect("find failed transfer")
                .progress
                .status,
            TransferStatus::Failed
        );
        runtime
            .create_mobile_transfer(
                &session.session_id,
                mobile_offer(&runtime, "after-stalled", 1),
            )
            .expect("another transfer is accepted");
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn recently_completed_mobile_transfer_uses_completion_time_for_retention() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let session = runtime
            .create_mobile_session("test phone".to_string())
            .expect("create mobile session");
        let mut offer = mobile_offer(&runtime, "recent-mobile", 1);
        offer.created_at_ms = crate::clock::now_millis() - 11 * 60 * 1000;
        runtime
            .create_mobile_transfer(&session.session_id, offer)
            .expect("create mobile transfer");
        runtime
            .set_mobile_transfer_status(
                "recent-mobile",
                &session.session_id,
                TransferStatus::Accepted,
            )
            .expect("accept mobile transfer");
        runtime
            .stage_mobile_item("recent-mobile", "recent-mobile-item", 1)
            .expect("stage mobile item");
        runtime
            .mark_mobile_item_downloaded("recent-mobile", "recent-mobile-item", false)
            .expect("download mobile item");

        runtime.prune_mobile_transfers();
        assert!(runtime.mobile_transfer("recent-mobile").is_some());
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn old_terminal_transfers_are_pruned() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let mut offer = mobile_offer(&runtime, "old-terminal", 1);
        offer.created_at_ms = crate::clock::now_millis() - 11 * 60 * 1000;
        runtime.add_outgoing_transfer(offer);
        runtime
            .fail_transfer(
                "old-terminal",
                "done".to_string(),
                TransferStatus::Cancelled,
            )
            .expect("cancel transfer");
        runtime
            .transfers
            .write()
            .get_mut("old-terminal")
            .expect("find terminal transfer")
            .updated_at_ms = crate::clock::now_millis() - 11 * 60 * 1000;
        runtime.prune_transfers();
        assert!(runtime.transfer("old-terminal").is_none());
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn failed_transfer_removal_drops_record_and_keeps_active() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        runtime.add_outgoing_transfer(mobile_offer(&runtime, "removable", 1));
        runtime
            .fail_transfer("removable", "failed".to_string(), TransferStatus::Failed)
            .expect("fail transfer");
        runtime.add_outgoing_transfer(mobile_offer(&runtime, "active", 1));

        assert!(runtime.remove_failed_transfer("active").is_none());
        assert!(runtime.transfer("active").is_some());

        let removed = runtime
            .remove_failed_transfer("removable")
            .expect("remove failed transfer");
        assert_eq!(removed.status, TransferStatus::Failed);
        assert!(runtime.transfer("removable").is_none());
        assert!(runtime
            .transfers_snapshot(None)
            .iter()
            .all(|progress| progress.transfer_id != "removable"));
        assert!(runtime.remove_failed_transfer("removable").is_none());
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn failed_mobile_transfer_removal_releases_manifest_bytes() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let session = runtime
            .create_mobile_session("test phone".to_string())
            .expect("create mobile session");
        let offer = mobile_offer(&runtime, "mobile-removable", 1);
        let manifest_bytes = super::mobile_manifest_size(&offer);
        runtime
            .create_mobile_transfer(&session.session_id, offer)
            .expect("create mobile transfer");
        assert_eq!(
            runtime
                .mobile_manifest_bytes
                .load(std::sync::atomic::Ordering::SeqCst),
            manifest_bytes
        );
        assert!(runtime
            .remove_failed_mobile_transfer("mobile-removable")
            .is_none());
        runtime
            .fail_mobile_transfer("mobile-removable", "failed".to_string())
            .expect("fail mobile transfer");

        let removed = runtime
            .remove_failed_mobile_transfer("mobile-removable")
            .expect("remove failed mobile transfer");
        assert_eq!(removed.progress.status, TransferStatus::Failed);
        assert!(runtime.mobile_transfer("mobile-removable").is_none());
        assert_eq!(
            runtime
                .mobile_manifest_bytes
                .load(std::sync::atomic::Ordering::SeqCst),
            0
        );
        runtime
            .create_mobile_transfer(
                &session.session_id,
                mobile_offer(&runtime, "mobile-after-removal", 1),
            )
            .expect("create mobile transfer after removal");
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn failed_transfer_retry_preserves_progress_and_increments_generation() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        runtime.add_outgoing_transfer(mobile_offer(&runtime, "retryable", 8));
        {
            let mut transfers = runtime.transfers.write();
            let record = transfers.get_mut("retryable").expect("find transfer");
            record.progress.status = TransferStatus::Failed;
            record.progress.completed_bytes = 4;
            record.progress.completed_items = 1;
            record.progress.error = Some("temporary".to_string());
        }
        let progress = runtime
            .retry_transfer("retryable", TransferStatus::Accepted, None)
            .expect("retry failed transfer");
        assert_eq!(progress.status, TransferStatus::Accepted);
        assert_eq!(progress.retry_count, 1);
        assert_eq!(progress.completed_bytes, 4);
        assert_eq!(progress.completed_items, 1);
        assert_eq!(progress.error, None);
        assert!(runtime
            .retry_transfer("retryable", TransferStatus::Accepted, Some(1))
            .is_err());
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn inactive_transfer_is_failed_by_lifecycle_cleanup() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        runtime.add_outgoing_transfer(mobile_offer(&runtime, "inactive", 1));
        runtime
            .transfers
            .write()
            .get_mut("inactive")
            .expect("find transfer")
            .updated_at_ms = crate::clock::now_millis() - 6 * 60 * 1000;
        runtime.prune_transfers();
        assert_eq!(
            runtime
                .transfer("inactive")
                .expect("find failed transfer")
                .progress
                .status,
            TransferStatus::Failed
        );
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn clipboard_queue_overflow_schedules_latest_snapshot_resync() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let event = runtime
            .create_local_clipboard_named(Some("latest".to_string()), None, "local".to_string())
            .expect("create clipboard event");
        let (tx, mut rx) = mpsc::channel(1);
        runtime.set_peer(test_peer("peer", vec!["clipboard.text".to_string()]));
        runtime.add_peer_channel("peer".to_string(), tx);
        assert!(runtime.send_to_peer("peer", WireMessage::Ping { at_ms: 1 }));
        assert!(!runtime.send_to_peer(
            "peer",
            WireMessage::ClipboardUpdate {
                event: event.clone(),
            }
        ));
        let _ = rx.try_recv().expect("drain full queue");
        assert!(runtime.flush_clipboard_resync("peer"));
        match rx.try_recv().expect("receive snapshot resync") {
            WireMessage::ClipboardSnapshot { snapshot } => {
                assert_eq!(snapshot.text.as_deref(), Some("latest"))
            }
            _ => panic!("expected clipboard snapshot"),
        }
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn clipboard_messages_skip_peers_without_clipboard_capability() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let event = runtime
            .create_local_clipboard_named(Some("manual".to_string()), None, "local".to_string())
            .expect("create clipboard event");
        let (tx, mut rx) = mpsc::channel(8);
        runtime.set_peer(test_peer(
            "android",
            vec![
                "transfer.file".to_string(),
                METRICS_NETWORK_CAPABILITY.to_string(),
            ],
        ));
        runtime.add_peer_channel("android".to_string(), tx);

        assert!(runtime.send_to_peer(
            "android",
            WireMessage::ClipboardUpdate {
                event: event.clone()
            },
        ));
        runtime.send_to_peers_except(None, WireMessage::ClipboardUpdate { event });
        assert!(rx.try_recv().is_err());

        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn clipboard_messages_reach_view_only_peers() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let event = runtime
            .create_local_clipboard_named(Some("view".to_string()), None, "local".to_string())
            .expect("create clipboard event");
        let (tx, mut rx) = mpsc::channel(8);
        runtime.set_peer(test_peer(
            "android",
            vec![CLIPBOARD_VIEW_CAPABILITY.to_string()],
        ));
        runtime.add_peer_channel("android".to_string(), tx);

        assert!(runtime.send_to_peer("android", WireMessage::ClipboardUpdate { event },));
        assert!(matches!(
            rx.try_recv(),
            Ok(WireMessage::ClipboardUpdate { .. })
        ));

        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn concurrent_clipboard_updates_keep_the_newest_hlc() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let older = ClipboardEvent {
            event_id: "older".to_string(),
            origin_node_id: "peer-a".to_string(),
            clock: Hlc {
                physical_ms: 1,
                logical: 0,
                node_id: "peer-a".to_string(),
            },
            text: Some("older".to_string()),
            image_png: None,
            updated_at_ms: 1,
        };
        let newer = ClipboardEvent {
            event_id: "newer".to_string(),
            origin_node_id: "peer-b".to_string(),
            clock: Hlc {
                physical_ms: 2,
                logical: 0,
                node_id: "peer-b".to_string(),
            },
            text: Some("newer".to_string()),
            image_png: None,
            updated_at_ms: 2,
        };
        let older_runtime = runtime.clone();
        let older_barrier = barrier.clone();
        let older_thread = std::thread::spawn(move || {
            older_barrier.wait();
            older_runtime.accept_clipboard(older, "older".to_string());
        });
        let newer_runtime = runtime.clone();
        let newer_barrier = barrier.clone();
        let newer_thread = std::thread::spawn(move || {
            newer_barrier.wait();
            newer_runtime.accept_clipboard(newer, "newer".to_string());
        });
        barrier.wait();
        older_thread.join().expect("older update thread");
        newer_thread.join().expect("newer update thread");
        assert_eq!(
            runtime
                .clipboard_snapshot()
                .expect("clipboard snapshot")
                .text
                .as_deref(),
            Some("newer")
        );
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn local_clipboard_accepts_png_payloads() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let mut png = Vec::new();
        image::DynamicImage::new_rgba8(1, 1)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .expect("encode png");
        let event = runtime
            .create_local_clipboard(None, Some(png))
            .expect("create image clipboard event");
        assert!(event.image_png.is_some());
        assert!(runtime
            .clipboard_snapshot()
            .expect("clipboard snapshot")
            .image_png
            .is_some());
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn persisted_current_snapshot_advances_clock_after_restart() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let future_clock = Hlc {
            physical_ms: crate::clock::now_millis() + 60_000,
            logical: 7,
            node_id: "peer".to_string(),
        };
        assert!(runtime.accept_clipboard(
            ClipboardEvent {
                event_id: "future-event".to_string(),
                origin_node_id: "peer".to_string(),
                clock: future_clock,
                text: Some("future".to_string()),
                image_png: None,
                updated_at_ms: crate::clock::now_millis(),
            },
            "peer".to_string(),
        ));
        let current = runtime.clipboard_snapshot().expect("current snapshot");
        drop(runtime);

        let restored = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("restore runtime");
        let next = restored.clock.tick();
        assert!(next.compare(&current.clock).is_gt());
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn startup_removes_clipboard_history_temporary_files() {
        let path = temp_data_dir();
        let payload_dir = path.join("clipboard-history-payloads");
        std::fs::create_dir_all(&payload_dir).expect("create payload directory");
        for file in [
            path.join("clipboard-history.json.tmp-123"),
            path.join("clipboard-current.json.tmp-123"),
            payload_dir.join("payload.txt.tmp-123"),
            payload_dir.join("payload.png.tmp-123"),
        ] {
            std::fs::write(file, b"stale").expect("write stale clipboard file");
        }
        let _runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        assert!(!path.join("clipboard-history.json.tmp-123").exists());
        assert!(!path.join("clipboard-current.json.tmp-123").exists());
        assert!(!payload_dir.join("payload.txt.tmp-123").exists());
        assert!(!payload_dir.join("payload.png.tmp-123").exists());
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn replaying_current_snapshot_does_not_recreate_cleared_history() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        runtime
            .create_local_clipboard_named(Some("current".to_string()), None, "local".to_string())
            .expect("create clipboard event");
        let current = runtime.clipboard_snapshot().expect("current snapshot");
        drop(runtime);

        let restored = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("restore runtime");
        restored
            .clear_clipboard_history()
            .expect("clear clipboard history");
        assert!(!restored.accept_clipboard(
            ClipboardEvent {
                event_id: current.event_id.clone(),
                origin_node_id: current.source_node_id.clone(),
                clock: current.clock.clone(),
                text: current.text.clone(),
                image_png: current.image_png.clone(),
                updated_at_ms: current.updated_at_ms,
            },
            current.source_name.clone(),
        ));
        assert!(restored.clipboard_history_list().items.is_empty());
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn clipboard_history_persists_deduplicates_and_restores_current() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        runtime
            .create_local_clipboard_named(Some("one".to_string()), None, "local".to_string())
            .expect("first clipboard event");
        runtime
            .create_local_clipboard_named(Some("two".to_string()), None, "local".to_string())
            .expect("second clipboard event");
        runtime
            .create_local_clipboard_named(Some("one".to_string()), None, "local".to_string())
            .expect("duplicate clipboard event");
        let history = runtime.clipboard_history_list();
        assert_eq!(history.items.len(), 2);
        assert_eq!(history.items[0].text_preview.as_deref(), Some("one"));

        let restored = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("restore runtime");
        assert_eq!(
            restored
                .clipboard_snapshot()
                .expect("current snapshot")
                .text
                .as_deref(),
            Some("one")
        );
        let restored_history = restored.clipboard_history_list();
        assert_eq!(restored_history.items.len(), 2);
        let cleared = restored
            .clear_clipboard_history()
            .expect("clear clipboard history");
        assert_eq!(cleared, 2);
        assert!(restored.clipboard_history_list().items.is_empty());
        assert!(restored.clipboard_snapshot().is_some());
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn clipboard_history_count_setting_prunes_immediately() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        runtime
            .set_clipboard_history_settings(ClipboardHistoryMode::Count, 2)
            .expect("set history count");
        for value in ["one", "two", "three"] {
            runtime
                .create_local_clipboard_named(Some(value.to_string()), None, "local".to_string())
                .expect("create clipboard event");
        }
        let history = runtime.clipboard_history_list();
        assert_eq!(history.items.len(), 2);
        assert_eq!(history.items[0].text_preview.as_deref(), Some("three"));
        assert_eq!(history.items[1].text_preview.as_deref(), Some("two"));
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn clipboard_history_days_prune_uses_local_recorded_time() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        runtime
            .create_local_clipboard_named(Some("old".to_string()), None, "local".to_string())
            .expect("create clipboard event");
        runtime.clipboard_history.lock()[0].recorded_at_ms =
            crate::clock::now_millis() - 2 * 24 * 60 * 60 * 1000;
        runtime
            .create_local_clipboard_named(Some("new".to_string()), None, "local".to_string())
            .expect("create clipboard event");
        runtime
            .set_clipboard_history_settings(ClipboardHistoryMode::Days, 1)
            .expect("set history days");
        let history = runtime.clipboard_history_list();
        assert_eq!(history.items.len(), 1);
        assert_eq!(history.items[0].text_preview.as_deref(), Some("new"));
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn clipboard_history_write_failure_keeps_previous_record_and_payload() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        runtime
            .create_local_clipboard_named(Some("old".to_string()), None, "local".to_string())
            .expect("create old clipboard event");
        let old = runtime.clipboard_history.lock()[0].clone();
        let history_path = path.join(CLIPBOARD_HISTORY_FILE);
        std::fs::remove_file(&history_path).expect("remove history index");
        std::fs::create_dir(&history_path).expect("block history index");

        let result = runtime.create_local_clipboard_named(
            Some("new".to_string()),
            None,
            "local".to_string(),
        );
        assert!(
            result.is_some(),
            "failed history write must not block publication"
        );
        assert_eq!(
            super::load_clipboard_current(&path)
                .expect("load current snapshot")
                .text
                .as_deref(),
            Some("new")
        );
        let history = runtime.clipboard_history.lock();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].event_id, old.event_id);
        assert_eq!(history[0].text_path, old.text_path);
        drop(history);
        assert_eq!(
            std::fs::read_dir(super::clipboard_payload_dir(&path))
                .expect("list history payloads")
                .count(),
            1,
            "failed history write must discard the new payload"
        );
        std::fs::remove_dir(&history_path).expect("remove blocking directory");
        let detail = runtime
            .clipboard_history_detail(&old.event_id)
            .expect("old history detail");
        assert_eq!(detail.snapshot.text.as_deref(), Some("old"));
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn clipboard_history_prunes_oldest_records_at_byte_limit() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        for value in ["one", "two", "three"] {
            runtime
                .create_local_clipboard_named(Some(value.to_string()), None, "local".to_string())
                .expect("create clipboard event");
        }
        let config = runtime.config.lock().clone();
        let mut history = runtime.clipboard_history.lock().clone();
        prune_clipboard_history_records_with_limit(
            &mut history,
            &config,
            crate::clock::now_millis(),
            8,
        );
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].text_len, 5);
        assert_eq!(history[1].text_len, 3);
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn older_hlc_event_is_kept_in_history_without_replacing_current() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        runtime
            .create_local_clipboard_named(Some("new".to_string()), None, "local".to_string())
            .expect("create clipboard event");
        let accepted_as_current = runtime.accept_clipboard(
            ClipboardEvent {
                event_id: "older-event".to_string(),
                origin_node_id: "peer".to_string(),
                clock: Hlc {
                    physical_ms: 1,
                    logical: 0,
                    node_id: "peer".to_string(),
                },
                text: Some("old".to_string()),
                image_png: None,
                updated_at_ms: 1,
            },
            "peer".to_string(),
        );
        assert!(!accepted_as_current);
        assert_eq!(
            runtime
                .clipboard_snapshot()
                .expect("current snapshot")
                .text
                .as_deref(),
            Some("new")
        );
        assert!(runtime
            .clipboard_history_list()
            .items
            .iter()
            .any(|item| item.text_preview.as_deref() == Some("old")));
        let _ = std::fs::remove_dir_all(path);
    }
}
