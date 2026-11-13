use crate::clock::{now_millis, LogicalClock};
use crate::model::{
    AddressInfo, ClipboardEvent, ClipboardSnapshot, HardwareMetrics, MobileSession,
    MobileTransferView, MonitorDisplayMode, NodeInfo, PeerInfo, TransferOffer, TransferProgress,
    TransferStatus, WireMessage, MAX_MOBILE_TRANSFERS_PER_SESSION, METRICS_GPU_CAPABILITY,
    METRICS_SYSTEM_CAPABILITY,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize)]
struct PersistedConfig {
    node_id: String,
    node_name: String,
    #[serde(default)]
    monitor_display: MonitorDisplayMode,
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
}

pub struct NodeRuntime {
    pub node: RwLock<NodeInfo>,
    pub clock: LogicalClock,
    pub clipboard: RwLock<Option<ClipboardSnapshot>>,
    pub peers: RwLock<HashMap<String, PeerInfo>>,
    pub transfers: RwLock<HashMap<String, TransferRecord>>,
    pub mobile_sessions: RwLock<HashMap<String, MobileSession>>,
    pub mobile_transfers: RwLock<HashMap<String, MobileTransferRecord>>,
    pub local_metrics: RwLock<Option<HardwareMetrics>>,
    pub monitor_display: RwLock<MonitorDisplayMode>,
    listening_addresses: RwLock<Option<Vec<Ipv4Addr>>>,
    config: Mutex<PersistedConfig>,
    data_dir: PathBuf,
    clipboard_update: Mutex<()>,
    seen_events: Mutex<HashMap<String, i64>>,
    suppression: Mutex<Option<(String, i64)>>,
    peer_channels: Mutex<HashMap<String, mpsc::Sender<WireMessage>>>,
    pending_clipboard_resync: Mutex<HashSet<String>>,
    connecting_peers: Mutex<HashSet<String>>,
    transfer_locks: Mutex<HashSet<(String, String)>>,
    pub events: broadcast::Sender<WireMessage>,
    pub sync_enabled: AtomicBool,
    pub stopping: AtomicBool,
    pub http: reqwest::Client,
}

