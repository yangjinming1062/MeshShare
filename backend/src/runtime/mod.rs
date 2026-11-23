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
    TERMINAL_SSH_CAPABILITY, TRANSFER_HISTORY_CAPABILITY, TRANSFER_HISTORY_LIMIT,
};
use futures_util::StreamExt;
use parking_lot::Condvar;
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::AsyncReadExt;
use tokio::sync::{broadcast, mpsc, watch};
use tokio_util::io::ReaderStream;
use uuid::Uuid;
use walkdir::WalkDir;

mod config;
mod history;
mod mobile;
mod network;
mod outgoing;
mod peers;
mod transfer;
mod transfer_history;

use config::*;
use history::*;
use mobile::MobileTransferRecord;
use network::*;
use outgoing::*;
use transfer::*;
use transfer_history::*;

pub use config::UpdateFrequency;
pub use outgoing::OutgoingPrepareView;

pub use config::validate_receive_directory;
pub use network::bind_addresses;
#[cfg(not(target_os = "android"))]
pub(crate) use network::is_tailscale_ipv4;
pub(crate) use network::{
    is_allowed_peer_address, node_http_client, normalize_peer_http_base, PeerEndpoint,
    PeerEndpointSource,
};

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

pub(crate) struct TerminalActivity(std::sync::Weak<NodeRuntime>);

impl Drop for TerminalActivity {
    fn drop(&mut self) {
        if let Some(runtime) = self.0.upgrade() {
            let _gate = runtime.transfer_update_gate.lock();
            if runtime.terminal_count.fetch_sub(1, Ordering::SeqCst) == 1 {
                runtime.terminals_stopped.notify_all();
            }
        }
    }
}

pub struct NodeRuntime {
    pub(crate) ssh: Arc<crate::terminal::ssh::SshService>,
    pub(crate) terminals: crate::terminal::commands::WindowTerminals,
    terminal_count: AtomicU64,
    terminals_stopped: Condvar,
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
    mobile_ca: RwLock<Option<MobileCa>>,
    config: Mutex<PersistedConfig>,
    data_dir: PathBuf,
    clipboard_update: Mutex<()>,
    seen_events: Mutex<HashMap<String, i64>>,
    peer_channels: Mutex<HashMap<String, peers::PeerChannel>>,
    pending_clipboard_resync: Mutex<HashSet<String>>,
    peer_dials: Mutex<HashMap<String, watch::Sender<Vec<PeerEndpoint>>>>,
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

pub(crate) struct PeerSession<'a> {
    runtime: &'a NodeRuntime,
    node_id: String,
}

impl PeerSession<'_> {
    pub(crate) fn node_id(&self) -> &str {
        &self.node_id
    }
}

