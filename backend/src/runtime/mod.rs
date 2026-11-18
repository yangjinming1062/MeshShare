use crate::clock::{now_millis, LogicalClock};
use crate::model::{
    AddressInfo, ClipboardEvent, ClipboardHistoryDetail, ClipboardHistoryItem,
    ClipboardHistoryList, ClipboardHistoryMode, ClipboardHistorySettings, ClipboardSession,
    ClipboardSnapshot, ClipboardStatus, HardwareMetrics, MobileAccessInfo, MobileSession,
    MobileTransferView, MonitorPosition, NodeInfo, PeerInfo, ReceiveMode, ReceiveSettings,
    TransferHistoryEntry, TransferItem, TransferOffer, TransferProgress, TransferStatus,
    WireMessage, CLIPBOARD_IMAGE_LIMIT, CLIPBOARD_TEXT_LIMIT, CLIPBOARD_VIEW_CAPABILITY,
    DEFAULT_CLIPBOARD_HISTORY_COUNT, DEFAULT_CLIPBOARD_HISTORY_DAYS, MAX_CLIPBOARD_HISTORY_COUNT,
    MAX_CLIPBOARD_HISTORY_DAYS, MAX_HISTORY_DELETE_BATCH, MAX_MOBILE_SESSIONS,
    MAX_TRANSFER_HISTORY_PATHS, MAX_TRANSFER_RECORDS, METRICS_GPU_CAPABILITY,
    METRICS_GPU_VIEW_CAPABILITY, METRICS_NETWORK_CAPABILITY, METRICS_SYSTEM_CAPABILITY,
    METRICS_SYSTEM_VIEW_CAPABILITY, REMOTE_FILES_CAPABILITY, REMOTE_MANAGEMENT_CAPABILITY,
    TRANSFER_HISTORY_CAPABILITY, TRANSFER_HISTORY_LIMIT,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use futures_util::StreamExt;
#[cfg(not(target_os = "android"))]
use parking_lot::Condvar;
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::net::Ipv4Addr;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::sync::{broadcast, mpsc};
use tokio_util::io::ReaderStream;
use url::Url;
use uuid::Uuid;
use walkdir::WalkDir;

mod config;
mod history;
mod network;
mod outgoing;

use config::*;
use history::*;
use network::*;
use outgoing::*;

pub(crate) use config::file_modified_ms;
pub use network::bind_addresses;
pub(crate) use network::{is_network_ipv4, normalize_peer_http_base};
pub use outgoing::validate_receive_directory;

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

fn parse_update_proxy(value: &str) -> anyhow::Result<Url> {
    let url = Url::parse(value).map_err(|error| anyhow::anyhow!("更新代理地址无效：{error}"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(anyhow::anyhow!("更新代理只支持 http 或 https 地址"));
    }
    if url.host_str().is_none_or(|host| host.is_empty()) {
        return Err(anyhow::anyhow!("更新代理地址缺少主机名"));
    }
    Ok(url)
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
    update_proxy: Option<String>,
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
    /// 所选接收目录的 macOS 安全作用域书签。
    /// 其他平台保持未设置，继续使用 path 字段。
    #[serde(default)]
    receive_directory_bookmark: Option<Vec<u8>>,
    /// 是否允许其他设备在远程管理中浏览本机目录并选择发送；默认关闭。
    #[serde(default)]
    remote_file_access: bool,
}

/// 界面使用的接收设置：配置保留规范化路径，展示时去掉 Windows 扩展长度前缀。
fn receive_settings_view(config: &PersistedConfig) -> ReceiveSettings {
    ReceiveSettings {
        mode: config.receive_mode,
        default_path: config
            .receive_directory
            .as_deref()
            .map(|path| crate::fsutil::display_absolute_path(Path::new(path))),
    }
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

/// 出站任务每一项的源：桌面端直接引用原文件，Android 仍先把字节复制进暂存目录。
#[derive(Debug, Clone)]
pub enum OutgoingItemSource {
    Staged,
    Original {
        path: PathBuf,
        size: u64,
        modified_ms: Option<i64>,
    },
}

/// 发送端已经接手本次传输：清单完整后就不再需要页面持有源文件。
#[derive(Debug, Clone)]
pub struct StagedOutgoing {
    pub offer: TransferOffer,
    pub directory: PathBuf,
    pub sources: HashMap<String, OutgoingItemSource>,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone)]
struct PreparedSource {
    file_name: String,
    relative_path: String,
    path: PathBuf,
    size: u64,
    modified_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum OutgoingPrepareState {
    Preparing,
    Failed(String),
}

#[derive(Debug, Clone)]
struct OutgoingPrepare {
    target_node_id: String,
    sources: Vec<PreparedSource>,
    source_roots: Vec<String>,
    total_bytes: u64,
    hashed_bytes: u64,
    items_ready: usize,
    state: OutgoingPrepareState,
    created_at_ms: i64,
}

/// 页面轮询的准备进度。`state` 为 `preparing` 时还在枚举与计算摘要。
#[derive(Debug, Clone, Serialize)]
pub struct OutgoingPrepareView {
    pub transfer_id: String,
    pub state: String,
    pub error: Option<String>,
    pub hashed_bytes: u64,
    pub total_bytes: u64,
    pub items_ready: usize,
    pub total_items: usize,
}

#[derive(Debug, Clone)]
pub struct TransferRecord {
    pub offer: TransferOffer,
    pub progress: TransferProgress,
    pub accepted_destination: Option<PathBuf>,
    pub received_items: HashSet<String>,
    /// 创建时快照的对端显示名。对端随后离线时历史仍能显示设备名。
    pub peer_name: Option<String>,
    /// 发送方向用户选择的根路径；接收与暂存发送为空。
    pub source_paths: Vec<String>,
    /// 已提交条目的累计字节；进度展示在此基础上叠加当前条目的在途字节。
    pub committed_bytes: u64,
    /// 当前条目已接收但尚未提交的字节，条目提交后清空。
    pub upload_in_flight: Option<(String, u64)>,
    pub updated_at_ms: i64,
}

/// 传输历史与尚未送达对端的同步删除。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct TransferHistoryStore {
    #[serde(default)]
    entries: Vec<TransferHistoryEntry>,
    #[serde(default)]
    pending_deletions: HashMap<String, Vec<String>>,
}

/// 清空历史的返回值：本地删除条数，以及需要同步给对端的 id 分组。
#[derive(Debug, Clone)]
pub struct TransferHistoryCleared {
    pub cleared: usize,
    pub peer_batches: Vec<(String, Vec<String>)>,
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
    /// 创建时算好的清单序列化字节数，用于全局清单内存预算。
    pub manifest_bytes: u64,
    pub updated_at_ms: i64,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TransferRetryError {
    InstallingUpdate,
    NotRetryable,
}

pub(crate) const UPDATE_INSTALLING_MESSAGE: &str = "应用正在安装更新，请稍后重试";

/// 单条传输进度的上报节流间隔。
pub(crate) const TRANSFER_PROGRESS_INTERVAL: Duration = Duration::from_millis(200);

struct MobileCa {
    path: PathBuf,
    fingerprint: String,
}

struct ClipboardState {
    snapshot: Option<ClipboardSnapshot>,
    #[cfg(not(target_os = "android"))]
    pending_system_write: bool,
}

pub struct NodeRuntime {
    profile: NodeProfile,
    pub node: RwLock<NodeInfo>,
    pub clock: LogicalClock,
    clipboard: Mutex<ClipboardState>,
    #[cfg(not(target_os = "android"))]
    system_clipboard_ready: Condvar,
    clipboard_status: RwLock<ClipboardStatus>,
    clipboard_history: Mutex<Vec<ClipboardHistoryRecord>>,
    pub peers: RwLock<HashMap<String, PeerInfo>>,
    pub transfers: RwLock<HashMap<String, TransferRecord>>,
    pub mobile_sessions: RwLock<HashMap<String, MobileSession>>,
    pub mobile_transfers: RwLock<HashMap<String, MobileTransferRecord>>,
    mobile_manifest_bytes: AtomicU64,
    network_rebind_generation: AtomicU64,
    pub local_metrics: RwLock<Option<HardwareMetrics>>,
    pub monitor_visible: AtomicBool,
    pub monitor_locked: AtomicBool,
    mobile_ca: RwLock<Option<MobileCa>>,
    config: Mutex<PersistedConfig>,
    data_dir: PathBuf,
    clipboard_update: Mutex<()>,
    seen_events: Mutex<HashMap<String, i64>>,
    peer_channels: Mutex<HashMap<String, mpsc::Sender<WireMessage>>>,
    pending_clipboard_resync: Mutex<HashSet<String>>,
    connecting_peers: Mutex<HashSet<String>>,
    transfer_locks: Mutex<HashSet<(String, String)>>,
    desktop_temp_files: Mutex<HashSet<PathBuf>>,
    staged_outgoing: Mutex<HashMap<String, StagedOutgoing>>,
    outgoing_prepares: Mutex<HashMap<String, OutgoingPrepare>>,
    outgoing_uploads: Mutex<HashSet<String>>,
    pub events: broadcast::Sender<WireMessage>,
    pub sync_enabled: AtomicBool,
    pub stopping: AtomicBool,
    update_installing: AtomicBool,
    pub http: reqwest::Client,
    pub proxy_http: reqwest::Client,
    /// 上传用客户端不设整体超时：单个大文件允许长时间传输。
    upload_http: reqwest::Client,
    transfer_update_gate: Mutex<()>,
    /// 最近 10 条终态传输的摘要，以及等待对端重连后补发的删除。
    transfer_history: Mutex<TransferHistoryStore>,
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
                REMOTE_FILES_CAPABILITY.to_string(),
                TRANSFER_HISTORY_CAPABILITY.to_string(),
            ],
            Self::Android => vec![
                "transfer.file".to_string(),
                "transfer.folder".to_string(),
                CLIPBOARD_VIEW_CAPABILITY.to_string(),
                METRICS_NETWORK_CAPABILITY.to_string(),
                METRICS_SYSTEM_VIEW_CAPABILITY.to_string(),
                METRICS_GPU_VIEW_CAPABILITY.to_string(),
                TRANSFER_HISTORY_CAPABILITY.to_string(),
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
/// 接收端已确认但发送端迟迟没有开始上传，用更短的窗口避免两端长时间对视。
const TRANSFER_ACCEPTED_IDLE_TIMEOUT_MS: i64 = 90 * 1000;
const MAX_TERMINAL_TRANSFERS: usize = 256;
const MAX_FAILED_TRANSFERS: usize = 20;
const MAX_MOBILE_TERMINAL_TRANSFERS_PER_SESSION: usize = 128;
const MAX_MOBILE_FAILED_TRANSFERS_PER_SESSION: usize = 20;
/// 全部会话合计的失败记录上限，避免大量会话把失败列表撑满。
const MAX_MOBILE_FAILED_TRANSFERS: usize = 64;
const MAX_MOBILE_TRANSFER_RECORDS: usize = 512;
const MAX_MOBILE_MANIFEST_BYTES: u64 = 128 * 1024 * 1024;
const MAX_MOBILE_CA_BYTES: usize = 64 * 1024;
const MAX_SEEN_CLIPBOARD_EVENTS: usize = 4_096;
const OUTGOING_STAGING_DIR: &str = "outgoing";
const OUTGOING_UPLOAD_IDLE_POLL_MS: u64 = 500;
const OUTGOING_ITEM_UPLOAD_ATTEMPTS: usize = 4;
const OUTGOING_HASH_CHUNK_BYTES: usize = 256 * 1024;
const OUTGOING_PREPARE_MAX_SELECTION: usize = 512;
const OUTGOING_PREPARE_MAX_ITEMS: usize = 20_000;
const OUTGOING_PREPARE_RETENTION_MS: i64 = 60 * 60 * 1000;
const OUTGOING_STAGING_RETENTION_MS: i64 = 60 * 60 * 1000;
const OUTGOING_MAX_RELATIVE_PATH: usize = 4_096;
const DESKTOP_TEMP_REGISTRY_FILE: &str = "desktop-transfer-temp-files.json";
const MAX_DESKTOP_TEMP_FILES: usize = 512;
const CLIPBOARD_HISTORY_FILE: &str = "clipboard-history.json";
const CLIPBOARD_CURRENT_FILE: &str = "clipboard-current.json";
const CLIPBOARD_PAYLOAD_DIR: &str = "clipboard-history-payloads";
const CLIPBOARD_HISTORY_PREVIEW_BYTES: usize = 256;
const CLIPBOARD_DAY_MS: i64 = 24 * 60 * 60 * 1000;
const MAX_CLIPBOARD_HISTORY_BYTES: u64 = 512 * 1024 * 1024;
const TRANSFER_HISTORY_FILE: &str = "transfer-history.json";
const MAX_PENDING_HISTORY_DELETIONS_PER_PEER: usize = MAX_HISTORY_DELETE_BATCH;

fn history_settings(config: &PersistedConfig) -> ClipboardHistorySettings {
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

fn is_terminal_transfer(status: &TransferStatus) -> bool {
    matches!(
        status,
        TransferStatus::Completed
            | TransferStatus::Rejected
            | TransferStatus::Failed
            | TransferStatus::Cancelled
    )
}

pub(crate) fn valid_history_delete_ids(ids: &[String]) -> bool {
    !ids.is_empty()
        && ids.len() <= MAX_HISTORY_DELETE_BATCH
        && ids
            .iter()
            .all(|id| !id.trim().is_empty() && id.len() <= 256)
}

fn short_node_id(node_id: &str) -> String {
    node_id.chars().take(8).collect()
}

/// 清单在全局内存预算里的占用；创建传输时算一次并记录在案。
fn mobile_manifest_size(offer: &TransferOffer) -> u64 {
    serde_json::to_vec(offer)
        .map(|value| value.len() as u64)
        .unwrap_or_default()
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
        // 启动时清空网页暂存与出站暂存：两处内容只服务于进程内任务。
        crate::fsutil::remove_dir_if_exists(&data_dir.join("mobile-transfers"))?;
        crate::fsutil::remove_dir_if_exists(&data_dir.join(OUTGOING_STAGING_DIR))?;
        let desktop_temp_files = load_and_cleanup_desktop_temp_files(&data_dir)?;
        cleanup_clipboard_temp_files(&data_dir)?;
        let config = load_or_create_config(&data_dir, profile)?;
        #[cfg(target_os = "macos")]
        let mut config = config;
        #[cfg(target_os = "macos")]
        {
            // 书签支持之前写入的配置只含路径。
            // 尝试在不打开面板的情况下迁移该路径；
            // 若 macOS 仍要求用户授权，
            // 下一次显式选择文件夹时会创建书签。
            if config.receive_directory_bookmark.is_none() {
                if let Some(path) = config.receive_directory.as_deref().map(Path::new) {
                    if let Ok(bookmark) = crate::macos::security_scope::create_bookmark(path) {
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
            clipboard: Mutex::new(ClipboardState {
                snapshot: clipboard,
                #[cfg(not(target_os = "android"))]
                pending_system_write: false,
            }),
            #[cfg(not(target_os = "android"))]
            system_clipboard_ready: Condvar::new(),
            clipboard_status: RwLock::new(ClipboardStatus::default()),
            clipboard_history: Mutex::new(clipboard_history),
            transfer_history: Mutex::new(load_transfer_history(&data_dir)),
            peers: RwLock::new(HashMap::new()),
            transfers: RwLock::new(HashMap::new()),
            mobile_sessions: RwLock::new(HashMap::new()),
            mobile_transfers: RwLock::new(HashMap::new()),
            mobile_manifest_bytes: AtomicU64::new(0),
            network_rebind_generation: AtomicU64::new(0),
            local_metrics: RwLock::new(None),
            monitor_visible: AtomicBool::new(config.monitor_visible),
            monitor_locked: AtomicBool::new(config.monitor_locked),
            mobile_ca: RwLock::new(None),
            config: Mutex::new(config),
            data_dir,
            clipboard_update: Mutex::new(()),
            seen_events: Mutex::new(HashMap::new()),
            peer_channels: Mutex::new(HashMap::new()),
            pending_clipboard_resync: Mutex::new(HashSet::new()),
            connecting_peers: Mutex::new(HashSet::new()),
            transfer_locks: Mutex::new(HashSet::new()),
            desktop_temp_files: Mutex::new(desktop_temp_files),
            staged_outgoing: Mutex::new(HashMap::new()),
            outgoing_prepares: Mutex::new(HashMap::new()),
            outgoing_uploads: Mutex::new(HashSet::new()),
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
            upload_http: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(8))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            transfer_update_gate: Mutex::new(()),
        }))
    }

    pub fn is_android_profile(&self) -> bool {
        self.profile.is_android()
    }

    pub fn network_rebind_generation(&self) -> u64 {
        self.network_rebind_generation.load(Ordering::SeqCst)
    }

    #[cfg(any(target_os = "android", test))]
    pub fn request_network_rebind(&self) {
        self.network_rebind_generation
            .fetch_add(1, Ordering::SeqCst);
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

    /// 只返回暂存根目录的直接子目录。
    ///
    /// 路径保存在内存记录中，因此该检查刻意只做词法判断，
    /// 拒绝根目录本身、父级组件以及嵌套路径。
    /// 它是清理调用方执行递归删除之前的最后一道守卫。
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
                    outgoing.cpu_model = None;
                    outgoing.cpu_logical_cores = None;
                    outgoing.cpu_physical_cores = None;
                    outgoing.cpu_frequency_mhz = None;
                    outgoing.memory_used_bytes = None;
                    outgoing.memory_total_bytes = None;
                    outgoing.memory_available_bytes = None;
                    outgoing.swap_used_bytes = None;
                    outgoing.swap_total_bytes = None;
                    outgoing.load_average = None;
                    outgoing.uptime_seconds = None;
                    outgoing.disks.clear();
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

    /// 移除拨号方当前的通道，同时保留其连接槽位。
    ///
    /// 发现任务在重连退避期间持有该槽位。
    /// 保持占用可防止一波 mDNS 解析同时开出多个并行 socket，
    /// 绕过断连后的指数重试延迟。
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
        const RETRY_DELAYS: [Duration; 3] = [
            Duration::from_millis(100),
            Duration::from_millis(250),
            Duration::from_millis(500),
        ];
        if self.send_to_peer(node_id, message.clone()) {
            return true;
        }
        for delay in RETRY_DELAYS {
            if !self.has_peer_channel(node_id) {
                return false;
            }
            tokio::time::sleep(delay).await;
            if self.send_to_peer(node_id, message.clone()) {
                return true;
            }
        }
        false
    }

    pub fn send_to_peers_except(&self, except: Option<&str>, message: WireMessage) {
        let is_clipboard = is_clipboard_message(&message);
        // 先复制通道再判定能力：持 peer_channels 锁时不能再取 peers 锁，
        // 否则与 remove_peer 的加锁顺序相反。
        let channels = self
            .peer_channels
            .lock()
            .iter()
            .filter(|(node_id, _)| Some(node_id.as_str()) != except)
            .map(|(node_id, channel)| (node_id.clone(), channel.clone()))
            .collect::<Vec<_>>();
        for (node_id, channel) in channels {
            if is_clipboard && !self.peer_supports_clipboard(&node_id) {
                continue;
            }
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
            // Hello 会刷新对端通告的地址，
            // 但不得丢弃承载当前已验证 WebSocket 通道的端点。
            // 保留该端点，
            // 能让 HTTP 管理跟随实际可达的接口。
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

    /// 空能力列表属于能力协商之前的旧对端。
    /// 对这些对端保持剪贴板投递兼容；
    /// 而显式给出不含 `clipboard.text` 或 `clipboard.view` 的能力列表，
    /// 视为主动放弃。
    pub fn peer_supports_clipboard(&self, node_id: &str) -> bool {
        self.peers.read().get(node_id).is_none_or(|peer| {
            peer.capabilities.is_empty()
                || peer
                    .capabilities
                    .iter()
                    .any(|item| item == "clipboard.text" || item == CLIPBOARD_VIEW_CAPABILITY)
        })
    }

    pub fn peer(&self, node_id: &str) -> Option<PeerInfo> {
        self.peers.read().get(node_id).cloned()
    }

    /// 对端显示名；空名称按未知处理，由调用方决定回退文案。
    pub fn peer_name(&self, node_id: &str) -> Option<String> {
        self.peers
            .read()
            .get(node_id)
            .map(|peer| peer.node_name.clone())
            .filter(|name| !name.trim().is_empty())
    }

    pub fn peer_http_base(&self, node_id: &str) -> Option<String> {
        self.peer_http_bases(node_id).into_iter().next()
    }

    /// 按本节点掌握的可用证据排序返回 HTTP 基地址。
    /// 端点是在 WebSocket 通道建立时记录的，
    /// 因此优先级高于对端通告的接口顺序。
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

    /// 配置修改统一顺序：锁内改副本，写盘成功后回写内存；写盘失败保持原值。
    fn update_config(
        &self,
        change: impl FnOnce(&mut PersistedConfig) -> anyhow::Result<()>,
    ) -> anyhow::Result<PersistedConfig> {
        let mut config = self.config.lock();
        let mut next = config.clone();
        change(&mut next)?;
        persist_config(&self.data_dir, &next)?;
        *config = next.clone();
        Ok(next)
    }

    pub fn set_monitor_visible(&self, visible: bool) -> anyhow::Result<()> {
        self.update_config(|config| {
            config.monitor_visible = visible;
            Ok(())
        })?;
        self.monitor_visible.store(visible, Ordering::SeqCst);
        Ok(())
    }

    pub fn monitor_locked(&self) -> bool {
        self.monitor_locked.load(Ordering::SeqCst)
    }

    pub fn set_monitor_locked(&self, locked: bool) -> anyhow::Result<()> {
        self.update_config(|config| {
            config.monitor_locked = locked;
            Ok(())
        })?;
        self.monitor_locked.store(locked, Ordering::SeqCst);
        Ok(())
    }

    pub fn monitor_position(&self) -> Option<MonitorPosition> {
        self.config.lock().monitor_position.clone()
    }

    pub fn set_monitor_position(&self, position: MonitorPosition) -> anyhow::Result<()> {
        self.update_config(|config| {
            config.monitor_position = Some(position);
            Ok(())
        })
        .map(|_| ())
    }

    /// 切换剪贴板同步开关并返回新状态；托盘与控制面共用。
    pub fn toggle_sync_enabled(&self) -> bool {
        let enabled = !self.sync_enabled.load(std::sync::atomic::Ordering::SeqCst);
        self.sync_enabled
            .store(enabled, std::sync::atomic::Ordering::SeqCst);
        enabled
    }

    pub fn autostart_enabled(&self) -> bool {
        self.config.lock().autostart_enabled
    }

    pub fn set_autostart_enabled(&self, enabled: bool) -> anyhow::Result<()> {
        self.update_config(|config| {
            config.autostart_enabled = enabled;
            Ok(())
        })
        .map(|_| ())
    }

    pub fn remote_file_access(&self) -> bool {
        self.config.lock().remote_file_access
    }

    pub fn set_remote_file_access(&self, enabled: bool) -> anyhow::Result<()> {
        self.update_config(|config| {
            config.remote_file_access = enabled;
            Ok(())
        })
        .map(|_| ())
    }

    pub fn receive_settings(&self) -> ReceiveSettings {
        receive_settings_view(&self.config.lock())
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
        let config = self.update_config(|config| {
            config.receive_mode = mode;
            config.receive_directory = canonical_directory
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned());
            #[cfg(target_os = "macos")]
            {
                config.receive_directory_bookmark = canonical_directory
                    .as_deref()
                    .and_then(|path| crate::macos::security_scope::create_bookmark(path).ok());
                if let Some(bookmark) = config.receive_directory_bookmark.as_deref() {
                    if let Err(error) = crate::macos::security_scope::activate_bookmark(bookmark) {
                        eprintln!("激活接收目录授权失败：{error}");
                    }
                }
            }
            Ok(())
        })?;
        Ok(receive_settings_view(&config))
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
        self.update_config(|config| {
            config.update_frequency = frequency;
            Ok(())
        })
        .map(|_| ())
    }

    pub fn set_update_check_state(
        &self,
        checked_at_ms: Option<i64>,
        pending_version: Option<String>,
    ) -> anyhow::Result<()> {
        self.update_config(|config| {
            config.update_last_checked_at_ms = checked_at_ms;
            config.pending_update_version = pending_version;
            Ok(())
        })
        .map(|_| ())
    }

    pub fn update_proxy(&self) -> Option<String> {
        self.config.lock().update_proxy.clone()
    }

    pub fn update_proxy_url(&self) -> Option<Url> {
        let value = self.update_proxy()?;
        match parse_update_proxy(&value) {
            Ok(url) => Some(url),
            Err(error) => {
                eprintln!("读取更新代理失败：{error}");
                None
            }
        }
    }

    pub fn set_update_proxy(&self, proxy: Option<String>) -> anyhow::Result<Option<String>> {
        let normalized = match proxy
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            Some(value) => Some(parse_update_proxy(value)?.to_string()),
            None => None,
        };
        self.update_config(|config| {
            config.update_proxy = normalized.clone();
            Ok(())
        })?;
        Ok(normalized)
    }

    pub fn clipboard_history_settings(&self) -> ClipboardHistorySettings {
        history_settings(&self.config.lock())
    }

    pub fn set_clipboard_history_settings(
        &self,
        mode: ClipboardHistoryMode,
        value: u64,
    ) -> anyhow::Result<ClipboardHistorySettings> {
        let config = self.update_config(|config| {
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
                    config.clipboard_history_count = count;
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
                    config.clipboard_history_days = days;
                }
            }
            config.clipboard_history_count = config
                .clipboard_history_count
                .clamp(1, MAX_CLIPBOARD_HISTORY_COUNT);
            config.clipboard_history_days = config
                .clipboard_history_days
                .clamp(1, MAX_CLIPBOARD_HISTORY_DAYS);
            config.clipboard_history_mode = mode;
            Ok(())
        })?;
        self.prune_clipboard_history();
        Ok(history_settings(&config))
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

    pub fn clipboard_history_thumbnail(&self, event_id: &str) -> Option<Vec<u8>> {
        let _update_guard = self.clipboard_update.lock();
        let history = self.clipboard_history.lock();
        history
            .iter()
            .find(|record| record.event_id == event_id)
            .and_then(|record| load_or_create_clipboard_thumbnail(&self.data_dir, record))
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
        self.accept_clipboard_locked(event, source_name, true)
    }

    fn accept_clipboard_locked(
        &self,
        event: ClipboardEvent,
        source_name: String,
        write_to_system: bool,
    ) -> bool {
        let now = now_millis();
        let mut seen = self.seen_events.lock();
        seen.retain(|_, timestamp| now.saturating_sub(*timestamp) < 10 * 60 * 1000);
        if seen.contains_key(&event.event_id) {
            return false;
        }
        let event_id = event.event_id.clone();
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
        // 只有校验通过的负载才登记为已见，避免非法内容占用去重标识。
        seen.insert(event_id, now);
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
        let (replayed_current, newer) =
            self.clipboard
                .lock()
                .snapshot
                .as_ref()
                .map_or((false, true), |current| {
                    (
                        current.event_id == snapshot.event_id,
                        snapshot.clock.compare(&current.clock).is_gt(),
                    )
                });
        if !replayed_current {
            if let Err(error) = self.upsert_clipboard_history_locked(&snapshot, now) {
                eprintln!("保存剪贴板历史失败：{error}");
            }
        }
        if !newer {
            return false;
        }
        self.clock.observe(&snapshot.clock);
        if let Err(error) = persist_clipboard_current(&self.data_dir, &snapshot) {
            eprintln!("保存当前剪贴板失败：{error}");
        }
        let mut clipboard = self.clipboard.lock();
        clipboard.snapshot = Some(snapshot);
        #[cfg(not(target_os = "android"))]
        {
            clipboard.pending_system_write = write_to_system && !self.profile.is_android();
            if clipboard.pending_system_write {
                self.system_clipboard_ready.notify_one();
            }
        }
        #[cfg(target_os = "android")]
        let _ = write_to_system;
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
        // 先写入替换用的载荷，再修改索引或删除旧记录。
        // 这样磁盘或权限失败时，
        // 旧历史条目及其载荷仍然可用于恢复。
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
            // 旧索引不引用新载荷。
            // 索引写入失败之后再删除它，同时保留旧条目。
            remove_clipboard_record_payloads(&self.data_dir, &new_record);
            return Err(error);
        }
        *history = next;
        Ok(())
    }

    /// 系统读操作可能等待其他应用。若共享状态期间已更新，保留较新的事件。
    #[cfg(not(target_os = "android"))]
    pub fn create_local_clipboard(
        &self,
        text: Option<String>,
        image_png: Option<Vec<u8>>,
        observed_event_id: Option<&str>,
    ) -> Option<ClipboardEvent> {
        let source_name = self.node.read().node_name.clone();
        let event = self.local_clipboard_event(text, image_png, &source_name)?;
        let _update_guard = self.clipboard_update.lock();
        if self.clipboard_event_id().as_deref() != observed_event_id {
            return None;
        }
        self.accept_clipboard_locked(event.clone(), source_name, false)
            .then_some(event)
    }

    pub fn create_local_clipboard_named(
        &self,
        text: Option<String>,
        image_png: Option<Vec<u8>>,
        source_name: String,
    ) -> Option<ClipboardEvent> {
        let event = self.local_clipboard_event(text, image_png, &source_name)?;
        self.accept_clipboard(event.clone(), source_name)
            .then_some(event)
    }

    fn local_clipboard_event(
        &self,
        text: Option<String>,
        image_png: Option<Vec<u8>>,
        source_name: &str,
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
        Some(ClipboardEvent {
            event_id: Uuid::new_v4().to_string(),
            origin_node_id: self.node_id(),
            clock: self.clock.tick(),
            text,
            image_png,
            updated_at_ms: now_millis(),
        })
    }

    pub fn restore_clipboard_history(&self, event_id: &str) -> Option<ClipboardEvent> {
        let detail = self.clipboard_history_detail(event_id)?;
        let image = detail
            .snapshot
            .image_png
            .as_deref()
            .and_then(|value| BASE64.decode(value).ok());
        let source_name = self.node.read().node_name.clone();
        self.create_local_clipboard_named(detail.snapshot.text, image, source_name)
    }

    pub fn clipboard_snapshot(&self) -> Option<ClipboardSnapshot> {
        self.clipboard.lock().snapshot.clone()
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn clipboard_event_id(&self) -> Option<String> {
        self.clipboard
            .lock()
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.event_id.clone())
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn take_system_clipboard_write(&self) -> Option<ClipboardSnapshot> {
        let mut clipboard = self.clipboard.lock();
        if std::mem::take(&mut clipboard.pending_system_write) {
            clipboard.snapshot.clone()
        } else {
            None
        }
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn wait_for_system_clipboard_write(&self, timeout: Duration) {
        let mut clipboard = self.clipboard.lock();
        if !clipboard.pending_system_write {
            self.system_clipboard_ready
                .wait_for(&mut clipboard, timeout);
        }
    }

    pub fn add_transfer(&self, offer: TransferOffer) -> bool {
        let _update_gate = self.transfer_update_gate.lock();
        if self.update_installing.load(Ordering::SeqCst) {
            return false;
        }
        let mut progress = transfer_progress(&offer, "receive");
        let accepted_destination = self.default_receive_directory();
        if let Some(destination) = accepted_destination.as_ref() {
            progress.destination = Some(crate::fsutil::display_absolute_path(destination));
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
                peer_name: Some(offer.source_name.clone()),
                source_paths: Vec::new(),
                committed_bytes: 0,
                upload_in_flight: None,
                updated_at_ms: now_millis(),
            },
        );
        drop(transfers);
        // 进度事件同时携带提议。先发布进度，
        // 使自动接受的传输不会在
        // 分时收到两个事件的 UI 中短暂显示为新的确认请求。
        self.publish_event(WireMessage::TransferProgress { progress });
        self.publish_event(WireMessage::TransferOffer { offer });
        true
    }

    pub fn add_outgoing_transfer(&self, offer: TransferOffer) -> TransferProgress {
        let peer_name = self.peer_name(&offer.target_node_id);
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
                peer_name,
                source_paths: Vec::new(),
                committed_bytes: 0,
                upload_in_flight: None,
                updated_at_ms: now_millis(),
            },
        );
        drop(transfers);
        self.publish_event(WireMessage::TransferProgress {
            progress: progress.clone(),
        });
        progress
    }

    fn outgoing_staging_directory(&self, transfer_id: &str) -> PathBuf {
        self.data_dir.join(OUTGOING_STAGING_DIR).join(transfer_id)
    }

    fn valid_staging_id(value: &str) -> bool {
        !value.is_empty()
            && value.len() <= 128
            && !value.contains("..")
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    }

    /// 页面把源文件写给本机节点时先登记清单，上传阶段就不再依赖页面状态。
    /// Android 的文件选择器只给 content URI，只能走这条复制路径。
    pub fn begin_outgoing_staging(&self, offer: TransferOffer) -> Result<(), String> {
        if !Self::valid_staging_id(&offer.transfer_id) {
            return Err("传输标识无效".to_string());
        }
        if offer
            .items
            .iter()
            .any(|item| !Self::valid_staging_id(&item.item_id))
        {
            return Err("传输项目标识无效".to_string());
        }
        let directory = self.outgoing_staging_directory(&offer.transfer_id);
        let mut staged = self.staged_outgoing.lock();
        if staged.contains_key(&offer.transfer_id) {
            return Err("传输已在暂存中".to_string());
        }
        if staged.len() >= MAX_TRANSFER_RECORDS {
            return Err("暂存任务数量已达到上限".to_string());
        }
        std::fs::create_dir_all(&directory)
            .map_err(|error| format!("无法创建暂存目录：{error}"))?;
        staged.insert(
            offer.transfer_id.clone(),
            StagedOutgoing {
                offer,
                directory,
                sources: HashMap::new(),
                created_at_ms: now_millis(),
            },
        );
        Ok(())
    }

    /// 桌面端按原路径直传：登记选中的文件与文件夹，由节点在后台枚举、计算摘要并登记清单。
    pub fn begin_outgoing_paths(
        self: &Arc<Self>,
        target_node_id: String,
        paths: Vec<String>,
    ) -> Result<OutgoingPrepareView, String> {
        if paths.is_empty() || paths.len() > OUTGOING_PREPARE_MAX_SELECTION {
            return Err("需要选择 1 到 512 个文件或文件夹".to_string());
        }
        let (sources, source_roots) = expand_outgoing_paths(&paths)?;
        if sources.is_empty() {
            return Err("选择的内容里没有可发送的文件".to_string());
        }
        let total_bytes = sources.iter().map(|source| source.size).sum::<u64>();
        let total_items = sources.len();
        let transfer_id = Uuid::new_v4().to_string();
        {
            let mut prepares = self.outgoing_prepares.lock();
            if prepares.len() >= MAX_TRANSFER_RECORDS {
                return Err("同时准备的发送任务过多，请稍后重试".to_string());
            }
            prepares.insert(
                transfer_id.clone(),
                OutgoingPrepare {
                    target_node_id,
                    sources,
                    source_roots,
                    total_bytes,
                    hashed_bytes: 0,
                    items_ready: 0,
                    state: OutgoingPrepareState::Preparing,
                    created_at_ms: now_millis(),
                },
            );
        }
        let runtime = self.clone();
        let id = transfer_id.clone();
        tokio::spawn(async move {
            runtime.prepare_outgoing_paths(&id).await;
        });
        Ok(OutgoingPrepareView {
            transfer_id,
            state: "preparing".to_string(),
            error: None,
            hashed_bytes: 0,
            total_bytes,
            items_ready: 0,
            total_items,
        })
    }

    fn outgoing_prepare_source_roots(&self, transfer_id: &str) -> Vec<String> {
        self.outgoing_prepares
            .lock()
            .get(transfer_id)
            .map(|prepare| prepare.source_roots.clone())
            .unwrap_or_default()
    }

    pub fn outgoing_prepare_view(&self, transfer_id: &str) -> Option<OutgoingPrepareView> {
        let prepares = self.outgoing_prepares.lock();
        let prepare = prepares.get(transfer_id)?;
        let (state, error) = match &prepare.state {
            OutgoingPrepareState::Preparing => ("preparing", None),
            OutgoingPrepareState::Failed(error) => ("failed", Some(error.clone())),
        };
        Some(OutgoingPrepareView {
            transfer_id: transfer_id.to_string(),
            state: state.to_string(),
            error,
            hashed_bytes: prepare.hashed_bytes,
            total_bytes: prepare.total_bytes,
            items_ready: prepare.items_ready,
            total_items: prepare.sources.len(),
        })
    }

    pub fn cancel_outgoing_prepare(&self, transfer_id: &str) {
        self.outgoing_prepares.lock().remove(transfer_id);
    }

    fn prune_outgoing_prepares(&self) {
        let now = now_millis();
        self.outgoing_prepares.lock().retain(|_, prepare| {
            now.saturating_sub(prepare.created_at_ms) <= OUTGOING_PREPARE_RETENTION_MS
        });
    }

    fn outgoing_prepare_snapshot(
        &self,
        transfer_id: &str,
    ) -> Option<(String, Vec<PreparedSource>)> {
        let prepares = self.outgoing_prepares.lock();
        let prepare = prepares.get(transfer_id)?;
        Some((prepare.target_node_id.clone(), prepare.sources.clone()))
    }

    fn outgoing_prepare_alive(&self, transfer_id: &str) -> bool {
        self.outgoing_prepares
            .lock()
            .get(transfer_id)
            .is_some_and(|prepare| prepare.state == OutgoingPrepareState::Preparing)
    }

    fn add_outgoing_prepare_bytes(&self, transfer_id: &str, bytes: u64) {
        if let Some(prepare) = self.outgoing_prepares.lock().get_mut(transfer_id) {
            prepare.hashed_bytes = prepare.hashed_bytes.saturating_add(bytes);
        }
    }

    fn add_outgoing_prepare_item(&self, transfer_id: &str) {
        if let Some(prepare) = self.outgoing_prepares.lock().get_mut(transfer_id) {
            prepare.items_ready = prepare.items_ready.saturating_add(1);
        }
    }

    fn fail_outgoing_prepare(&self, transfer_id: &str, message: String) {
        if let Some(prepare) = self.outgoing_prepares.lock().get_mut(transfer_id) {
            prepare.state = OutgoingPrepareState::Failed(message);
        }
    }

    async fn prepare_outgoing_paths(&self, transfer_id: &str) {
        let Some((target_node_id, sources)) = self.outgoing_prepare_snapshot(transfer_id) else {
            return;
        };
        let mut items = Vec::with_capacity(sources.len());
        for (index, source) in sources.iter().enumerate() {
            if !self.outgoing_prepare_alive(transfer_id) {
                return;
            }
            let digest = match self.hash_outgoing_source(transfer_id, source).await {
                Ok(Some(digest)) => digest,
                Ok(None) => return,
                Err(error) => {
                    self.fail_outgoing_prepare(transfer_id, error);
                    return;
                }
            };
            items.push(TransferItem {
                item_id: format!("{index}-{}", Uuid::new_v4()),
                name: source.file_name.clone(),
                relative_path: source.relative_path.clone(),
                size: source.size,
                sha256: digest,
                media_type: outgoing_media_type(&source.relative_path),
                directory: false,
            });
            self.add_outgoing_prepare_item(transfer_id);
        }
        let offer = TransferOffer {
            transfer_id: transfer_id.to_string(),
            source_node_id: self.node_id(),
            source_name: self.node_info().node_name,
            target_node_id: target_node_id.clone(),
            total_bytes: sources.iter().map(|source| source.size).sum(),
            items,
            created_at_ms: now_millis(),
        };
        if let Err(error) = crate::server::validate_outgoing_offer(&offer) {
            self.fail_outgoing_prepare(transfer_id, error);
            return;
        }
        let sources_by_item = offer
            .items
            .iter()
            .zip(sources.iter())
            .map(|(item, source)| {
                (
                    item.item_id.clone(),
                    OutgoingItemSource::Original {
                        path: source.path.clone(),
                        size: source.size,
                        modified_ms: source.modified_ms,
                    },
                )
            })
            .collect::<HashMap<_, _>>();
        if let Err(error) = self.install_outgoing_paths(offer.clone(), sources_by_item) {
            self.fail_outgoing_prepare(transfer_id, error);
            return;
        }
        let progress = self.add_outgoing_transfer(offer.clone());
        let source_roots = self.outgoing_prepare_source_roots(transfer_id);
        self.set_transfer_source_paths(transfer_id, source_roots);
        if progress.status == TransferStatus::Failed {
            let message = progress
                .error
                .clone()
                .unwrap_or_else(|| "无法创建传输".to_string());
            self.remove_outgoing_staging(transfer_id);
            self.fail_outgoing_prepare(transfer_id, message);
            return;
        }
        if !self
            .send_to_peer_with_retry(&target_node_id, WireMessage::TransferOffer { offer })
            .await
        {
            let message = "目标设备暂时无法接收传输请求".to_string();
            if let Some(failed) =
                self.fail_transfer(transfer_id, message.clone(), TransferStatus::Failed)
            {
                self.publish_event(WireMessage::TransferProgress { progress: failed });
            }
            self.remove_outgoing_staging(transfer_id);
            self.fail_outgoing_prepare(transfer_id, message);
            return;
        }
        self.outgoing_prepares.lock().remove(transfer_id);
    }

    /// 返回 `Ok(None)` 表示传输已被取消，调用方应静默退出。
    async fn hash_outgoing_source(
        &self,
        transfer_id: &str,
        source: &PreparedSource,
    ) -> Result<Option<String>, String> {
        let mut file = tokio::fs::File::open(&source.path)
            .await
            .map_err(|error| format!("无法读取 {}：{error}", source.relative_path))?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; OUTGOING_HASH_CHUNK_BYTES];
        let mut total = 0u64;
        loop {
            if !self.outgoing_prepare_alive(transfer_id) {
                return Ok(None);
            }
            let read = file
                .read(&mut buffer)
                .await
                .map_err(|error| format!("读取 {} 失败：{error}", source.relative_path))?;
            if read == 0 {
                break;
            }
            total = total.saturating_add(read as u64);
            hasher.update(&buffer[..read]);
            self.add_outgoing_prepare_bytes(transfer_id, read as u64);
        }
        if total != source.size {
            return Err(format!(
                "{} 在准备期间发生变化，请重新选择后再发送",
                source.relative_path
            ));
        }
        Ok(Some(format!("{:x}", hasher.finalize())))
    }

    fn install_outgoing_paths(
        &self,
        offer: TransferOffer,
        sources: HashMap<String, OutgoingItemSource>,
    ) -> Result<(), String> {
        let directory = self.outgoing_staging_directory(&offer.transfer_id);
        std::fs::create_dir_all(&directory)
            .map_err(|error| format!("无法创建暂存目录：{error}"))?;
        self.staged_outgoing.lock().insert(
            offer.transfer_id.clone(),
            StagedOutgoing {
                offer,
                directory,
                sources,
                created_at_ms: now_millis(),
            },
        );
        Ok(())
    }

    pub fn outgoing_item(&self, transfer_id: &str, item_id: &str) -> Option<TransferItem> {
        self.staged_outgoing
            .lock()
            .get(transfer_id)?
            .offer
            .items
            .iter()
            .find(|item| item.item_id == item_id)
            .cloned()
    }

    pub fn outgoing_staged_target(&self, transfer_id: &str, item_id: &str) -> Option<PathBuf> {
        let staged = self.staged_outgoing.lock();
        let record = staged.get(transfer_id)?;
        let index = record
            .offer
            .items
            .iter()
            .position(|item| item.item_id == item_id)?;
        Some(record.directory.join(format!("{index}.bin")))
    }

    pub fn is_outgoing_item_ready(&self, transfer_id: &str, item_id: &str) -> bool {
        self.staged_outgoing
            .lock()
            .get(transfer_id)
            .is_some_and(|record| record.sources.contains_key(item_id))
    }

    /// digest 是暂存上传实际写入内容的摘要，回填后随清单发给接收端。
    pub fn mark_outgoing_item_staged(&self, transfer_id: &str, item_id: &str, digest: String) {
        if let Some(record) = self.staged_outgoing.lock().get_mut(transfer_id) {
            if let Some(item) = record
                .offer
                .items
                .iter_mut()
                .find(|item| item.item_id == item_id)
            {
                item.sha256 = digest;
            }
            record
                .sources
                .insert(item_id.to_string(), OutgoingItemSource::Staged);
        }
    }

    pub fn outgoing_offer_if_ready(&self, transfer_id: &str) -> Result<TransferOffer, String> {
        let staged = self.staged_outgoing.lock();
        let record = staged
            .get(transfer_id)
            .ok_or_else(|| "暂存任务不存在".to_string())?;
        if record.sources.len() < record.offer.items.len() {
            return Err("源文件尚未全部就绪".to_string());
        }
        Ok(record.offer.clone())
    }

    pub fn outgoing_staging_counts(&self, transfer_id: &str) -> Option<(usize, usize)> {
        let staged = self.staged_outgoing.lock();
        let record = staged.get(transfer_id)?;
        Some((record.sources.len(), record.offer.items.len()))
    }

    fn staged_outgoing_snapshot(&self) -> Vec<StagedOutgoing> {
        self.staged_outgoing.lock().values().cloned().collect()
    }

    /// 取消或完成后删除暂存目录，避免发送端长期占用双份空间。
    pub fn remove_outgoing_staging(&self, transfer_id: &str) {
        self.outgoing_prepares.lock().remove(transfer_id);
        let removed = self.staged_outgoing.lock().remove(transfer_id);
        let Some(record) = removed else {
            return;
        };
        if let Err(error) = std::fs::remove_dir_all(&record.directory) {
            if error.kind() != std::io::ErrorKind::NotFound {
                eprintln!("清理发送暂存目录失败：{error}");
            }
        }
    }

    /// 出站上传由节点定时驱动：接收端确认后立即开始，不依赖页面定时器或窗口可见性。
    pub fn spawn_outgoing_upload_supervisor(self: &Arc<Self>) {
        let runtime = self.clone();
        tokio::spawn(async move {
            loop {
                if runtime.stopping.load(Ordering::SeqCst) {
                    break;
                }
                runtime.drive_outgoing_uploads();
                runtime.prune_outgoing_prepares();
                tokio::time::sleep(Duration::from_millis(OUTGOING_UPLOAD_IDLE_POLL_MS)).await;
            }
        });
    }

    fn drive_outgoing_uploads(self: &Arc<Self>) {
        for entry in self.staged_outgoing_snapshot() {
            let transfer_id = entry.offer.transfer_id.clone();
            let Some(record) = self.transfer(&transfer_id) else {
                // 复制式暂存在 /start 之前还没有传输记录，只清理长期无人推进的遗留条目。
                if now_millis().saturating_sub(entry.created_at_ms) > OUTGOING_STAGING_RETENTION_MS
                {
                    self.remove_outgoing_staging(&transfer_id);
                }
                continue;
            };
            match record.progress.status {
                TransferStatus::Completed
                | TransferStatus::Rejected
                | TransferStatus::Cancelled => {
                    self.remove_outgoing_staging(&transfer_id);
                }
                TransferStatus::Failed => {
                    if now_millis().saturating_sub(record.updated_at_ms)
                        > FAILED_TRANSFER_RETENTION_MS
                    {
                        self.remove_outgoing_staging(&transfer_id);
                    }
                }
                TransferStatus::Accepted | TransferStatus::Uploading => {
                    self.spawn_outgoing_upload(&transfer_id);
                }
                TransferStatus::Waiting | TransferStatus::Ready => {}
            }
        }
    }

    fn spawn_outgoing_upload(self: &Arc<Self>, transfer_id: &str) {
        if !self.outgoing_uploads.lock().insert(transfer_id.to_string()) {
            return;
        }
        let runtime = self.clone();
        let id = transfer_id.to_string();
        tokio::spawn(async move {
            if let Err(error) = runtime.upload_outgoing_transfer(&id).await {
                eprintln!("向对端发送文件失败：{error}");
            }
            runtime.outgoing_uploads.lock().remove(&id);
        });
    }

    async fn resolve_outgoing_target(&self, node_id: &str) -> Option<String> {
        for base in self.peer_http_bases(node_id) {
            let response = self
                .upload_http
                .get(format!("{base}/api/v1/info"))
                .timeout(Duration::from_secs(6))
                .send()
                .await;
            let Ok(response) = response else {
                continue;
            };
            if !response.status().is_success() {
                continue;
            }
            let Ok(info) = response.json::<NodeInfo>().await else {
                continue;
            };
            if info.node_id == node_id {
                return Some(base);
            }
        }
        None
    }

    async fn put_outgoing_item(
        &self,
        runtime: &Arc<NodeRuntime>,
        base: &str,
        transfer_id: &str,
        item: &TransferItem,
        path: &Path,
    ) -> Result<Option<TransferProgress>, (String, bool)> {
        let file = tokio::fs::File::open(path)
            .await
            .map_err(|error| (format!("读取源文件失败：{error}"), false))?;
        let url = format!(
            "{base}/api/v1/transfers/{}/items/{}",
            crate::server::encode_path_segment(transfer_id),
            crate::server::encode_path_segment(&item.item_id)
        );
        let runtime = runtime.clone();
        let transfer_id = transfer_id.to_string();
        let item_id = item.item_id.clone();
        let stream = ReaderStream::new(file).scan(
            (0u64, Instant::now()),
            move |(sent, last_report), chunk| {
                if let Ok(bytes) = &chunk {
                    *sent = sent.saturating_add(bytes.len() as u64);
                    if last_report.elapsed() >= TRANSFER_PROGRESS_INTERVAL {
                        if let Some(progress) =
                            runtime.report_transfer_progress(&transfer_id, &item_id, *sent)
                        {
                            runtime.publish_event(WireMessage::TransferProgress { progress });
                        }
                        *last_report = Instant::now();
                    }
                }
                std::future::ready(Some(chunk))
            },
        );
        let response = self
            .upload_http
            .put(url)
            .header(reqwest::header::CONTENT_TYPE, item.media_type.clone())
            .body(reqwest::Body::wrap_stream(stream))
            .send()
            .await;
        match response {
            // 接收端的响应就是该项的权威进度；解析失败时退回本地计数。
            Ok(response) if response.status().is_success() => {
                Ok(response.json::<TransferProgress>().await.ok())
            }
            Ok(response) => {
                let status = response.status().as_u16();
                let retryable = status >= 500 || matches!(status, 408 | 425 | 429);
                let message = response
                    .json::<serde_json::Value>()
                    .await
                    .ok()
                    .and_then(|value| {
                        value
                            .get("error")
                            .and_then(|error| error.as_str())
                            .map(str::to_string)
                    })
                    .unwrap_or_else(|| format!("接收端返回 {status}"));
                Err((message, retryable))
            }
            Err(error) => Err((format!("传输中断：{error}"), true)),
        }
    }

    async fn fail_outgoing_transfer(&self, transfer_id: &str, message: String) -> String {
        let Some(progress) =
            self.fail_transfer(transfer_id, message.clone(), TransferStatus::Failed)
        else {
            return message;
        };
        self.publish_event(WireMessage::TransferProgress {
            progress: progress.clone(),
        });
        if let Some((source_node_id, target_node_id)) = self.transfer_peer_ids(transfer_id) {
            let peer = if source_node_id == self.node_id() {
                target_node_id
            } else {
                source_node_id
            };
            if peer != self.node_id() {
                let _ = self
                    .send_to_peer_with_retry(
                        &peer,
                        WireMessage::TransferAction {
                            transfer_id: transfer_id.to_string(),
                            action: "fail".to_string(),
                            reason: Some(message.clone()),
                            retry_count: Some(progress.retry_count),
                        },
                    )
                    .await;
            }
        }
        message
    }

    pub(crate) async fn upload_outgoing_transfer(
        self: &Arc<Self>,
        transfer_id: &str,
    ) -> Result<(), String> {
        let Some(entry) = self
            .staged_outgoing_snapshot()
            .into_iter()
            .find(|item| item.offer.transfer_id == transfer_id)
        else {
            return Ok(());
        };
        let Some(base) = self
            .resolve_outgoing_target(&entry.offer.target_node_id)
            .await
        else {
            return Err(self
                .fail_outgoing_transfer(transfer_id, "无法连接接收端节点，文件尚未发送".to_string())
                .await);
        };
        if let Some(progress) = self.begin_transfer_upload(transfer_id) {
            self.publish_event(WireMessage::TransferProgress { progress });
        }
        for (index, item) in entry.offer.items.iter().enumerate() {
            if !self.transfer_upload_active(transfer_id) {
                return Ok(());
            }
            let (path, staged) = match entry.sources.get(&item.item_id) {
                Some(OutgoingItemSource::Staged) => {
                    (entry.directory.join(format!("{index}.bin")), true)
                }
                Some(OutgoingItemSource::Original {
                    path,
                    size,
                    modified_ms,
                }) => {
                    let metadata = match tokio::fs::metadata(path).await {
                        Ok(metadata) => metadata,
                        Err(error) => {
                            let message = format!("{}：无法读取源文件（{error}）", item.name);
                            return Err(self.fail_outgoing_transfer(transfer_id, message).await);
                        }
                    };
                    // 源文件在准备之后被修改或替换时明确失败，避免把不一致的字节发出去。
                    if metadata.len() != *size || file_modified_ms(&metadata) != *modified_ms {
                        let message = format!("{}：源文件已变化，请重新发送", item.name);
                        return Err(self.fail_outgoing_transfer(transfer_id, message).await);
                    }
                    (path.clone(), false)
                }
                None => {
                    let message = format!("{}：源文件尚未就绪", item.name);
                    return Err(self.fail_outgoing_transfer(transfer_id, message).await);
                }
            };
            let mut attempt = 0;
            let reported = loop {
                attempt += 1;
                match self
                    .put_outgoing_item(self, &base, transfer_id, item, &path)
                    .await
                {
                    Ok(progress) => break progress,
                    Err((message, retryable)) => {
                        if !retryable || attempt >= OUTGOING_ITEM_UPLOAD_ATTEMPTS {
                            return Err(self
                                .fail_outgoing_transfer(
                                    transfer_id,
                                    format!("{}：{message}", item.name),
                                )
                                .await);
                        }
                        let backoff = 400u64.saturating_mul(1u64 << (attempt as u32 - 1));
                        tokio::time::sleep(Duration::from_millis(backoff.min(4_000))).await;
                    }
                }
            };
            match reported {
                // 以接收端返回的进度为准，避免本地计数与对端进度叠加。
                Some(progress) => {
                    self.acknowledge_outgoing_item(
                        transfer_id,
                        &item.item_id,
                        progress.completed_bytes,
                    );
                    self.update_transfer(transfer_id, progress);
                }
                None => {
                    if let Some(progress) =
                        self.mark_outgoing_item_uploaded(transfer_id, &item.item_id, item.size)
                    {
                        self.publish_event(WireMessage::TransferProgress { progress });
                    }
                }
            }
            if staged {
                if let Err(error) = tokio::fs::remove_file(&path).await {
                    if error.kind() != std::io::ErrorKind::NotFound {
                        eprintln!("清理暂存文件失败：{error}");
                    }
                }
            }
        }
        if self
            .transfer(transfer_id)
            .is_some_and(|record| record.progress.status == TransferStatus::Completed)
        {
            self.remove_outgoing_staging(transfer_id);
        }
        Ok(())
    }

    /// 接收端确认条目后记录已提交基线。
    ///
    /// `upload_outgoing_transfer` 在收到条目的成功响应时调用；把条目记入
    /// `received_items` 后，重传同一已提交条目不会再叠加在途进度。
    fn acknowledge_outgoing_item(&self, transfer_id: &str, item_id: &str, completed_bytes: u64) {
        let mut transfers = self.transfers.write();
        let Some(record) = transfers.get_mut(transfer_id) else {
            return;
        };
        record.received_items.insert(item_id.to_string());
        record.committed_bytes = record
            .committed_bytes
            .max(completed_bytes.min(record.offer.total_bytes));
        record.upload_in_flight = None;
    }

    pub fn mark_outgoing_item_uploaded(
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
        if !record
            .offer
            .items
            .iter()
            .any(|item| item.item_id == item_id)
        {
            return None;
        }
        if record.received_items.insert(item_id.to_string()) {
            record.committed_bytes = record.committed_bytes.saturating_add(bytes);
            record.progress.completed_items = record.progress.completed_items.saturating_add(1);
        }
        record.upload_in_flight = None;
        record.updated_at_ms = now_millis();
        record.progress.offer = Some(record.offer.clone());
        record.progress.status = if record.progress.completed_items >= record.progress.total_items {
            TransferStatus::Completed
        } else {
            TransferStatus::Uploading
        };
        record.progress.completed_bytes = record
            .progress
            .completed_bytes
            .max(record.committed_bytes.min(record.offer.total_bytes));
        let progress = record.progress.clone();
        let snapshot = record.clone();
        drop(transfers);
        if progress.status == TransferStatus::Completed {
            self.record_terminal_transfer(&snapshot);
        }
        Some(progress)
    }

    pub fn update_transfer(&self, transfer_id: &str, progress: TransferProgress) {
        let _update_gate = self.transfer_update_gate.lock();
        if is_active_transfer(&progress.status) && self.update_installing.load(Ordering::SeqCst) {
            return;
        }
        let snapshot = {
            let mut transfers = self.transfers.write();
            transfers.get_mut(transfer_id).map(|record| {
                let was_terminal = is_terminal_transfer(&record.progress.status);
                if record.progress.direction.as_deref() == Some("send") {
                    record.committed_bytes = record
                        .committed_bytes
                        .max(progress.completed_bytes.min(record.offer.total_bytes));
                    record.upload_in_flight = None;
                }
                let mut next = progress.clone();
                next.offer = Some(record.offer.clone());
                next.direction = record.progress.direction.clone();
                record.progress = next;
                record.updated_at_ms = now_millis();
                (record.clone(), was_terminal)
            })
        };
        let Some((record, was_terminal)) = snapshot else {
            return;
        };
        if is_terminal_transfer(&record.progress.status) {
            // 对端重复发送终态进度时不重复写入历史，也不推迟结束时间。
            if !was_terminal {
                self.record_terminal_transfer(&record);
            }
        } else {
            // 对端把失败任务重新打开后，本机不再保留旧的失败历史。
            self.remove_transfer_history_entry(transfer_id);
        }
        self.publish_event(WireMessage::TransferProgress {
            progress: record.progress,
        });
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
        record.progress.destination = Some(crate::fsutil::display_absolute_path(&destination));
        record.progress.status = TransferStatus::Accepted;
        record.progress.offer = Some(record.offer.clone());
        Some(record.progress.clone())
    }

    /// digest 是已提交文件内容的摘要，回填到清单后下载校验以它为准。
    pub fn mark_item_received(
        &self,
        transfer_id: &str,
        item_id: &str,
        bytes: u64,
        digest: String,
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
            .iter_mut()
            .find(|item| item.item_id == item_id)?;
        if item.size != bytes {
            return None;
        }
        item.sha256 = digest;
        if record.received_items.insert(item_id.to_string()) {
            record.committed_bytes = record.committed_bytes.saturating_add(bytes);
            record.progress.completed_items = record.progress.completed_items.saturating_add(1);
        }
        record.upload_in_flight = None;
        record.updated_at_ms = now_millis();
        if record.progress.completed_items >= record.progress.total_items {
            record.progress.status = TransferStatus::Completed;
        } else {
            record.progress.status = TransferStatus::Uploading;
        }
        record.progress.completed_bytes = record
            .progress
            .completed_bytes
            .max(record.committed_bytes.min(record.offer.total_bytes));
        record.progress.offer = Some(record.offer.clone());
        let progress = record.progress.clone();
        let snapshot = record.clone();
        drop(transfers);
        if progress.status == TransferStatus::Completed {
            self.record_terminal_transfer(&snapshot);
        }
        Some(progress)
    }

    /// 上传条目已提交时返回当前进度。
    ///
    /// 发送方可能在接收方已提交文件后丢失 HTTP 响应，然后重试同一个 PUT。
    /// 把该检查放在运行时里，
    /// 使重试可被观察到，
    /// 同时不会第二次累加传输进度。
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
        record.upload_in_flight = None;
        record.updated_at_ms = now_millis();
        record.progress.offer = Some(record.offer.clone());
        let progress = record.progress.clone();
        let snapshot = record.clone();
        drop(transfers);
        self.record_terminal_transfer(&snapshot);
        Some(progress)
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
        record.upload_in_flight = None;
        record.updated_at_ms = now_millis();
        record.progress.offer = Some(record.offer.clone());
        let progress = record.progress.clone();
        drop(transfers);
        // 任务被重新打开后不应继续以「已结束」出现在历史里。
        self.remove_transfer_history_entry(transfer_id);
        Ok(progress)
    }

    /// 删除失败的传输记录。未失败的记录会保留并拒绝删除，
    /// 使活动任务不会被直接丢弃而不先取消。
    pub fn remove_failed_transfer(&self, transfer_id: &str) -> Option<TransferProgress> {
        let mut transfers = self.transfers.write();
        if transfers.get(transfer_id)?.progress.status != TransferStatus::Failed {
            return None;
        }
        transfers.remove(transfer_id).map(|record| record.progress)
    }

    pub fn set_transfer_source_paths(&self, transfer_id: &str, paths: Vec<String>) {
        if let Some(record) = self.transfers.write().get_mut(transfer_id) {
            record.source_paths = paths;
        }
    }

    fn history_entry(&self, record: &TransferRecord) -> TransferHistoryEntry {
        let receive = match record.progress.direction.as_deref() {
            Some("receive") => true,
            Some(_) => false,
            None => record.offer.target_node_id == self.node_id(),
        };
        let (direction, peer_node_id, peer_name) = if receive {
            (
                "receive".to_string(),
                record.offer.source_node_id.clone(),
                record
                    .peer_name
                    .clone()
                    .unwrap_or_else(|| record.offer.source_name.clone()),
            )
        } else {
            (
                "send".to_string(),
                record.offer.target_node_id.clone(),
                record
                    .peer_name
                    .clone()
                    .unwrap_or_else(|| short_node_id(&record.offer.target_node_id)),
            )
        };
        TransferHistoryEntry {
            transfer_id: record.progress.transfer_id.clone(),
            direction,
            peer_node_id,
            peer_name,
            status: record.progress.status.clone(),
            completed_bytes: record.progress.completed_bytes,
            total_bytes: record.progress.total_bytes,
            completed_items: record.progress.completed_items,
            total_items: record.progress.total_items,
            source_paths: record.source_paths.clone(),
            destination: record.progress.destination.clone(),
            error: record.progress.error.clone(),
            retry_count: record.progress.retry_count,
            session_id: None,
            finished_at_ms: now_millis(),
        }
    }

    fn mobile_history_entry(&self, record: &MobileTransferRecord) -> TransferHistoryEntry {
        let peer_name = self
            .mobile_sessions
            .read()
            .get(&record.session_id)
            .map(|session| session.label.clone())
            .filter(|label| !label.trim().is_empty())
            .unwrap_or_else(|| "网页会话".to_string());
        TransferHistoryEntry {
            transfer_id: record.progress.transfer_id.clone(),
            direction: "send".to_string(),
            peer_node_id: record.session_id.clone(),
            peer_name,
            status: record.progress.status.clone(),
            completed_bytes: record.progress.completed_bytes,
            total_bytes: record.progress.total_bytes,
            completed_items: record.progress.completed_items,
            total_items: record.progress.total_items,
            source_paths: Vec::new(),
            destination: None,
            error: record.progress.error.clone(),
            retry_count: record.progress.retry_count,
            session_id: Some(record.session_id.clone()),
            finished_at_ms: now_millis(),
        }
    }

    fn push_transfer_history(&self, entries: Vec<TransferHistoryEntry>) {
        if entries.is_empty() {
            return;
        }
        let mut store = self.transfer_history.lock();
        for entry in entries {
            store
                .entries
                .retain(|item| item.transfer_id != entry.transfer_id);
            store.entries.insert(0, entry);
        }
        store.entries.truncate(TRANSFER_HISTORY_LIMIT);
        if let Err(error) = persist_transfer_history(&self.data_dir, &store) {
            eprintln!("保存传输历史失败：{error}");
        }
    }

    fn record_terminal_transfer(&self, record: &TransferRecord) {
        if is_terminal_transfer(&record.progress.status) {
            self.push_transfer_history(vec![self.history_entry(record)]);
        }
    }

    fn record_terminal_mobile_transfer(&self, record: &MobileTransferRecord) {
        if is_terminal_transfer(&record.progress.status) {
            self.push_transfer_history(vec![self.mobile_history_entry(record)]);
        }
    }

    fn remove_transfer_history_entry(&self, transfer_id: &str) {
        let mut store = self.transfer_history.lock();
        let before = store.entries.len();
        store
            .entries
            .retain(|entry| entry.transfer_id != transfer_id);
        if store.entries.len() == before {
            return;
        }
        if let Err(error) = persist_transfer_history(&self.data_dir, &store) {
            eprintln!("保存传输历史失败：{error}");
        }
    }

    pub fn transfer_history_snapshot(&self, session_id: Option<&str>) -> Vec<TransferHistoryEntry> {
        self.transfer_history
            .lock()
            .entries
            .iter()
            .filter(|entry| {
                session_id.is_none_or(|value| entry.session_id.as_deref() == Some(value))
            })
            .cloned()
            .collect()
    }

    /// 清空历史并返回需要通知对端的删除分组。持久化失败时恢复原记录。
    pub fn clear_transfer_history(
        &self,
        session_id: Option<&str>,
    ) -> anyhow::Result<TransferHistoryCleared> {
        let self_id = self.node_id();
        let mut store = self.transfer_history.lock();
        let previous = store.clone();
        let mut batches = HashMap::<String, Vec<String>>::new();
        let mut cleared = 0usize;
        store.entries.retain(|entry| {
            let matched = session_id.is_none_or(|value| entry.session_id.as_deref() == Some(value));
            if !matched {
                return true;
            }
            cleared += 1;
            if entry.session_id.is_none() && entry.peer_node_id != self_id {
                batches
                    .entry(entry.peer_node_id.clone())
                    .or_default()
                    .push(entry.transfer_id.clone());
            }
            false
        });
        if cleared == 0 {
            return Ok(TransferHistoryCleared {
                cleared,
                peer_batches: Vec::new(),
            });
        }
        if let Err(error) = persist_transfer_history(&self.data_dir, &store) {
            *store = previous;
            return Err(error);
        }
        Ok(TransferHistoryCleared {
            cleared,
            peer_batches: batches.into_iter().collect(),
        })
    }

    /// 只删除与发送方配对的记录，网页会话记录不参与同步删除。
    pub fn forget_transfer_history_from_peer(
        &self,
        peer_id: &str,
        transfer_ids: &[String],
    ) -> usize {
        if transfer_ids.is_empty() {
            return 0;
        }
        let mut store = self.transfer_history.lock();
        let previous = store.clone();
        let before = store.entries.len();
        store.entries.retain(|entry| {
            !(entry.session_id.is_none()
                && entry.peer_node_id == peer_id
                && transfer_ids.contains(&entry.transfer_id))
        });
        let removed = before.saturating_sub(store.entries.len());
        if removed == 0 {
            return 0;
        }
        if let Err(error) = persist_transfer_history(&self.data_dir, &store) {
            *store = previous;
            eprintln!("保存传输历史同步删除失败：{error}");
            return 0;
        }
        removed
    }

    /// 返回 Some(true) 表示对端支持，Some(false) 表示已知不支持，None 表示不在线或尚未完成握手。
    fn history_delete_support(&self, peer_id: &str) -> Option<bool> {
        self.peers.read().get(peer_id).and_then(|peer| {
            if peer.capabilities.is_empty() {
                // 刚建立连接、还没收到 Hello，或旧到没有能力列表：不能当成已知不支持。
                return None;
            }
            Some(
                peer.capabilities
                    .iter()
                    .any(|item| item == TRANSFER_HISTORY_CAPABILITY),
            )
        })
    }

    pub fn queue_history_deletions(&self, peer_id: &str, ids: Vec<String>) {
        if ids.is_empty() || peer_id.is_empty() || peer_id.len() > 256 || peer_id == self.node_id()
        {
            return;
        }
        let support = self.history_delete_support(peer_id);
        // 旧节点无法反序列化新消息，能力缺失时既不发送也不排队。
        if support == Some(false) {
            return;
        }
        // 离线或还没收到 Hello 时只排队，避免把新消息发给尚未确认能力的旧节点。
        if support == Some(true)
            && self.send_to_peer(
                peer_id,
                WireMessage::TransferHistoryDeleted {
                    transfer_ids: ids.clone(),
                },
            )
        {
            return;
        }
        let mut store = self.transfer_history.lock();
        let queue = store
            .pending_deletions
            .entry(peer_id.to_string())
            .or_default();
        for id in ids {
            if !queue.contains(&id) {
                queue.push(id);
            }
        }
        if queue.len() > MAX_PENDING_HISTORY_DELETIONS_PER_PEER {
            let dropped = queue.len() - MAX_PENDING_HISTORY_DELETIONS_PER_PEER;
            queue.drain(0..dropped);
            eprintln!("传输历史待补发删除超过上限，已丢弃 {dropped} 条");
        }
        if let Err(error) = persist_transfer_history(&self.data_dir, &store) {
            eprintln!("保存待补发传输历史删除失败：{error}");
        }
    }

    pub fn flush_history_deletions(&self, peer_id: &str) -> bool {
        let ids = match self.transfer_history.lock().pending_deletions.get(peer_id) {
            Some(ids) if !ids.is_empty() => ids.clone(),
            _ => return false,
        };
        match self.history_delete_support(peer_id) {
            Some(true) => {}
            Some(false) => {
                let mut store = self.transfer_history.lock();
                if store.pending_deletions.remove(peer_id).is_some() {
                    if let Err(error) = persist_transfer_history(&self.data_dir, &store) {
                        eprintln!("保存传输历史失败：{error}");
                    }
                }
                return false;
            }
            None => return false,
        }
        let batch = ids
            .iter()
            .take(MAX_HISTORY_DELETE_BATCH)
            .cloned()
            .collect::<Vec<_>>();
        if !self.send_to_peer(
            peer_id,
            WireMessage::TransferHistoryDeleted {
                transfer_ids: batch.clone(),
            },
        ) {
            return false;
        }
        let mut store = self.transfer_history.lock();
        if let Some(queue) = store.pending_deletions.get_mut(peer_id) {
            queue.retain(|id| !batch.contains(id));
        }
        if store
            .pending_deletions
            .get(peer_id)
            .is_some_and(|queue| queue.is_empty())
        {
            store.pending_deletions.remove(peer_id);
        }
        if let Err(error) = persist_transfer_history(&self.data_dir, &store) {
            eprintln!("保存传输历史失败：{error}");
        }
        true
    }

    pub fn transfers_snapshot(&self) -> Vec<TransferProgress> {
        self.prune_transfers();
        let mut transfers = self
            .transfers
            .read()
            .values()
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
            if terminal(&record.progress.status) {
                continue;
            }
            // 已接受却没有开始上传说明发送端没有接上，用更短的窗口暴露问题。
            let accepted = record.progress.status == TransferStatus::Accepted;
            let idle_timeout = if accepted {
                TRANSFER_ACCEPTED_IDLE_TIMEOUT_MS
            } else {
                TRANSFER_ACTIVITY_TIMEOUT_MS
            };
            if now.saturating_sub(record.updated_at_ms) > idle_timeout {
                record.progress.status = TransferStatus::Failed;
                record.progress.error = Some(
                    if accepted {
                        "接收端已确认，但发送端没有开始上传，已自动失败"
                    } else {
                        "传输长时间没有进展，已自动失败"
                    }
                    .to_string(),
                );
                record.progress.offer = Some(record.offer.clone());
                record.updated_at_ms = now;
                stale.push((
                    record.progress.clone(),
                    record.offer.clone(),
                    record.clone(),
                ));
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
        for (progress, offer, snapshot) in stale {
            self.record_terminal_transfer(&snapshot);
            // 自动失败要让对端也离开活动状态，否则另一端只能等自己的同一套超时。
            let peer = if offer.target_node_id == self.node_id() {
                offer.source_node_id.clone()
            } else {
                offer.target_node_id.clone()
            };
            if peer != self.node_id() {
                let _ = self.send_to_peer(
                    &peer,
                    WireMessage::TransferAction {
                        transfer_id: progress.transfer_id.clone(),
                        action: "fail".to_string(),
                        reason: progress.error.clone(),
                        retry_count: Some(progress.retry_count),
                    },
                );
            }
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

    /// 在条目提交前记录当前已接收字节并生成对外进度。
    ///
    /// `completed_bytes` 取已提交字节与在途字节的高水位，保证进度只增不减
    /// 且不超过总量；`completed_items` 仍只统计已提交条目。
    pub fn report_transfer_progress(
        &self,
        transfer_id: &str,
        item_id: &str,
        received_bytes: u64,
    ) -> Option<TransferProgress> {
        let mut transfers = self.transfers.write();
        let record = transfers.get_mut(transfer_id)?;
        if !matches!(
            record.progress.status,
            TransferStatus::Accepted | TransferStatus::Uploading
        ) || record.received_items.contains(item_id)
        {
            return None;
        }
        let item_size = record
            .offer
            .items
            .iter()
            .find(|item| item.item_id == item_id)?
            .size;
        let received = received_bytes.min(item_size);
        let received = match &record.upload_in_flight {
            Some((current, bytes)) if current == item_id => (*bytes).max(received),
            _ => received,
        };
        record.upload_in_flight = Some((item_id.to_string(), received));
        let displayed = record
            .committed_bytes
            .saturating_add(received)
            .min(record.offer.total_bytes);
        record.progress.completed_bytes = record.progress.completed_bytes.max(displayed);
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
        // `transfer_id` 由对端提供，也会出现在 HTTP API 中。
        // 它只作为协议键使用，绝不作为路径组件：
        // 新生成的 UUID 给每个传输一个不透明的暂存目录，
        // 不存在路径穿越或与根目录重名的风险。
        let staged_directory = self.mobile_staging_root().join(Uuid::new_v4().to_string());
        let record = MobileTransferRecord {
            session_id: session_id.to_string(),
            offer,
            progress,
            staged_directory,
            staged_items: HashSet::new(),
            downloaded_items: HashSet::new(),
            retryable_downloads: HashSet::new(),
            manifest_bytes,
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
        let mut failed_snapshots = transfers
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
                record.clone()
            })
            .collect::<Vec<_>>();
        let mut cleanup = failed_snapshots
            .iter()
            .map(|record| record.staged_directory.clone())
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
        let failed_remove_count = failed_ids.len().saturating_sub(MAX_MOBILE_FAILED_TRANSFERS);
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
                    released_manifest_bytes.saturating_add(record.manifest_bytes);
            }
        }
        if released_manifest_bytes > 0 {
            self.mobile_manifest_bytes
                .fetch_sub(released_manifest_bytes, Ordering::SeqCst);
        }
        cleanup.extend(remove.into_iter().map(|(_, directory)| directory));
        drop(transfers);
        drop(sessions);
        for record in failed_snapshots.drain(..) {
            self.record_terminal_mobile_transfer(&record);
        }
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

    /// 删除失败的移动端传输记录，并释放其清单预算。
    pub fn remove_failed_mobile_transfer(&self, transfer_id: &str) -> Option<MobileTransferRecord> {
        let mut transfers = self.mobile_transfers.write();
        if transfers.get(transfer_id)?.progress.status != TransferStatus::Failed {
            return None;
        }
        let record = transfers.remove(transfer_id)?;
        if record.manifest_bytes > 0 {
            self.mobile_manifest_bytes
                .fetch_sub(record.manifest_bytes, Ordering::SeqCst);
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
        let view = mobile_view(record);
        let snapshot = record.clone();
        drop(transfers);
        if status == TransferStatus::Rejected {
            self.record_terminal_mobile_transfer(&snapshot);
        }
        Some(view)
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
        let view = mobile_view(record);
        let snapshot = record.clone();
        drop(transfers);
        self.record_terminal_mobile_transfer(&snapshot);
        Some(view)
    }

    pub fn stage_mobile_item(
        &self,
        transfer_id: &str,
        item_id: &str,
        bytes: u64,
        digest: String,
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
            .iter_mut()
            .find(|item| item.item_id == item_id)?;
        if item.size != bytes {
            return None;
        }
        item.sha256 = digest;
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
        record.progress.offer = Some(record.offer.clone());
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
        let was_completed = record.progress.status == TransferStatus::Completed;
        if record.downloaded_items.len() >= record.progress.total_items {
            record.progress.status = TransferStatus::Completed;
        }
        let view = mobile_view(record);
        let snapshot = record.clone();
        drop(transfers);
        if view.progress.status == TransferStatus::Completed && !was_completed {
            self.record_terminal_mobile_transfer(&snapshot);
        }
        Some(view)
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
        let view = mobile_view(record);
        let snapshot = record.clone();
        drop(transfers);
        self.record_terminal_mobile_transfer(&snapshot);
        Some(view)
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

#[cfg(test)]
mod tests {
    use super::{
        bind_addresses, bindable_addresses, default_node_name, interface_rank,
        is_cellular_interface, is_placeholder_node_name, load_or_create_config,
        load_or_create_config_with_hostname, mobile_ca_url, network_addresses,
        network_addresses_for_bound, parse_update_proxy,
        prune_clipboard_history_records_with_limit, sync_node_name, valid_history_delete_ids,
        NodeProfile, NodeRuntime, UpdateFrequency, CLIPBOARD_HISTORY_FILE, CLIPBOARD_PAYLOAD_DIR,
        DESKTOP_TEMP_REGISTRY_FILE, MAX_MOBILE_TRANSFER_RECORDS, TRANSFER_ACCEPTED_IDLE_TIMEOUT_MS,
        TRANSFER_ACTIVITY_TIMEOUT_MS, TRANSFER_HISTORY_FILE, TRANSFER_HISTORY_LIMIT,
    };
    use crate::model::{
        AddressInfo, ClipboardEvent, ClipboardHistoryMode, DiskMetrics, GpuMemoryKind, GpuMetrics,
        HardwareMetrics, Hlc, LoadAverage, NodeInfo, PeerInfo, ReceiveMode, TransferItem,
        TransferOffer, TransferProgress, TransferStatus, WireMessage, CLIPBOARD_VIEW_CAPABILITY,
        MAX_HISTORY_DELETE_BATCH, MAX_MOBILE_SESSIONS, MAX_TRANSFER_RECORDS,
        METRICS_GPU_VIEW_CAPABILITY, METRICS_NETWORK_CAPABILITY, METRICS_SYSTEM_CAPABILITY,
        METRICS_SYSTEM_VIEW_CAPABILITY, TRANSFER_HISTORY_CAPABILITY,
    };
    use sha2::{Digest, Sha256};
    use std::collections::HashMap;
    use std::net::{IpAddr, Ipv4Addr};
    use std::path::PathBuf;
    use std::sync::Arc;
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
    fn android_skips_vpn_interfaces_but_keeps_tailscale() {
        let interfaces = vec![
            (
                "tun0".to_string(),
                "10.8.0.2".parse::<IpAddr>().expect("VPN address"),
            ),
            (
                "tailscale0".to_string(),
                "100.64.0.2".parse::<IpAddr>().expect("Tailscale address"),
            ),
        ];
        let android = bindable_addresses(NodeProfile::Android, interfaces);
        assert!(!android.contains(&Ipv4Addr::new(10, 8, 0, 2)));
        assert!(android.contains(&Ipv4Addr::new(100, 64, 0, 2)));
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
    fn hostname_change_replaces_saved_node_name() {
        let path = temp_data_dir();
        std::fs::write(
            path.join("node.json"),
            r#"{"node_id":"node-a","node_name":"OLD-PC"}"#,
        )
        .expect("write named config");
        let config = load_or_create_config_with_hostname(
            &path,
            NodeProfile::Desktop,
            Some("NEW-PC".to_string()),
        )
        .expect("load config");
        assert_eq!(config.node_name, "NEW-PC");
        let persisted: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(path.join("node.json")).expect("read config file"),
        )
        .expect("parse config file");
        assert_eq!(persisted["node_name"], "NEW-PC");
        assert_eq!(persisted["node_id"], "node-a");
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn saved_node_name_survives_missing_hostname() {
        let path = temp_data_dir();
        std::fs::write(
            path.join("node.json"),
            r#"{"node_id":"node-a","node_name":"手动名称"}"#,
        )
        .expect("write named config");
        let config = load_or_create_config_with_hostname(&path, NodeProfile::Desktop, None)
            .expect("load config");
        assert_eq!(config.node_name, "手动名称");
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn placeholder_name_migration_uses_platform_default() {
        let mut name = "localhost".to_string();
        assert!(sync_node_name(
            &mut name,
            NodeProfile::Android,
            Some("localhost".to_string())
        ));
        assert_eq!(name, "移动设备");

        let mut kept = "Redmi-K70".to_string();
        assert!(!sync_node_name(
            &mut kept,
            NodeProfile::Android,
            Some("localhost".to_string())
        ));
        assert_eq!(kept, "Redmi-K70");

        let mut blank = String::new();
        assert!(sync_node_name(&mut blank, NodeProfile::Desktop, None));
        assert_eq!(blank, "MeshShare 节点");
    }

    #[test]
    fn long_hostname_is_truncated_on_char_boundary() {
        let mut ascii = "a".repeat(300);
        assert!(sync_node_name(
            &mut ascii,
            NodeProfile::Desktop,
            Some("b".repeat(300))
        ));
        assert_eq!(ascii.len(), 256);

        let long_name = format!("{}{}", "a".repeat(255), "中");
        let mut names = long_name.clone();
        assert!(sync_node_name(
            &mut names,
            NodeProfile::Desktop,
            Some(long_name)
        ));
        assert_eq!(names.len(), 255);
        assert!(names.is_char_boundary(names.len()));
    }

    #[test]
    fn placeholder_name_is_replaced_and_persisted() {
        let path = temp_data_dir();
        std::fs::write(
            path.join("node.json"),
            r#"{"node_id":"node-a","node_name":"localhost"}"#,
        )
        .expect("write placeholder config");
        let config = load_or_create_config_with_hostname(
            &path,
            NodeProfile::Android,
            Some("Redmi-K70".to_string()),
        )
        .expect("load config");
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
        let named = load_or_create_config_with_hostname(
            &path,
            NodeProfile::Android,
            Some("Redmi-K70".to_string()),
        )
        .expect("reload config");
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
        let default_path = runtime
            .receive_settings()
            .default_path
            .expect("default receive path");
        assert!(
            !default_path.starts_with(r"\\?\"),
            "接收设置不应展示扩展长度前缀：{default_path}"
        );
        assert_eq!(
            std::fs::canonicalize(&default_path).expect("canonical default path"),
            std::fs::canonicalize(&destination).expect("canonical destination")
        );
        let offer = mobile_offer(&runtime, "auto-receive", 1);
        assert!(runtime.add_transfer(offer));
        let progress = runtime
            .transfer("auto-receive")
            .expect("auto transfer")
            .progress;
        assert_eq!(progress.status, TransferStatus::Accepted);
        let saved = progress.destination.expect("auto destination");
        assert!(
            !saved.starts_with(r"\\?\"),
            "接收记录不应展示扩展长度前缀：{saved}"
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
    fn update_proxy_is_validated_and_persisted() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        assert_eq!(runtime.update_proxy(), None);
        let saved = runtime
            .set_update_proxy(Some("  http://127.0.0.1:7890  ".to_string()))
            .expect("save update proxy");
        assert_eq!(saved.as_deref(), Some("http://127.0.0.1:7890/"));
        let url = runtime.update_proxy_url().expect("parse update proxy");
        assert_eq!(url.host_str(), Some("127.0.0.1"));
        assert_eq!(url.port(), Some(7890));

        let restored = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("restore runtime");
        assert_eq!(
            restored.update_proxy().as_deref(),
            Some("http://127.0.0.1:7890/")
        );
        assert_eq!(
            restored
                .set_update_proxy(Some("   ".to_string()))
                .expect("clear update proxy"),
            None
        );
        assert!(restored.update_proxy_url().is_none());
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn update_proxy_rejects_unsupported_addresses() {
        for value in [
            "socks5://127.0.0.1:1080",
            "ftp://127.0.0.1:21",
            "127.0.0.1:7890",
            "http://",
        ] {
            assert!(parse_update_proxy(value).is_err(), "{value}");
        }
        let proxy = parse_update_proxy("https://proxy.example.com:8443").expect("https proxy");
        assert_eq!(proxy.host_str(), Some("proxy.example.com"));
        assert_eq!(proxy.port(), Some(8443));
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
            ..Default::default()
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
            cpu_model: Some("CPU".to_string()),
            memory_used_bytes: Some(10),
            memory_total_bytes: Some(20),
            memory_available_bytes: Some(10),
            swap_used_bytes: Some(1),
            swap_total_bytes: Some(2),
            load_average: Some(LoadAverage {
                one: 1.0,
                five: 0.8,
                fifteen: 0.6,
            }),
            uptime_seconds: Some(100),
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
            disks: vec![DiskMetrics {
                name: "System".to_string(),
                mount_point: "/".to_string(),
                file_system: "ext4".to_string(),
                total_bytes: 100,
                available_bytes: 40,
                removable: false,
            }],
            ..Default::default()
        });
        let message = rx.try_recv().expect("network peer receives metrics");
        match message {
            WireMessage::MetricsUpdate { metrics } => {
                assert_eq!(metrics.cpu_usage_percent, None);
                assert_eq!(metrics.cpu_model, None);
                assert_eq!(metrics.memory_used_bytes, None);
                assert_eq!(metrics.memory_available_bytes, None);
                assert_eq!(metrics.swap_total_bytes, None);
                assert_eq!(metrics.load_average, None);
                assert_eq!(metrics.uptime_seconds, None);
                assert!(metrics.disks.is_empty());
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
            ..Default::default()
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

    fn named_peer(node_id: &str, node_name: &str, capabilities: Vec<String>) -> PeerInfo {
        let mut peer = test_peer(node_id, capabilities);
        peer.node_name = node_name.to_string();
        peer
    }

    fn incoming_history_offer(
        runtime: &NodeRuntime,
        transfer_id: &str,
        source_node_id: &str,
    ) -> TransferOffer {
        TransferOffer {
            transfer_id: transfer_id.to_string(),
            source_node_id: source_node_id.to_string(),
            source_name: format!("{source_node_id}-name"),
            target_node_id: runtime.node_id(),
            items: vec![TransferItem {
                item_id: format!("{transfer_id}-item"),
                name: "file.bin".to_string(),
                relative_path: "file.bin".to_string(),
                size: 4,
                sha256: "0".repeat(64),
                media_type: "application/octet-stream".to_string(),
                directory: false,
            }],
            total_bytes: 4,
            created_at_ms: crate::clock::now_millis(),
        }
    }

    fn outgoing_history_offer(
        runtime: &NodeRuntime,
        transfer_id: &str,
        target_node_id: &str,
    ) -> TransferOffer {
        TransferOffer {
            transfer_id: transfer_id.to_string(),
            source_node_id: runtime.node_id(),
            source_name: "本机".to_string(),
            target_node_id: target_node_id.to_string(),
            items: vec![TransferItem {
                item_id: format!("{transfer_id}-item"),
                name: "file.bin".to_string(),
                relative_path: "file.bin".to_string(),
                size: 4,
                sha256: "0".repeat(64),
                media_type: "application/octet-stream".to_string(),
                directory: false,
            }],
            total_bytes: 4,
            created_at_ms: crate::clock::now_millis(),
        }
    }

    fn complete_incoming(runtime: &NodeRuntime, transfer_id: &str, source_node_id: &str) {
        runtime.add_transfer(incoming_history_offer(runtime, transfer_id, source_node_id));
        runtime
            .mark_item_received(
                transfer_id,
                &format!("{transfer_id}-item"),
                4,
                "0".repeat(64),
            )
            .expect("complete incoming transfer");
    }

    #[test]
    fn transfer_progress_reports_in_flight_bytes_without_double_counting() {
        let path = temp_data_dir();
        let destination = path.join("received");
        std::fs::create_dir_all(&destination).expect("create destination");
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let mut offer = incoming_history_offer(&runtime, "live-progress", "peer-sender");
        offer.items[0].size = 100;
        offer.total_bytes = 100;
        runtime.add_transfer(offer);
        runtime
            .set_transfer_acceptance("live-progress", destination)
            .expect("accept transfer");
        runtime
            .begin_transfer_upload("live-progress")
            .expect("begin upload");

        for (received, expected) in [(40u64, 40u64), (60, 60), (30, 60)] {
            let progress = runtime
                .report_transfer_progress("live-progress", "live-progress-item", received)
                .expect("report progress");
            assert_eq!(progress.completed_bytes, expected);
            assert_eq!(progress.completed_items, 0);
            assert_eq!(progress.status, TransferStatus::Uploading);
        }
        let capped = runtime
            .report_transfer_progress("live-progress", "live-progress-item", 999)
            .expect("report capped progress");
        assert_eq!(capped.completed_bytes, 100);

        let completed = runtime
            .mark_item_received("live-progress", "live-progress-item", 100, "0".repeat(64))
            .expect("complete transfer");
        assert_eq!(completed.completed_bytes, 100);
        assert_eq!(completed.completed_items, 1);
        assert_eq!(completed.status, TransferStatus::Completed);
        let history = runtime.transfer_history_snapshot(None);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].completed_bytes, 100);
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn transfer_progress_continues_from_committed_bytes_across_items() {
        let path = temp_data_dir();
        let destination = path.join("received");
        std::fs::create_dir_all(&destination).expect("create destination");
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let mut offer = incoming_history_offer(&runtime, "multi-live", "peer-sender");
        offer.items[0].item_id = "first".to_string();
        offer.items[0].size = 10;
        let mut second = offer.items[0].clone();
        second.item_id = "second".to_string();
        second.relative_path = "second.bin".to_string();
        second.size = 20;
        offer.items.push(second);
        offer.total_bytes = 30;
        runtime.add_transfer(offer);
        runtime
            .set_transfer_acceptance("multi-live", destination)
            .expect("accept transfer");
        runtime
            .begin_transfer_upload("multi-live")
            .expect("begin upload");

        assert_eq!(
            runtime
                .report_transfer_progress("multi-live", "first", 10)
                .expect("report first")
                .completed_bytes,
            10
        );
        assert_eq!(
            runtime
                .report_transfer_progress("multi-live", "first", 5)
                .expect("keep first high watermark")
                .completed_bytes,
            10
        );
        let after_first = runtime
            .mark_item_received("multi-live", "first", 10, "0".repeat(64))
            .expect("commit first");
        assert_eq!(after_first.completed_bytes, 10);
        assert_eq!(after_first.completed_items, 1);
        assert_eq!(after_first.status, TransferStatus::Uploading);
        assert_eq!(
            runtime
                .report_transfer_progress("multi-live", "second", 5)
                .expect("report second")
                .completed_bytes,
            15
        );
        let completed = runtime
            .mark_item_received("multi-live", "second", 20, "0".repeat(64))
            .expect("commit second");
        assert_eq!(completed.completed_bytes, 30);
        assert_eq!(completed.completed_items, 2);
        assert_eq!(completed.status, TransferStatus::Completed);
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn outgoing_progress_uses_receiver_ack_as_new_baseline() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let mut offer = outgoing_history_offer(&runtime, "send-live", "peer-target");
        offer.items[0].item_id = "first".to_string();
        offer.items[0].size = 10;
        let mut second = offer.items[0].clone();
        second.item_id = "second".to_string();
        second.relative_path = "second.bin".to_string();
        second.size = 20;
        offer.items.push(second);
        offer.total_bytes = 30;
        runtime.add_outgoing_transfer(offer);
        runtime
            .transfers
            .write()
            .get_mut("send-live")
            .expect("find outgoing transfer")
            .progress
            .status = TransferStatus::Accepted;
        runtime
            .begin_transfer_upload("send-live")
            .expect("begin upload");
        assert_eq!(
            runtime
                .report_transfer_progress("send-live", "first", 10)
                .expect("report first")
                .completed_bytes,
            10
        );
        let mut ack = runtime
            .transfer("send-live")
            .expect("find outgoing transfer")
            .progress;
        ack.completed_bytes = 10;
        ack.completed_items = 1;
        ack.status = TransferStatus::Uploading;
        runtime.acknowledge_outgoing_item("send-live", "first", 10);
        runtime.update_transfer("send-live", ack);
        let record = runtime
            .transfer("send-live")
            .expect("find outgoing transfer");
        assert_eq!(record.committed_bytes, 10);
        assert!(record.upload_in_flight.is_none());
        assert!(record.received_items.contains("first"));
        assert!(runtime
            .report_transfer_progress("send-live", "first", 5)
            .is_none());
        assert_eq!(
            runtime
                .report_transfer_progress("send-live", "second", 5)
                .expect("report second")
                .completed_bytes,
            15
        );
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn transfer_history_keeps_latest_ten_terminal_records() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        for index in 0..12 {
            complete_incoming(&runtime, &format!("history-{index}"), "peer-a");
        }
        let history = runtime.transfer_history_snapshot(None);
        assert_eq!(history.len(), TRANSFER_HISTORY_LIMIT);
        assert_eq!(history[0].transfer_id, "history-11");
        assert_eq!(history[9].transfer_id, "history-2");
        assert!(history
            .iter()
            .all(|entry| entry.status == TransferStatus::Completed));
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn transfer_history_survives_restart_and_ignores_corruption() {
        let path = temp_data_dir();
        {
            let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
                .expect("create runtime");
            complete_incoming(&runtime, "persisted", "peer-a");
        }
        {
            let restored = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
                .expect("restore runtime");
            let history = restored.transfer_history_snapshot(None);
            assert_eq!(history.len(), 1);
            assert_eq!(history[0].transfer_id, "persisted");
        }
        std::fs::write(path.join(TRANSFER_HISTORY_FILE), b"{broken")
            .expect("write corrupt history");
        let recovered = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("start with corrupt history");
        assert!(recovered.transfer_history_snapshot(None).is_empty());
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn transfer_history_captures_direction_peer_and_paths() {
        let path = temp_data_dir();
        let destination = path.join("received");
        std::fs::create_dir_all(&destination).expect("create destination");
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        runtime.set_peer(named_peer(
            "peer-target",
            "目标电脑",
            vec![TRANSFER_HISTORY_CAPABILITY.to_string()],
        ));
        runtime.add_transfer(incoming_history_offer(
            &runtime,
            "receive-one",
            "peer-sender",
        ));
        runtime
            .set_transfer_acceptance("receive-one", destination.clone())
            .expect("accept receive");
        runtime
            .mark_item_received("receive-one", "receive-one-item", 4, "0".repeat(64))
            .expect("complete receive");
        runtime.add_outgoing_transfer(outgoing_history_offer(&runtime, "send-one", "peer-target"));
        runtime.set_transfer_source_paths(
            "send-one",
            vec!["C:/资料".to_string(), "D:/照片".to_string()],
        );
        runtime
            .fail_transfer("send-one", "网络中断".to_string(), TransferStatus::Failed)
            .expect("fail send");
        let history = runtime.transfer_history_snapshot(None);
        let receive = history
            .iter()
            .find(|entry| entry.transfer_id == "receive-one")
            .expect("receive history");
        assert_eq!(receive.direction, "receive");
        assert_eq!(receive.peer_node_id, "peer-sender");
        assert_eq!(receive.peer_name, "peer-sender-name");
        assert_eq!(
            receive.destination,
            Some(destination.to_string_lossy().into_owned())
        );
        let send = history
            .iter()
            .find(|entry| entry.transfer_id == "send-one")
            .expect("send history");
        assert_eq!(send.direction, "send");
        assert_eq!(send.peer_node_id, "peer-target");
        assert_eq!(send.peer_name, "目标电脑");
        assert_eq!(
            send.source_paths,
            vec!["C:/资料".to_string(), "D:/照片".to_string()]
        );
        assert_eq!(send.error.as_deref(), Some("网络中断"));
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn retry_removes_failed_history_and_dismiss_keeps_it() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        runtime.add_transfer(incoming_history_offer(&runtime, "retry-one", "peer-a"));
        runtime
            .fail_transfer("retry-one", "失败".to_string(), TransferStatus::Failed)
            .expect("fail transfer");
        assert_eq!(runtime.transfer_history_snapshot(None).len(), 1);
        runtime
            .retry_transfer("retry-one", TransferStatus::Waiting, None)
            .expect("retry transfer");
        assert!(runtime.transfer_history_snapshot(None).is_empty());
        runtime
            .fail_transfer("retry-one", "再次失败".to_string(), TransferStatus::Failed)
            .expect("fail transfer again");
        let history = runtime.transfer_history_snapshot(None);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].retry_count, 1);
        runtime
            .remove_failed_transfer("retry-one")
            .expect("remove failed transfer");
        assert_eq!(runtime.transfer_history_snapshot(None).len(), 1);
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn peer_reopened_transfer_clears_local_failure_history() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        runtime.add_outgoing_transfer(outgoing_history_offer(&runtime, "peer-retry", "peer-b"));
        runtime
            .fail_transfer("peer-retry", "失败".to_string(), TransferStatus::Failed)
            .expect("fail transfer");
        assert_eq!(runtime.transfer_history_snapshot(None).len(), 1);
        let mut progress = runtime
            .transfer("peer-retry")
            .expect("transfer record")
            .progress;
        progress.status = TransferStatus::Waiting;
        progress.error = None;
        progress.retry_count = 1;
        runtime.update_transfer("peer-retry", progress);
        assert!(runtime.transfer_history_snapshot(None).is_empty());
        let _ = std::fs::remove_dir_all(path);
    }
    #[test]
    fn clear_transfer_history_is_persisted_and_reports_peer_batches() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        complete_incoming(&runtime, "from-a", "peer-a");
        complete_incoming(&runtime, "from-b", "peer-b");
        let cleared = runtime.clear_transfer_history(None).expect("clear history");
        assert_eq!(cleared.cleared, 2);
        assert!(runtime.transfer_history_snapshot(None).is_empty());
        let mut batches = cleared.peer_batches;
        batches.sort();
        assert_eq!(batches.len(), 2);
        assert!(batches
            .iter()
            .any(|(peer, ids)| peer == "peer-a" && ids == &vec!["from-a".to_string()]));
        let restored = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("restore runtime");
        assert!(restored.transfer_history_snapshot(None).is_empty());
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn mobile_transfer_history_is_scoped_to_session() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let first = runtime
            .create_mobile_session("手机 A".to_string())
            .expect("create first session");
        let second = runtime
            .create_mobile_session("手机 B".to_string())
            .expect("create second session");
        for (session_id, transfer_id) in [
            (&first.session_id, "mobile-a"),
            (&second.session_id, "mobile-b"),
        ] {
            let offer = mobile_offer(&runtime, transfer_id, 4);
            runtime
                .create_mobile_transfer(session_id, offer)
                .expect("create mobile transfer");
            runtime
                .set_mobile_transfer_status(transfer_id, session_id, TransferStatus::Accepted)
                .expect("accept mobile transfer");
            runtime
                .stage_mobile_item(
                    transfer_id,
                    &format!("{transfer_id}-item"),
                    4,
                    "hash".to_string(),
                )
                .expect("stage mobile item");
            runtime
                .mark_mobile_item_downloaded(transfer_id, &format!("{transfer_id}-item"), false)
                .expect("complete mobile download");
        }
        let all = runtime.transfer_history_snapshot(None);
        assert_eq!(all.len(), 2);
        assert!(all.iter().all(|entry| entry.session_id.is_some()));
        let scoped = runtime.transfer_history_snapshot(Some(&first.session_id));
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].peer_name, "手机 A");
        let cleared = runtime
            .clear_transfer_history(Some(&first.session_id))
            .expect("clear session history");
        assert_eq!(cleared.cleared, 1);
        assert!(cleared.peer_batches.is_empty());
        assert_eq!(runtime.transfer_history_snapshot(None).len(), 1);
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn history_deletions_queue_for_offline_peer_and_flush_on_reconnect() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        complete_incoming(&runtime, "offline-one", "peer-offline");
        let cleared = runtime.clear_transfer_history(None).expect("clear history");
        for (peer_id, ids) in cleared.peer_batches {
            runtime.queue_history_deletions(&peer_id, ids);
        }
        assert_eq!(
            runtime
                .transfer_history
                .lock()
                .pending_deletions
                .get("peer-offline")
                .cloned(),
            Some(vec!["offline-one".to_string()])
        );
        let restored = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("restore runtime");
        assert_eq!(
            restored
                .transfer_history
                .lock()
                .pending_deletions
                .get("peer-offline")
                .cloned(),
            Some(vec!["offline-one".to_string()])
        );
        let (tx, mut rx) = mpsc::channel(8);
        restored.set_peer(named_peer(
            "peer-offline",
            "离线电脑",
            vec![TRANSFER_HISTORY_CAPABILITY.to_string()],
        ));
        restored.add_peer_channel("peer-offline".to_string(), tx);
        assert!(restored.flush_history_deletions("peer-offline"));
        match rx.try_recv().expect("peer receives history deletion") {
            WireMessage::TransferHistoryDeleted { transfer_ids } => {
                assert_eq!(transfer_ids, vec!["offline-one".to_string()]);
            }
            _ => panic!("unexpected message"),
        }
        assert!(restored
            .transfer_history
            .lock()
            .pending_deletions
            .is_empty());
        // 旧节点声明了其他能力但没有 transfer.history.v1：既不能收新消息，也不应留下队列。
        restored.set_peer(named_peer(
            "peer-legacy",
            "旧电脑",
            vec!["transfer.file".to_string()],
        ));
        restored.queue_history_deletions("peer-legacy", vec!["legacy-one".to_string()]);
        assert!(!restored
            .transfer_history
            .lock()
            .pending_deletions
            .contains_key("peer-legacy"));
        // 能力尚未确认的对端只排队，不把新消息直接发给可能是旧版本的连接。
        let (tx, mut rx) = mpsc::channel(8);
        restored.set_peer(named_peer("peer-handshake", "握手中的电脑", Vec::new()));
        restored.add_peer_channel("peer-handshake".to_string(), tx);
        restored.queue_history_deletions("peer-handshake", vec!["handshake-one".to_string()]);
        assert!(rx.try_recv().is_err());
        assert_eq!(
            restored
                .transfer_history
                .lock()
                .pending_deletions
                .get("peer-handshake")
                .cloned(),
            Some(vec!["handshake-one".to_string()])
        );
        // 尚未收到 Hello 的对端不能按旧节点丢弃待删除记录。
        restored.set_peer(named_peer("peer-pending", "握手中的电脑", Vec::new()));
        restored.queue_history_deletions("peer-pending", vec!["pending-one".to_string()]);
        assert_eq!(
            restored
                .transfer_history
                .lock()
                .pending_deletions
                .get("peer-pending")
                .cloned(),
            Some(vec!["pending-one".to_string()])
        );
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn history_deletion_from_peer_only_removes_paired_entries() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        complete_incoming(&runtime, "from-a", "peer-a");
        complete_incoming(&runtime, "from-b", "peer-b");
        let session = runtime
            .create_mobile_session("网页".to_string())
            .expect("create session");
        let offer = mobile_offer(&runtime, "mobile-one", 4);
        runtime
            .create_mobile_transfer(&session.session_id, offer)
            .expect("create mobile transfer");
        runtime
            .set_mobile_transfer_status("mobile-one", &session.session_id, TransferStatus::Accepted)
            .expect("accept mobile transfer");
        runtime
            .stage_mobile_item("mobile-one", "mobile-one-item", 4, "hash".to_string())
            .expect("stage mobile item");
        runtime
            .mark_mobile_item_downloaded("mobile-one", "mobile-one-item", false)
            .expect("complete mobile download");
        assert_eq!(
            runtime.forget_transfer_history_from_peer("peer-a", &["from-a".to_string()]),
            1
        );
        let remaining = runtime.transfer_history_snapshot(None);
        assert_eq!(remaining.len(), 2);
        assert!(remaining.iter().any(|entry| entry.transfer_id == "from-b"));
        assert!(remaining
            .iter()
            .any(|entry| entry.transfer_id == "mobile-one"));
        assert_eq!(
            runtime.forget_transfer_history_from_peer(
                &session.session_id,
                &["mobile-one".to_string()]
            ),
            0
        );
        assert!(!valid_history_delete_ids(&[]));
        assert!(!valid_history_delete_ids(&[" ".to_string()]));
        assert!(valid_history_delete_ids(&["ok".to_string()]));
        let too_many = vec!["x".to_string(); MAX_HISTORY_DELETE_BATCH + 1];
        assert!(!valid_history_delete_ids(&too_many));
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
            ..Default::default()
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
                sha256: String::new(),
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
            .stage_mobile_item("mobile-transfer", "item", 3, "hash".to_string())
            .expect("stage item");
        assert_eq!(ready.progress.status, TransferStatus::Ready);
        let manifest = ready.progress.offer.as_ref().expect("staged manifest");
        assert_eq!(manifest.items[0].sha256, "hash");
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
            .stage_mobile_item("recent-mobile", "recent-mobile-item", 1, "hash".to_string())
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
            .transfers_snapshot()
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
    #[cfg(not(target_os = "android"))]
    fn system_clipboard_keeps_latest_pending_snapshot_and_discards_stale_reads() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let observed_event_id = runtime.clipboard_event_id();

        // 系统线程还在读取时，节点仍能接受后续剪贴板事件。
        runtime
            .create_local_clipboard_named(Some("first".to_string()), None, "web".to_string())
            .expect("accept first publication");
        let newest = runtime
            .create_local_clipboard_named(Some("newest".to_string()), None, "web".to_string())
            .expect("accept latest publication");
        assert!(runtime
            .create_local_clipboard(
                Some("stale read".to_string()),
                None,
                observed_event_id.as_deref()
            )
            .is_none());
        let pending = runtime
            .take_system_clipboard_write()
            .expect("pending write");
        assert_eq!(pending.event_id, newest.event_id);
        assert!(runtime.take_system_clipboard_write().is_none());

        let copied = runtime
            .create_local_clipboard(Some("fresh copy".to_string()), None, Some(&newest.event_id))
            .expect("publish fresh system clipboard read");
        assert_eq!(
            runtime.clipboard_event_id().as_deref(),
            Some(copied.event_id.as_str())
        );
        assert!(runtime.take_system_clipboard_write().is_none());
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
            .create_local_clipboard_named(None, Some(png), "local".to_string())
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
    fn clipboard_history_thumbnail_is_generated_and_cleaned_up() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let mut png = Vec::new();
        image::DynamicImage::new_rgba8(200, 100)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .expect("encode png");
        let event = runtime
            .create_local_clipboard_named(None, Some(png), "local".to_string())
            .expect("create image clipboard event");

        let thumbnail = runtime
            .clipboard_history_thumbnail(&event.event_id)
            .expect("thumbnail");
        let decoded = image::load_from_memory(&thumbnail).expect("decode thumbnail");
        assert_eq!((decoded.width(), decoded.height()), (96, 72));
        assert!(runtime
            .clipboard_history_thumbnail(&event.event_id)
            .is_some());

        let payload_dir = path.join(CLIPBOARD_PAYLOAD_DIR);
        assert!(std::fs::read_dir(&payload_dir)
            .expect("list payloads")
            .filter_map(Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().ends_with(".thumb.png")));

        runtime
            .create_local_clipboard_named(Some("text".to_string()), None, "local".to_string())
            .expect("create text clipboard event");
        let text_event_id = runtime.clipboard_history_list().items[0].event_id.clone();
        assert!(runtime
            .clipboard_history_thumbnail(&text_event_id)
            .is_none());
        assert!(runtime.clipboard_history_thumbnail("missing").is_none());

        runtime.clear_clipboard_history().expect("clear history");
        assert_eq!(
            std::fs::read_dir(&payload_dir)
                .expect("list payloads after clear")
                .count(),
            0
        );
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

    fn outgoing_test_item(name: &str, bytes: &[u8]) -> TransferItem {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        TransferItem {
            item_id: format!("item-{}", Uuid::new_v4()),
            name: name.to_string(),
            relative_path: name.to_string(),
            size: bytes.len() as u64,
            sha256: format!("{:x}", hasher.finalize()),
            media_type: "application/octet-stream".to_string(),
            directory: false,
        }
    }

    fn outgoing_test_offer(
        source_node_id: &str,
        target_node_id: &str,
        items: Vec<TransferItem>,
    ) -> TransferOffer {
        TransferOffer {
            transfer_id: Uuid::new_v4().to_string(),
            source_node_id: source_node_id.to_string(),
            source_name: "sender".to_string(),
            target_node_id: target_node_id.to_string(),
            total_bytes: items.iter().map(|item| item.size).sum(),
            items,
            created_at_ms: crate::clock::now_millis(),
        }
    }

    fn register_peer_endpoint(runtime: &Arc<NodeRuntime>, node: NodeInfo) {
        runtime.peers.write().insert(
            node.node_id.clone(),
            PeerInfo {
                node_id: node.node_id.clone(),
                node_name: node.node_name.clone(),
                platform: node.platform.clone(),
                addresses: node.addresses.clone(),
                mobile_addresses: node.mobile_addresses.clone(),
                capabilities: node.capabilities.clone(),
                online: true,
                last_seen_ms: crate::clock::now_millis(),
                endpoint: Some(format!("http://127.0.0.1:{}/api/v1/ws", node.port)),
                source: "websocket".to_string(),
                metrics: None,
                metrics_received_at_ms: None,
                metrics_stale: false,
            },
        );
    }

    #[tokio::test]
    async fn accepted_outgoing_transfer_streams_staged_items_without_page_state() {
        let receiver_dir =
            std::env::temp_dir().join(format!("meshshare-outgoing-receiver-{}", Uuid::new_v4()));
        let static_root = receiver_dir.join("web");
        std::fs::create_dir_all(&static_root).expect("create static directory");
        std::fs::write(static_root.join("index.html"), "<html></html>")
            .expect("write static index");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind receiver");
        let address = listener.local_addr().expect("receiver address");
        let receiver = NodeRuntime::new(
            receiver_dir.clone(),
            address.port(),
            env!("CARGO_PKG_VERSION").into(),
        )
        .expect("create receiver runtime");
        let app = crate::server::router(receiver.clone(), static_root);
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve receiver");
        });

        let sender_dir =
            std::env::temp_dir().join(format!("meshshare-outgoing-sender-{}", Uuid::new_v4()));
        let sender = NodeRuntime::new(sender_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create sender runtime");
        register_peer_endpoint(&sender, receiver.node_info());

        let payload = b"meshshare staged payload".to_vec();
        let item = outgoing_test_item("payload.bin", &payload);
        let offer = outgoing_test_offer(&sender.node_id(), &receiver.node_id(), vec![item.clone()]);
        sender
            .begin_outgoing_staging(offer.clone())
            .expect("begin staging");
        let staged = sender
            .outgoing_staged_target(&offer.transfer_id, &item.item_id)
            .expect("staged target");
        std::fs::create_dir_all(staged.parent().expect("staging directory"))
            .expect("create staging directory");
        std::fs::write(&staged, &payload).expect("write staged payload");
        sender.mark_outgoing_item_staged(&offer.transfer_id, &item.item_id, item.sha256.clone());

        receiver.add_transfer(offer.clone());
        let destination = receiver_dir.join("received");
        std::fs::create_dir_all(&destination).expect("create destination");
        receiver
            .set_transfer_acceptance(&offer.transfer_id, destination.clone())
            .expect("accept transfer");

        sender.add_outgoing_transfer(offer.clone());
        sender.mark_transfer_accepted(&offer.transfer_id);
        sender
            .upload_outgoing_transfer(&offer.transfer_id)
            .await
            .expect("upload staged transfer");

        assert_eq!(
            sender
                .transfer(&offer.transfer_id)
                .expect("sender record")
                .progress
                .status,
            TransferStatus::Completed
        );
        assert_eq!(
            receiver
                .transfer(&offer.transfer_id)
                .expect("receiver record")
                .progress
                .status,
            TransferStatus::Completed
        );
        assert_eq!(
            std::fs::read(destination.join("payload.bin")).expect("read received payload"),
            payload
        );
        assert!(sender.outgoing_staging_counts(&offer.transfer_id).is_none());

        sender
            .stopping
            .store(true, std::sync::atomic::Ordering::SeqCst);
        server.abort();
        let _ = server.await;
        let _ = std::fs::remove_dir_all(&sender_dir);
        let _ = std::fs::remove_dir_all(&receiver_dir);
    }

    #[tokio::test]
    async fn outgoing_staging_endpoints_verify_source_bytes() {
        let data_dir =
            std::env::temp_dir().join(format!("meshshare-outgoing-staging-{}", Uuid::new_v4()));
        let static_root = data_dir.join("web");
        std::fs::create_dir_all(&static_root).expect("create static directory");
        std::fs::write(static_root.join("index.html"), "<html></html>")
            .expect("write static index");
        let runtime = NodeRuntime::new(data_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let mut peer_node = runtime.node_info();
        peer_node.node_id = "staging-peer".to_string();
        peer_node.port = 9;
        register_peer_endpoint(&runtime, peer_node);

        let app = crate::server::router(runtime.clone(), static_root);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind staging server");
        let address = listener.local_addr().expect("staging address");
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .expect("serve staging router");
        });

        let item = outgoing_test_item("apk.bin", b"staged");
        let offer = outgoing_test_offer(&runtime.node_id(), "staging-peer", vec![item.clone()]);
        let created = runtime
            .http
            .post(format!("http://{address}/api/v1/outgoing"))
            .json(&offer)
            .send()
            .await
            .expect("create staging");
        assert!(created.status().is_success());

        let item_url = format!(
            "http://{address}/api/v1/outgoing/{}/items/{}",
            offer.transfer_id, item.item_id
        );
        let rejected = runtime
            .http
            .put(item_url.as_str())
            .body(b"wrong".to_vec())
            .send()
            .await
            .expect("stage wrong bytes");
        assert_eq!(rejected.status(), reqwest::StatusCode::BAD_REQUEST);
        assert!(!runtime.is_outgoing_item_ready(&offer.transfer_id, &item.item_id));

        let staged = runtime
            .http
            .put(item_url.as_str())
            .body(b"staged".to_vec())
            .send()
            .await
            .expect("stage bytes");
        assert!(staged.status().is_success());
        assert!(runtime.is_outgoing_item_ready(&offer.transfer_id, &item.item_id));
        let repeated = runtime
            .http
            .put(item_url.as_str())
            .body(b"staged".to_vec())
            .send()
            .await
            .expect("stage repeated bytes");
        assert!(repeated.status().is_success());

        let started = runtime
            .http
            .post(format!(
                "http://{address}/api/v1/outgoing/{}/start",
                offer.transfer_id
            ))
            .send()
            .await
            .expect("start staging");
        assert_eq!(started.status(), reqwest::StatusCode::CONFLICT);
        assert_eq!(
            runtime
                .transfer(&offer.transfer_id)
                .expect("staged record")
                .progress
                .status,
            TransferStatus::Failed
        );

        let staged_path = runtime
            .outgoing_staged_target(&offer.transfer_id, &item.item_id)
            .expect("staged path");
        assert!(staged_path.exists());

        runtime
            .stopping
            .store(true, std::sync::atomic::Ordering::SeqCst);
        server.abort();
        let _ = server.await;
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn accepted_transfer_without_upload_fails_and_notifies_source() {
        let data_dir =
            std::env::temp_dir().join(format!("meshshare-accepted-idle-{}", Uuid::new_v4()));
        let runtime = NodeRuntime::new(data_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let (tx, mut rx) = mpsc::channel(8);
        assert!(runtime.add_peer_channel("source".to_string(), tx));
        let item = outgoing_test_item("payload.bin", b"staged");
        let offer = TransferOffer {
            transfer_id: "accepted-idle".to_string(),
            source_node_id: "source".to_string(),
            source_name: "source".to_string(),
            target_node_id: runtime.node_id(),
            items: vec![item],
            total_bytes: 6,
            created_at_ms: crate::clock::now_millis(),
        };
        assert!(runtime.add_transfer(offer));
        let destination = data_dir.join("received");
        std::fs::create_dir_all(&destination).expect("create destination");
        runtime
            .set_transfer_acceptance("accepted-idle", destination)
            .expect("accept transfer");
        {
            let mut transfers = runtime.transfers.write();
            let record = transfers.get_mut("accepted-idle").expect("transfer record");
            record.updated_at_ms =
                crate::clock::now_millis() - TRANSFER_ACCEPTED_IDLE_TIMEOUT_MS - 1;
        }
        runtime.prune_transfers();
        let record = runtime.transfer("accepted-idle").expect("transfer record");
        assert_eq!(record.progress.status, TransferStatus::Failed);
        assert!(record
            .progress
            .error
            .as_deref()
            .is_some_and(|error| error.contains("没有开始上传")));
        match rx.try_recv().expect("fail action queued") {
            WireMessage::TransferAction {
                transfer_id,
                action,
                reason,
                retry_count,
            } => {
                assert_eq!(transfer_id, "accepted-idle");
                assert_eq!(action, "fail");
                assert!(reason.is_some());
                assert_eq!(retry_count, Some(0));
            }
            other => panic!("unexpected wire message: {other:?}"),
        }
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn outgoing_staging_survives_until_upload_starts() {
        let data_dir =
            std::env::temp_dir().join(format!("meshshare-staging-window-{}", Uuid::new_v4()));
        let runtime = NodeRuntime::new(data_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let item = outgoing_test_item("payload.bin", b"staged");
        let offer = outgoing_test_offer(&runtime.node_id(), "receiver", vec![item.clone()]);
        runtime
            .begin_outgoing_staging(offer.clone())
            .expect("begin staging");
        runtime.spawn_outgoing_upload_supervisor();
        tokio::time::sleep(std::time::Duration::from_millis(1_200)).await;
        // 还没调用 /start 时没有传输记录，监督器不能把复制式暂存清掉。
        assert_eq!(
            runtime.outgoing_staging_counts(&offer.transfer_id),
            Some((0, 1))
        );
        runtime.mark_outgoing_item_staged(&offer.transfer_id, &item.item_id, item.sha256.clone());
        assert_eq!(
            runtime.outgoing_staging_counts(&offer.transfer_id),
            Some((1, 1))
        );
        runtime
            .stopping
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = std::fs::remove_dir_all(data_dir);
    }

    async fn wait_for_outgoing_record(
        runtime: &Arc<NodeRuntime>,
        transfer_id: &str,
    ) -> TransferProgress {
        for _ in 0..300 {
            if let Some(record) = runtime.transfer(transfer_id) {
                return record.progress;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("outgoing transfer was not registered");
    }

    async fn wait_for_outgoing_prepare(runtime: &Arc<NodeRuntime>, transfer_id: &str) {
        for _ in 0..300 {
            if runtime.outgoing_prepare_view(transfer_id).is_none() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("outgoing prepare did not finish");
    }

    async fn outgoing_receiver_fixture() -> (
        Arc<NodeRuntime>,
        Arc<NodeRuntime>,
        std::net::SocketAddr,
        tokio::task::JoinHandle<()>,
        PathBuf,
    ) {
        let receiver_dir =
            std::env::temp_dir().join(format!("meshshare-path-receiver-{}", Uuid::new_v4()));
        let static_root = receiver_dir.join("web");
        std::fs::create_dir_all(&static_root).expect("create static directory");
        std::fs::write(static_root.join("index.html"), "<html></html>")
            .expect("write static index");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind receiver");
        let address = listener.local_addr().expect("receiver address");
        let receiver = NodeRuntime::new(
            receiver_dir.clone(),
            address.port(),
            env!("CARGO_PKG_VERSION").into(),
        )
        .expect("create receiver runtime");
        let app = crate::server::router(receiver.clone(), static_root);
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        let sender_dir =
            std::env::temp_dir().join(format!("meshshare-path-sender-{}", Uuid::new_v4()));
        let sender = NodeRuntime::new(sender_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create sender runtime");
        register_peer_endpoint(&sender, receiver.node_info());
        (receiver, sender, address, server, sender_dir)
    }

    #[tokio::test]
    async fn outgoing_paths_stream_original_files_without_copying() {
        let (receiver, sender, _address, server, sender_dir) = outgoing_receiver_fixture().await;
        let source_dir = sender_dir.join("source");
        std::fs::create_dir_all(&source_dir).expect("create source directory");
        let payload = b"original file payload".to_vec();
        let source_file = source_dir.join("payload.bin");
        std::fs::write(&source_file, &payload).expect("write source file");
        let mut hasher = Sha256::new();
        hasher.update(&payload);
        let expected_digest = format!("{:x}", hasher.finalize());

        let (tx, mut rx) = mpsc::channel(8);
        assert!(sender.add_peer_channel(receiver.node_id(), tx));
        let view = sender
            .begin_outgoing_paths(
                receiver.node_id(),
                vec![source_file.to_string_lossy().into_owned()],
            )
            .expect("begin outgoing paths");
        wait_for_outgoing_prepare(&sender, &view.transfer_id).await;
        let progress = wait_for_outgoing_record(&sender, &view.transfer_id).await;
        assert_eq!(progress.status, TransferStatus::Waiting);
        assert_eq!(progress.total_bytes, payload.len() as u64);
        let offer = match tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("offer was sent")
            .expect("offer message")
        {
            WireMessage::TransferOffer { offer } => offer,
            other => panic!("unexpected wire message: {other:?}"),
        };
        assert_eq!(offer.items[0].sha256, expected_digest);
        assert_eq!(offer.items[0].relative_path, "payload.bin");
        // 原路径直传不应产生任何暂存副本。
        let staging_dir = sender_dir.join("outgoing").join(&view.transfer_id);
        let staged_files = std::fs::read_dir(&staging_dir)
            .map(|entries| entries.filter_map(Result::ok).count())
            .unwrap_or(0);
        assert_eq!(staged_files, 0);

        receiver.add_transfer(offer.clone());
        let destination =
            std::env::temp_dir().join(format!("meshshare-path-target-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&destination).expect("create destination");
        receiver
            .set_transfer_acceptance(&offer.transfer_id, destination.clone())
            .expect("accept transfer");
        sender.mark_transfer_accepted(&offer.transfer_id);
        sender
            .upload_outgoing_transfer(&offer.transfer_id)
            .await
            .expect("upload original file");
        assert_eq!(
            std::fs::read(destination.join("payload.bin")).expect("read received payload"),
            payload
        );
        assert_eq!(
            sender
                .transfer(&offer.transfer_id)
                .expect("sender record")
                .progress
                .status,
            TransferStatus::Completed
        );
        assert!(!staging_dir.exists());

        sender
            .stopping
            .store(true, std::sync::atomic::Ordering::SeqCst);
        server.abort();
        let _ = server.await;
        let _ = std::fs::remove_dir_all(&sender_dir);
        let _ = std::fs::remove_dir_all(destination);
    }

    #[tokio::test]
    async fn outgoing_paths_expand_directory_relative_paths() {
        let (_receiver, sender, _address, server, sender_dir) = outgoing_receiver_fixture().await;
        let root = sender_dir.join("folder");
        std::fs::create_dir_all(root.join("sub")).expect("create nested directory");
        std::fs::write(root.join("a.txt"), b"alpha").expect("write first file");
        std::fs::write(root.join("sub/b.txt"), b"beta").expect("write second file");

        let target = "folder-peer";
        let (tx, mut rx) = mpsc::channel(8);
        assert!(sender.add_peer_channel(target.to_string(), tx));
        let view = sender
            .begin_outgoing_paths(
                target.to_string(),
                vec![root.to_string_lossy().into_owned()],
            )
            .expect("begin outgoing paths");
        wait_for_outgoing_prepare(&sender, &view.transfer_id).await;
        let progress = wait_for_outgoing_record(&sender, &view.transfer_id).await;
        assert_eq!(progress.total_items, 2);
        assert_eq!(progress.total_bytes, 9);
        let offer = match tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("offer was sent")
            .expect("offer message")
        {
            WireMessage::TransferOffer { offer } => offer,
            other => panic!("unexpected wire message: {other:?}"),
        };
        let mut paths = offer
            .items
            .iter()
            .map(|item| item.relative_path.clone())
            .collect::<Vec<_>>();
        paths.sort();
        assert_eq!(paths, vec!["folder/a.txt", "folder/sub/b.txt"]);

        sender
            .stopping
            .store(true, std::sync::atomic::Ordering::SeqCst);
        server.abort();
        let _ = server.await;
        let _ = std::fs::remove_dir_all(&sender_dir);
    }

    #[tokio::test]
    async fn outgoing_paths_fail_when_source_changes() {
        let (receiver, sender, _address, server, sender_dir) = outgoing_receiver_fixture().await;
        let source_file = sender_dir.join("changing.bin");
        std::fs::write(&source_file, b"stable").expect("write source file");
        let (tx, mut rx) = mpsc::channel(8);
        assert!(sender.add_peer_channel(receiver.node_id(), tx));
        let view = sender
            .begin_outgoing_paths(
                receiver.node_id(),
                vec![source_file.to_string_lossy().into_owned()],
            )
            .expect("begin outgoing paths");
        wait_for_outgoing_prepare(&sender, &view.transfer_id).await;
        let offer = match rx.recv().await.expect("offer message") {
            WireMessage::TransferOffer { offer } => offer,
            other => panic!("unexpected wire message: {other:?}"),
        };
        receiver.add_transfer(offer.clone());
        let destination = sender_dir.join("changed-target");
        std::fs::create_dir_all(&destination).expect("create destination");
        receiver
            .set_transfer_acceptance(&offer.transfer_id, destination)
            .expect("accept transfer");
        sender.mark_transfer_accepted(&offer.transfer_id);
        // 准备完成后源文件被追加内容：上传前的大小对比必须拦住。
        std::fs::write(&source_file, b"changed and longer").expect("rewrite source file");
        let error = sender
            .upload_outgoing_transfer(&offer.transfer_id)
            .await
            .expect_err("changed source must fail");
        assert!(error.contains("源文件已变化"), "unexpected error: {error}");
        assert_eq!(
            sender
                .transfer(&offer.transfer_id)
                .expect("sender record")
                .progress
                .status,
            TransferStatus::Failed
        );

        sender
            .stopping
            .store(true, std::sync::atomic::Ordering::SeqCst);
        server.abort();
        let _ = server.await;
        let _ = std::fs::remove_dir_all(&sender_dir);
    }
}
