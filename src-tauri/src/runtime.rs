use crate::clock::{now_millis, LogicalClock};
use crate::model::{
    AddressInfo, ClipboardEvent, ClipboardSnapshot, HardwareMetrics, MobileSession,
    MobileTransferView, MonitorDisplayMode, NodeInfo, PeerInfo, TransferOffer, TransferProgress,
    TransferStatus, WireMessage, METRICS_GPU_CAPABILITY, METRICS_SYSTEM_CAPABILITY,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
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
}

#[derive(Debug, Clone)]
pub struct MobileTransferRecord {
    pub session_id: String,
    pub offer: TransferOffer,
    pub progress: TransferProgress,
    pub staged_directory: PathBuf,
    pub staged_items: HashSet<String>,
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
    config: Mutex<PersistedConfig>,
    data_dir: PathBuf,
    seen_events: Mutex<HashMap<String, i64>>,
    suppression: Mutex<Option<(String, i64)>>,
    peer_channels: Mutex<HashMap<String, mpsc::Sender<WireMessage>>>,
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

fn mobile_view(record: &MobileTransferRecord) -> MobileTransferView {
    MobileTransferView {
        session_id: record.session_id.clone(),
        progress: record.progress.clone(),
    }
}

impl NodeRuntime {
    pub fn new(data_dir: PathBuf, port: u16, version: String) -> anyhow::Result<Arc<Self>> {
        std::fs::create_dir_all(&data_dir)?;
        let config = load_or_create_config(&data_dir)?;
        let addresses = network_addresses(port);
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
            config: Mutex::new(config),
            data_dir,
            seen_events: Mutex::new(HashMap::new()),
            suppression: Mutex::new(None),
            peer_channels: Mutex::new(HashMap::new()),
            connecting_peers: Mutex::new(HashSet::new()),
            transfer_locks: Mutex::new(HashSet::new()),
            events,
            sync_enabled: AtomicBool::new(true),
            stopping: AtomicBool::new(false),
            http: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(5))
                .build()?,
        }))
    }

    pub fn node_info(&self) -> NodeInfo {
        self.node.read().clone()
    }

    pub fn node_id(&self) -> String {
        self.node.read().node_id.clone()
    }

    pub fn update_addresses(&self) {
        let port = self.node.read().port;
        self.node.write().addresses = network_addresses(port);
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
        channel.is_some_and(|channel| channel.try_send(message).is_ok())
    }

    pub fn send_to_peers_except(&self, except: Option<&str>, message: WireMessage) {
        let channels = self
            .peer_channels
            .lock()
            .iter()
            .filter(|(node_id, _)| Some(node_id.as_str()) != except)
            .map(|(_, channel)| channel.clone())
            .collect::<Vec<_>>();
        for channel in channels {
            let _ = channel.try_send(message.clone());
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
        if let Some(peer) = self.peers.write().get_mut(node_id) {
            peer.online = false;
        }
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
        let now = now_millis();
        {
            let mut seen = self.seen_events.lock();
            seen.retain(|_, timestamp| now.saturating_sub(*timestamp) < 10 * 60 * 1000);
            if seen.contains_key(&event.event_id) {
                return false;
            }
            seen.insert(event.event_id.clone(), now);
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
        self.transfers.write().insert(
            offer.transfer_id.clone(),
            TransferRecord {
                offer: offer.clone(),
                progress: progress.clone(),
                accepted_destination: None,
                received_items: HashSet::new(),
            },
        );
        self.publish_event(WireMessage::TransferOffer { offer });
        self.publish_event(WireMessage::TransferProgress { progress });
    }

    pub fn add_outgoing_transfer(&self, offer: TransferOffer) -> TransferProgress {
        let progress = transfer_progress(&offer, "send");
        self.transfers.write().insert(
            offer.transfer_id.clone(),
            TransferRecord {
                offer,
                progress: progress.clone(),
                accepted_destination: None,
                received_items: HashSet::new(),
            },
        );
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
        if record.received_items.insert(item_id.to_string()) {
            record.progress.completed_items = record.progress.completed_items.saturating_add(1);
            record.progress.completed_bytes = record.progress.completed_bytes.saturating_add(bytes);
        }
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
        record.progress.offer = Some(record.offer.clone());
        Some(record.progress.clone())
    }

    pub fn transfers_snapshot(&self, status: Option<TransferStatus>) -> Vec<TransferProgress> {
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
        let progress = transfer_progress(&offer, "receive");
        let staged_directory = self
            .data_dir
            .join("mobile-transfers")
            .join(&offer.transfer_id);
        let record = MobileTransferRecord {
            session_id: session_id.to_string(),
            offer,
            progress,
            staged_directory,
            staged_items: HashSet::new(),
        };
        let view = mobile_view(&record);
        self.mobile_transfers
            .write()
            .insert(record.offer.transfer_id.clone(), record);
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
        let expired = transfers
            .iter()
            .filter(|(_, record)| !sessions.contains_key(&record.session_id))
            .map(|(transfer_id, record)| (transfer_id.clone(), record.staged_directory.clone()))
            .collect::<Vec<_>>();
        for (transfer_id, _) in &expired {
            transfers.remove(transfer_id);
        }
        expired
            .into_iter()
            .map(|(_, directory)| directory)
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

    pub fn fail_mobile_transfer(
        &self,
        transfer_id: &str,
        message: String,
    ) -> Option<MobileTransferView> {
        let mut transfers = self.mobile_transfers.write();
        let record = transfers.get_mut(transfer_id)?;
        if matches!(
            record.progress.status,
            TransferStatus::Ready | TransferStatus::Rejected | TransferStatus::Failed
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

pub fn network_addresses(port: u16) -> Vec<AddressInfo> {
    let mut addresses = get_if_addrs::get_if_addrs()
        .map(|items| {
            items
                .into_iter()
                .filter_map(|interface| {
                    let ip = interface.ip();
                    match ip {
                        IpAddr::V4(ipv4) if !ipv4.is_loopback() => {
                            let address = ipv4.to_string();
                            let value = u32::from(ipv4);
                            let tailscale = (value & 0xffc00000) == 0x64400000;
                            if !ipv4.is_private() && !ipv4.is_link_local() && !tailscale {
                                return None;
                            }
                            let kind = if tailscale {
                                "tailscale"
                            } else if ipv4.is_private() {
                                "lan"
                            } else {
                                "network"
                            };
                            Some(AddressInfo {
                                address: address.clone(),
                                url: format!("http://{address}:{port}"),
                                label: format!("{} · {}", interface.name, address),
                                kind: kind.to_string(),
                            })
                        }
                        _ => None,
                    }
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    addresses.sort_by(|left, right| {
        let rank = |kind: &str| match kind {
            "lan" => 0,
            "tailscale" => 1,
            _ => 2,
        };
        rank(&left.kind)
            .cmp(&rank(&right.kind))
            .then(left.address.cmp(&right.address))
    });
    addresses.dedup_by(|left, right| left.address == right.address);
    if addresses.is_empty() {
        addresses.push(AddressInfo {
            address: "127.0.0.1".to_string(),
            url: format!("http://127.0.0.1:{port}"),
            label: "本机 · 127.0.0.1".to_string(),
            kind: "loopback".to_string(),
        });
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
    use super::{load_or_create_config, NodeRuntime};
    use crate::model::{
        GpuMemoryKind, GpuMetrics, HardwareMetrics, PeerInfo, TransferItem, TransferOffer,
        TransferStatus, METRICS_SYSTEM_CAPABILITY,
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
        assert!(runtime
            .mobile_item_path("mobile-transfer", "item")
            .is_some());
        let _ = std::fs::remove_dir_all(path);
    }
}