fn transfer_progress(offer: &TransferOffer, direction: &str) -> TransferProgress {
    TransferProgress {
        transfer_id: offer.transfer_id.clone(),
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
const TRANSFER_ACTIVITY_TIMEOUT_MS: i64 = 5 * 60 * 1000;
const MAX_TERMINAL_TRANSFERS: usize = 256;
const MAX_MOBILE_TERMINAL_TRANSFERS_PER_SESSION: usize = 128;
const MAX_SEEN_CLIPBOARD_EVENTS: usize = 4_096;

fn mobile_view(record: &MobileTransferRecord) -> MobileTransferView {
    MobileTransferView {
        session_id: record.session_id.clone(),
        progress: record.progress.clone(),
    }
}

fn mobile_reserved_bytes(record: &MobileTransferRecord) -> u64 {
    match &record.progress.status {
        TransferStatus::Waiting
        | TransferStatus::Accepted
        | TransferStatus::Uploading
        | TransferStatus::Ready => record.offer.total_bytes,
        TransferStatus::Failed => record.progress.completed_bytes,
        TransferStatus::Rejected | TransferStatus::Cancelled | TransferStatus::Completed => 0,
    }
}

fn is_clipboard_message(message: &WireMessage) -> bool {
    matches!(
        message,
        WireMessage::ClipboardUpdate { .. } | WireMessage::ClipboardSnapshot { .. }
    )
}

impl NodeRuntime {
    pub fn new(data_dir: PathBuf, port: u16, version: String) -> anyhow::Result<Arc<Self>> {
        std::fs::create_dir_all(&data_dir)?;
        let staging_root = data_dir.join("mobile-transfers");
        match std::fs::remove_dir_all(&staging_root) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        let config = load_or_create_config(&data_dir)?;
        let addresses = network_addresses_for_bound(port, &[Ipv4Addr::LOCALHOST]);
        let node = NodeInfo {
            node_id: config.node_id.clone(),
            node_name: config.node_name.clone(),
            version,
            protocol_version: crate::model::PROTOCOL_VERSION,
            platform: std::env::consts::OS.to_string(),
            port,
            addresses,
            capabilities: vec![
                "clipboard.text".to_string(),
                "clipboard.image/png".to_string(),
                "transfer.file".to_string(),
                "transfer.folder".to_string(),
                "web.mobile".to_string(),
                METRICS_SYSTEM_CAPABILITY.to_string(),
                METRICS_GPU_CAPABILITY.to_string(),
            ],
        };
        let (events, _) = broadcast::channel(256);
        Ok(Arc::new(Self {
            clock: LogicalClock::new(config.node_id.clone()),
            node: RwLock::new(node),
            clipboard: RwLock::new(None),
            peers: RwLock::new(HashMap::new()),
            transfers: RwLock::new(HashMap::new()),
            mobile_sessions: RwLock::new(HashMap::new()),
            mobile_transfers: RwLock::new(HashMap::new()),
            local_metrics: RwLock::new(None),
            monitor_display: RwLock::new(config.monitor_display.clone()),
            listening_addresses: RwLock::new(Some(vec![Ipv4Addr::LOCALHOST])),
            config: Mutex::new(config),
            data_dir,
            clipboard_update: Mutex::new(()),
            seen_events: Mutex::new(HashMap::new()),
            suppression: Mutex::new(None),
            peer_channels: Mutex::new(HashMap::new()),
            pending_clipboard_resync: Mutex::new(HashSet::new()),
            connecting_peers: Mutex::new(HashSet::new()),
            transfer_locks: Mutex::new(HashSet::new()),
            events,
            sync_enabled: AtomicBool::new(true),
            stopping: AtomicBool::new(false),
            http: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(5))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
        }))
    }

    pub fn node_info(&self) -> NodeInfo {
        self.node.read().clone()
    }

    pub fn node_id(&self) -> String {
        self.node.read().node_id.clone()
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

    pub fn update_addresses(&self) {
        let port = self.node.read().port;
        let listening_addresses = self.listening_addresses.read().clone();
        let addresses = network_addresses_for_bound(
            port,
            &currently_available_bound_addresses(listening_addresses.as_deref().unwrap_or(&[])),
        );
        self.node.write().addresses = addresses;
    }

    pub fn set_listening_addresses(&self, addresses: Vec<Ipv4Addr>) {
        let mut addresses = addresses
            .into_iter()
            .filter(|address| is_local_network_ipv4(*address))
            .collect::<Vec<_>>();
        addresses.sort();
        addresses.dedup();
        if addresses.is_empty() {
            addresses.push(Ipv4Addr::LOCALHOST);
        }
        let port = self.node.read().port;
        *self.listening_addresses.write() = Some(addresses.clone());
        self.node.write().addresses = network_addresses_for_bound(port, &addresses);
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
                        peer.capabilities
                            .iter()
                            .any(|capability| capability == METRICS_SYSTEM_CAPABILITY),
                        peer.capabilities
                            .iter()
                            .any(|capability| capability == METRICS_GPU_CAPABILITY),
                    ),
                )
            })
            .collect::<HashMap<_, _>>();
        let channels = self
            .peer_channels
            .lock()
            .iter()
            .filter_map(|(node_id, channel)| {
                let (supports_system, supports_gpu) = peer_capabilities.get(node_id)?;
                if !*supports_system {
                    return None;
                }
                let mut outgoing = metrics.clone();
                if !*supports_gpu {
                    outgoing.gpus.clear();
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

    pub fn remove_peer_channel(&self, node_id: &str) {
        self.peer_channels.lock().remove(node_id);
        self.connecting_peers.lock().remove(node_id);
        self.pending_clipboard_resync.lock().remove(node_id);
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

    pub fn send_to_peer(&self, node_id: &str, message: WireMessage) -> bool {
        let channel = self.peer_channels.lock().get(node_id).cloned();
        let is_clipboard = is_clipboard_message(&message);
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

    pub fn send_to_peers_except(&self, except: Option<&str>, message: WireMessage) {
        let channels = self
            .peer_channels
            .lock()
            .iter()
            .filter(|(node_id, _)| Some(node_id.as_str()) != except)
            .map(|(node_id, channel)| (node_id.clone(), channel.clone()))
            .collect::<Vec<_>>();
        let is_clipboard = is_clipboard_message(&message);
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

    pub fn peer_name(&self, node_id: &str) -> Option<String> {
        self.peers
            .read()
            .get(node_id)
            .map(|peer| peer.node_name.clone())
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

    pub fn monitor_display_mode(&self) -> MonitorDisplayMode {
        self.monitor_display.read().clone()
    }

    pub fn set_monitor_display_mode(&self, mode: MonitorDisplayMode) -> anyhow::Result<()> {
        *self.monitor_display.write() = mode.clone();
        let mut config = self.config.lock();
        config.monitor_display = mode;
        persist_config(&self.data_dir, &config)
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
        let newer = self
            .clipboard
            .read()
            .as_ref()
            .map(|current| event.clock.compare(&current.clock).is_gt())
            .unwrap_or(true);
        if !newer {
            return false;
        }
        self.clock.observe(&event.clock);
        let snapshot = ClipboardSnapshot {
            event_id: event.event_id,
            source_node_id: event.origin_node_id,
            source_name,
            clock: event.clock,
            text: event.text,
            image_png: event.image_png,
            updated_at_ms: event.updated_at_ms,
        };
        *self.clipboard.write() = Some(snapshot);
        true
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
        let _ = self.accept_clipboard(event.clone(), source_name);
        Some(event)
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

    pub fn add_transfer(&self, offer: TransferOffer) {
        let progress = transfer_progress(&offer, "receive");
        let mut transfers = self.transfers.write();
        if transfers.contains_key(&offer.transfer_id) {
            return;
        }
        transfers.insert(
            offer.transfer_id.clone(),
            TransferRecord {
                offer: offer.clone(),
                progress: progress.clone(),
                accepted_destination: None,
                received_items: HashSet::new(),
                updated_at_ms: now_millis(),
            },
        );
        drop(transfers);
        self.publish_event(WireMessage::TransferOffer { offer });
        self.publish_event(WireMessage::TransferProgress { progress });
    }

    pub fn add_outgoing_transfer(&self, offer: TransferOffer) -> TransferProgress {
        let mut transfers = self.transfers.write();
        if let Some(record) = transfers.get(&offer.transfer_id) {
            return record.progress.clone();
        }
        let progress = transfer_progress(&offer, "send");
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

    pub fn transfers_snapshot(&self, status: Option<TransferStatus>) -> Vec<TransferProgress> {
        self.prune_transfers();
        let mut transfers = self
            .transfers
            .read()
            .values()
            .filter(|record| {
                status
                    .as_ref()
                    .map_or(true, |value| &record.progress.status == value)
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
                || now.saturating_sub(record.updated_at_ms) <= TRANSFER_RETENTION_MS
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

    pub fn create_mobile_session(&self, label: String) -> MobileSession {
        let now = now_millis();
        let session = MobileSession {
            session_id: Uuid::new_v4().to_string(),
            node_id: self.node_id(),
            node_name: self.node_info().node_name,
            label,
            created_at_ms: now,
            last_seen_ms: now,
        };
        self.mobile_sessions
            .write()
            .insert(session.session_id.clone(), session.clone());
        session
    }

    pub fn touch_mobile_session(&self, session_id: &str) -> Option<MobileSession> {
        let now = now_millis();
        let mut sessions = self.mobile_sessions.write();
        sessions
            .retain(|_, session| now.saturating_sub(session.last_seen_ms) <= MOBILE_SESSION_TTL_MS);
        let session = sessions.get_mut(session_id)?;
        session.last_seen_ms = now;
        Some(session.clone())
    }

    pub fn mobile_sessions_snapshot(&self) -> Vec<MobileSession> {
        let now = now_millis();
        let mut sessions = self.mobile_sessions.write();
        sessions
            .retain(|_, session| now.saturating_sub(session.last_seen_ms) <= MOBILE_SESSION_TTL_MS);
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
        if offer.total_bytes > crate::model::MAX_MOBILE_STAGING_BYTES_PER_SESSION {
            return Err("手机临时文件超过会话配额".to_string());
        }
        let mut transfers = self.mobile_transfers.write();
        if transfers
            .values()
            .any(|record| record.offer.transfer_id == offer.transfer_id)
        {
            return Err("手机传输任务已存在".to_string());
        }
        if transfers
            .values()
            .filter(|record| record.session_id == session_id)
            .count()
            >= MAX_MOBILE_TRANSFERS_PER_SESSION
        {
            return Err("手机会话传输任务过多".to_string());
        }
        let reserved = transfers
            .values()
            .filter(|record| record.session_id == session_id)
            .try_fold(0u64, |total, record| {
                total.checked_add(mobile_reserved_bytes(record))
            })
            .ok_or_else(|| "手机临时文件超过会话配额".to_string())?;
        if reserved
            .checked_add(offer.total_bytes)
            .map_or(true, |total| {
                total > crate::model::MAX_MOBILE_STAGING_BYTES_PER_SESSION
            })
        {
            return Err("手机临时文件超过会话配额".to_string());
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
        };
        let view = mobile_view(&record);
        transfers.insert(record.offer.transfer_id.clone(), record);
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
        let mut remove = transfers
            .iter()
            .filter(|(_, record)| {
                !sessions.contains_key(&record.session_id)
                    || (terminal(&record.progress.status)
                        && now.saturating_sub(record.offer.created_at_ms) > TRANSFER_RETENTION_MS)
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
                        record.offer.created_at_ms,
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
        remove.sort_by(|left, right| left.0.cmp(&right.0));
        remove.dedup_by(|left, right| left.0 == right.0);
        for (transfer_id, _) in &remove {
            transfers.remove(transfer_id);
        }
        remove
            .into_iter()
            .filter_map(|(_, directory)| {
                self.is_mobile_staging_directory(&directory)
                    .then_some(directory)
            })
            .collect()
    }

    pub fn mobile_transfer(&self, transfer_id: &str) -> Option<MobileTransferRecord> {
        self.mobile_transfers.read().get(transfer_id).cloned()
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
        Some(mobile_view(record))
    }

    pub fn mark_mobile_item_downloaded(
        &self,
        transfer_id: &str,
        item_id: &str,
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
        record.downloaded_items.insert(item_id.to_string());
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

fn load_or_create_config(data_dir: &Path) -> anyhow::Result<PersistedConfig> {
    let path = data_dir.join("node.json");
    if let Ok(contents) = std::fs::read_to_string(&path) {
        if let Ok(config) = serde_json::from_str::<PersistedConfig>(&contents) {
            return Ok(config);
        }
    }
    let config = PersistedConfig {
        node_id: Uuid::new_v4().to_string(),
        node_name: hostname::get()
            .ok()
            .and_then(|value| value.into_string().ok())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "MeshShare 节点".to_string()),
        monitor_display: MonitorDisplayMode::default(),
    };
    persist_config(data_dir, &config)?;
    Ok(config)
}

fn persist_config(data_dir: &Path, config: &PersistedConfig) -> anyhow::Result<()> {
    let path = data_dir.join("node.json");
    std::fs::write(path, serde_json::to_vec_pretty(config)?)?;
    Ok(())
}

fn is_tailscale_ipv4(address: Ipv4Addr) -> bool {
    (u32::from(address) & 0xffc00000) == 0x64400000
}

fn is_local_network_ipv4(address: Ipv4Addr) -> bool {
    address.is_loopback()
        || address.is_private()
        || address.is_link_local()
        || is_tailscale_ipv4(address)
}

fn network_addresses_for_bound(port: u16, bound: &[Ipv4Addr]) -> Vec<AddressInfo> {
    let interface_names = get_if_addrs::get_if_addrs()
        .map(|interfaces| {
            interfaces
                .into_iter()
                .filter_map(|interface| match interface.ip() {
                    IpAddr::V4(address) => Some((address, interface.name)),
                    IpAddr::V6(_) => None,
                })
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    let mut addresses = bound
        .iter()
        .copied()
        .filter(|address| is_local_network_ipv4(*address))
        .map(|address| {
            let address_text = address.to_string();
            let kind = if address.is_loopback() {
                "loopback"
            } else if is_tailscale_ipv4(address) {
                "tailscale"
            } else if address.is_private() {
                "lan"
            } else {
                "network"
            };
            let label = if address.is_loopback() {
                format!("本机 · {address_text}")
            } else {
                format!(
                    "{} · {address_text}",
                    interface_names
                        .get(&address)
                        .map(String::as_str)
                        .unwrap_or("network")
                )
            };
            AddressInfo {
                address: address_text.clone(),
                url: format!("http://{address_text}:{port}"),
                label,
                kind: kind.to_string(),
            }
        })
        .collect::<Vec<_>>();
    addresses.sort_by(|left, right| {
        let rank = |kind: &str| match kind {
            "lan" => 0,
            "tailscale" => 1,
            "network" => 2,
            _ => 3,
        };
        rank(&left.kind)
            .cmp(&rank(&right.kind))
            .then(left.address.cmp(&right.address))
    });
    addresses.dedup_by(|left, right| left.address == right.address);
    if addresses.is_empty() {
        addresses.push(AddressInfo {
            address: Ipv4Addr::LOCALHOST.to_string(),
            url: format!("http://127.0.0.1:{port}"),
            label: "本机 · 127.0.0.1".to_string(),
            kind: "loopback".to_string(),
        });
    }
    addresses
}

fn currently_available_bound_addresses(bound: &[Ipv4Addr]) -> Vec<Ipv4Addr> {
    let Ok(interfaces) = get_if_addrs::get_if_addrs() else {
        return bound.to_vec();
    };
    let available = interfaces
        .into_iter()
        .filter_map(|interface| match interface.ip() {
            IpAddr::V4(address) => Some(address),
            IpAddr::V6(_) => None,
        })
        .collect::<HashSet<_>>();
    let mut addresses = bound
        .iter()
        .copied()
        .filter(|address| address.is_loopback() || available.contains(address))
        .collect::<Vec<_>>();
    addresses.sort();
    addresses.dedup();
    if addresses.is_empty() {
        addresses.push(Ipv4Addr::LOCALHOST);
    }
    addresses
}

pub fn bind_addresses() -> Vec<std::net::Ipv4Addr> {
    let mut addresses = vec![std::net::Ipv4Addr::LOCALHOST];
    if let Ok(interfaces) = get_if_addrs::get_if_addrs() {
        for interface in interfaces {
            let IpAddr::V4(address) = interface.ip() else {
                continue;
            };
            let value = u32::from(address);
            let tailscale = (value & 0xffc00000) == 0x64400000;
            if address.is_private() || address.is_link_local() || tailscale {
                addresses.push(address);
            }
        }
    }
    addresses.sort();
    addresses.dedup();
    addresses
}

#[cfg(test)]
mod tests {
    use super::{bind_addresses, load_or_create_config, network_addresses_for_bound, NodeRuntime};
    use crate::model::{
        ClipboardEvent, GpuMemoryKind, GpuMetrics, HardwareMetrics, Hlc, PeerInfo, TransferItem,
        TransferOffer, TransferStatus, WireMessage, MAX_MOBILE_STAGING_BYTES_PER_SESSION,
        METRICS_SYSTEM_CAPABILITY,
    };
    use std::path::PathBuf;
    use tokio::sync::mpsc;
    use uuid::Uuid;

    fn test_peer(node_id: &str, capabilities: Vec<String>) -> PeerInfo {
        PeerInfo {
            node_id: node_id.to_string(),
            node_name: "peer".to_string(),
            platform: "test".to_string(),
            addresses: Vec::new(),
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
    fn bound_addresses_exclude_public_addresses_and_prefer_lan() {
        let addresses = network_addresses_for_bound(
            37880,
            &[
                std::net::Ipv4Addr::LOCALHOST,
                std::net::Ipv4Addr::new(192, 168, 1, 10),
                std::net::Ipv4Addr::new(100, 64, 0, 2),
                std::net::Ipv4Addr::new(8, 8, 8, 8),
            ],
        );
        assert_eq!(addresses[0].address, "192.168.1.10");
        assert!(addresses.iter().all(|address| address.address != "8.8.8.8"));
        assert!(addresses
            .iter()
            .any(|address| address.address == "127.0.0.1"));
        assert!(addresses
            .iter()
            .any(|address| address.address == "100.64.0.2"));
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
    fn old_config_uses_floating_monitor_default() {
        let path = temp_data_dir();
        std::fs::write(
            path.join("node.json"),
            r#"{"node_id":"node-a","node_name":"test"}"#,
        )
        .expect("write old config");
        let config = load_or_create_config(&path).expect("load config");
        assert_eq!(config.monitor_display.as_str(), "floating");
        let _ = std::fs::remove_dir_all(path);
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
        let session = runtime.create_mobile_session("test phone".to_string());
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
            .mark_mobile_item_downloaded("mobile-transfer", "item")
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
        let session = runtime.create_mobile_session("test phone".to_string());
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
    fn mobile_session_reserves_a_bounded_staging_quota() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let session = runtime.create_mobile_session("test phone".to_string());
        runtime
            .create_mobile_transfer(
                &session.session_id,
                mobile_offer(&runtime, "quota-full", MAX_MOBILE_STAGING_BYTES_PER_SESSION),
            )
            .expect("reserve full session quota");
        let error = runtime
            .create_mobile_transfer(&session.session_id, mobile_offer(&runtime, "quota-over", 1))
            .expect_err("second transfer must exceed session quota");
        assert!(error.contains("配额"));
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn rejected_mobile_transfer_releases_session_quota() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let session = runtime.create_mobile_session("test phone".to_string());
        runtime
            .create_mobile_transfer(
                &session.session_id,
                mobile_offer(
                    &runtime,
                    "quota-rejected",
                    MAX_MOBILE_STAGING_BYTES_PER_SESSION,
                ),
            )
            .expect("reserve session quota");
        runtime
            .set_mobile_transfer_status(
                "quota-rejected",
                &session.session_id,
                TransferStatus::Rejected,
            )
            .expect("reject mobile transfer");
        runtime
            .create_mobile_transfer(
                &session.session_id,
                mobile_offer(
                    &runtime,
                    "quota-reused",
                    MAX_MOBILE_STAGING_BYTES_PER_SESSION,
                ),
            )
            .expect("rejected transfer no longer reserves quota");
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn cancelled_mobile_transfer_releases_session_quota() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let session = runtime.create_mobile_session("test phone".to_string());
        runtime
            .create_mobile_transfer(
                &session.session_id,
                mobile_offer(
                    &runtime,
                    "quota-cancelled",
                    MAX_MOBILE_STAGING_BYTES_PER_SESSION,
                ),
            )
            .expect("reserve session quota");
        runtime
            .set_mobile_transfer_status(
                "quota-cancelled",
                &session.session_id,
                TransferStatus::Accepted,
            )
            .expect("accept mobile transfer");
        runtime
            .cancel_mobile_transfer("quota-cancelled", &session.session_id)
            .expect("cancel mobile transfer");
        runtime
            .create_mobile_transfer(
                &session.session_id,
                mobile_offer(
                    &runtime,
                    "quota-after-cancel",
                    MAX_MOBILE_STAGING_BYTES_PER_SESSION,
                ),
            )
            .expect("cancelled transfer no longer reserves quota");
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn failed_mobile_transfer_reserves_only_staged_bytes() {
        let path = temp_data_dir();
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let session = runtime.create_mobile_session("test phone".to_string());
        let first_size = MAX_MOBILE_STAGING_BYTES_PER_SESSION - 1;
        let first_offer = TransferOffer {
            transfer_id: "quota-failed".to_string(),
            source_node_id: runtime.node_id(),
            source_name: "desktop".to_string(),
            target_node_id: runtime.node_id(),
            items: vec![
                TransferItem {
                    item_id: "first".to_string(),
                    name: "first.bin".to_string(),
                    relative_path: "first.bin".to_string(),
                    size: first_size,
                    sha256: "hash".to_string(),
                    media_type: "application/octet-stream".to_string(),
                    directory: false,
                },
                TransferItem {
                    item_id: "second".to_string(),
                    name: "second.bin".to_string(),
                    relative_path: "second.bin".to_string(),
                    size: 1,
                    sha256: "hash".to_string(),
                    media_type: "application/octet-stream".to_string(),
                    directory: false,
                },
            ],
            total_bytes: MAX_MOBILE_STAGING_BYTES_PER_SESSION,
            created_at_ms: crate::clock::now_millis(),
        };
        runtime
            .create_mobile_transfer(&session.session_id, first_offer)
            .expect("reserve full session quota");
        runtime
            .set_mobile_transfer_status(
                "quota-failed",
                &session.session_id,
                TransferStatus::Accepted,
            )
            .expect("accept mobile transfer");
        runtime
            .stage_mobile_item("quota-failed", "first", first_size)
            .expect("stage first item");
        runtime
            .fail_mobile_transfer("quota-failed", "network error".to_string())
            .expect("fail mobile transfer");
        runtime
            .create_mobile_transfer(
                &session.session_id,
                mobile_offer(&runtime, "quota-after-failure", 1),
            )
            .expect("failed transfer reserves only staged bytes");
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
}