impl Drop for PeerSession<'_> {
    fn drop(&mut self) {
        self.runtime.remove_peer(&self.node_id);
    }
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
        awaiting_offer_reply: false,
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
#[cfg(any(test, not(target_os = "android")))]
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
    !is_active_transfer(status)
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
        let config = load_or_create_config(&data_dir, profile)?;
        // 启动时清空网页暂存与出站暂存：两处内容只服务于进程内任务。
        crate::fsutil::remove_dir_if_exists(&data_dir.join("mobile-transfers"))?;
        crate::fsutil::remove_dir_if_exists(&data_dir.join(OUTGOING_STAGING_DIR))?;
        let desktop_temp_files = load_and_cleanup_desktop_temp_files(&data_dir)?;
        cleanup_clipboard_temp_files(&data_dir)?;
        #[cfg(target_os = "macos")]
        let mut config = config;
        #[cfg(target_os = "macos")]
        {
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
        #[cfg(not(target_os = "android"))]
        let host_ready = !profile.is_android() && config.terminal_host_ready(&data_dir);
        #[cfg(target_os = "android")]
        let host_ready = false;
        let ssh = Arc::new(crate::terminal::ssh::SshService::new(
            config.node_id.clone(),
            profile,
            host_ready,
        )?);
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
            terminal: Some(crate::model::TerminalKeys {
                client_fingerprint: ssh.client_fingerprint(),
                host_fingerprint: None,
            }),
        };
        let (events, _) = broadcast::channel(256);
        Ok(Arc::new(Self {
            ssh,
            terminals: Default::default(),
            terminal_count: AtomicU64::new(0),
            terminals_stopped: Condvar::new(),
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
            mobile_ca: RwLock::new(None),
            config: Mutex::new(config),
            data_dir,
            clipboard_update: Mutex::new(()),
            seen_events: Mutex::new(HashMap::new()),
            peer_channels: Mutex::new(HashMap::new()),
            pending_clipboard_resync: Mutex::new(HashSet::new()),
            peer_dials: Mutex::new(HashMap::new()),
            transfer_locks: Mutex::new(HashSet::new()),
            desktop_temp_files: Mutex::new(desktop_temp_files),
            staged_outgoing: Mutex::new(HashMap::new()),
            outgoing_prepares: Mutex::new(HashMap::new()),
            outgoing_uploads: Mutex::new(HashSet::new()),
            events,
            sync_enabled: AtomicBool::new(true),
            stopping: AtomicBool::new(false),
            update_installing: AtomicBool::new(false),
            http: node_http_client()
                .connect_timeout(std::time::Duration::from_secs(5))
                .timeout(std::time::Duration::from_secs(15))
                .build()?,
            proxy_http: node_http_client()
                .connect_timeout(std::time::Duration::from_secs(5))
                .build()?,
            upload_http: node_http_client()
                .connect_timeout(std::time::Duration::from_secs(8))
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
        if capability == TERMINAL_SSH_CAPABILITY {
            return self.ssh.host_fingerprint().is_some();
        }
        self.node
            .read()
            .capabilities
            .iter()
            .any(|item| item == capability)
    }

    pub fn node_info(&self) -> NodeInfo {
        let mut node = self.node.read().clone();
        if let (Some(keys), Some(host)) = (&mut node.terminal, self.ssh.host_fingerprint()) {
            keys.host_fingerprint = Some(host);
            node.capabilities.push(TERMINAL_SSH_CAPABILITY.to_owned());
        }
        node
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
    #[cfg(any(test, not(target_os = "android")))]
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

    pub fn publish_event(&self, message: WireMessage) {
        let _ = self.events.send(message);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<WireMessage> {
        self.events.subscribe()
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

    pub(crate) fn reserve_terminal(self: &Arc<Self>) -> anyhow::Result<Arc<TerminalActivity>> {
        let _gate = self.transfer_update_gate.lock();
        anyhow::ensure!(!self.stopping.load(Ordering::SeqCst), "节点正在退出");
        anyhow::ensure!(
            !self.update_installing.load(Ordering::SeqCst),
            "{}",
            UPDATE_INSTALLING_MESSAGE
        );
        anyhow::ensure!(
            self.terminal_count.load(Ordering::SeqCst) < 32,
            "终端会话已满"
        );
        self.terminal_count.fetch_add(1, Ordering::SeqCst);
        Ok(Arc::new(TerminalActivity(Arc::downgrade(self))))
    }

    pub(crate) fn wait_for_terminal_shutdown(&self) {
        let mut gate = self.transfer_update_gate.lock();
        let wait = self.terminals_stopped.wait_while_for(
            &mut gate,
            |_| self.terminal_count.load(Ordering::SeqCst) != 0,
            Duration::from_secs(5),
        );
        if wait.timed_out() {
            eprintln!("退出时等待终端进程清理超时");
        }
    }

    pub(crate) fn update_has_active_work(&self) -> bool {
        if self.terminal_count.load(Ordering::SeqCst) != 0 {
            return true;
        }
        self.has_active_transfers()
    }

    pub fn begin_update_install(&self) -> bool {
        let _update_gate = self.transfer_update_gate.lock();
        if self.update_installing.load(Ordering::SeqCst) || self.update_has_active_work() {
            return false;
        }
        self.update_installing.store(true, Ordering::SeqCst);
        true
    }

    pub fn end_update_install(&self) {
        let _update_gate = self.transfer_update_gate.lock();
        self.update_installing.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests;
