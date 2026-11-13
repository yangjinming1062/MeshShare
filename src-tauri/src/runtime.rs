use crate::clock::{now_millis, LogicalClock};
use crate::model::{
    AddressInfo, ClipboardEvent, ClipboardSnapshot, NodeInfo, PeerInfo, TransferOffer,
    TransferProgress, TransferStatus, WireMessage,
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
}

#[derive(Debug, Clone)]
pub struct TransferRecord {
    pub offer: TransferOffer,
    pub progress: TransferProgress,
    pub accepted_destination: Option<PathBuf>,
    pub received_items: HashSet<String>,
}

pub struct NodeRuntime {
    pub node: RwLock<NodeInfo>,
    pub clock: LogicalClock,
    pub clipboard: RwLock<Option<ClipboardSnapshot>>,
    pub peers: RwLock<HashMap<String, PeerInfo>>,
    pub transfers: RwLock<HashMap<String, TransferRecord>>,
    seen_events: Mutex<HashMap<String, i64>>,
    suppression: Mutex<Option<(String, i64)>>,
    peer_channels: Mutex<HashMap<String, mpsc::UnboundedSender<WireMessage>>>,
    pub events: broadcast::Sender<WireMessage>,
    pub sync_enabled: AtomicBool,
    pub stopping: AtomicBool,
    pub http: reqwest::Client,
}

impl NodeRuntime {
    pub fn new(data_dir: PathBuf, port: u16) -> anyhow::Result<Arc<Self>> {
        std::fs::create_dir_all(&data_dir)?;
        let config = load_or_create_config(&data_dir)?;
        let addresses = network_addresses(port);
        let node = NodeInfo {
            node_id: config.node_id.clone(),
            node_name: config.node_name,
            version: env!("CARGO_PKG_VERSION").to_string(),
            platform: std::env::consts::OS.to_string(),
            port,
            addresses,
            capabilities: vec![
                "clipboard.text".to_string(),
                "clipboard.image/png".to_string(),
                "transfer.file".to_string(),
                "transfer.folder".to_string(),
                "web.mobile".to_string(),
            ],
        };
        let (events, _) = broadcast::channel(256);
        Ok(Arc::new(Self {
            clock: LogicalClock::new(config.node_id),
            node: RwLock::new(node),
            clipboard: RwLock::new(None),
            peers: RwLock::new(HashMap::new()),
            transfers: RwLock::new(HashMap::new()),
            seen_events: Mutex::new(HashMap::new()),
            suppression: Mutex::new(None),
            peer_channels: Mutex::new(HashMap::new()),
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

    pub fn publish(&self, message: WireMessage) {
        let _ = self.events.send(message.clone());
        if matches!(&message, WireMessage::ClipboardUpdate { .. }) {
            let channels = self
                .peer_channels
                .lock()
                .values()
                .cloned()
                .collect::<Vec<_>>();
            for channel in channels {
                let _ = channel.send(message.clone());
            }
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<WireMessage> {
        self.events.subscribe()
    }

    pub fn add_peer_channel(&self, node_id: String, channel: mpsc::UnboundedSender<WireMessage>) {
        self.peer_channels.lock().insert(node_id, channel);
    }

    pub fn remove_peer_channel(&self, node_id: &str) {
        self.peer_channels.lock().remove(node_id);
    }

    pub fn set_peer(&self, peer: PeerInfo) {
        self.peers
            .write()
            .insert(peer.node_id.clone(), peer.clone());
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

    pub fn peers_snapshot(&self) -> Vec<PeerInfo> {
        let mut peers = self.peers.read().values().cloned().collect::<Vec<_>>();
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
        let progress = TransferProgress {
            transfer_id: offer.transfer_id.clone(),
            status: TransferStatus::Waiting,
            completed_bytes: 0,
            total_bytes: offer.total_bytes,
            completed_items: 0,
            total_items: offer.items.len(),
            destination: None,
            error: None,
        };
        self.transfers.write().insert(
            offer.transfer_id.clone(),
            TransferRecord {
                offer: offer.clone(),
                progress: progress.clone(),
                accepted_destination: None,
                received_items: HashSet::new(),
            },
        );
        self.publish(WireMessage::TransferOffer { offer });
        self.publish(WireMessage::TransferProgress { progress });
    }

    pub fn update_transfer(&self, transfer_id: &str, progress: TransferProgress) {
        if let Some(record) = self.transfers.write().get_mut(transfer_id) {
            record.progress = progress.clone();
        }
        self.publish(WireMessage::TransferProgress { progress });
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
        record.accepted_destination = Some(destination.clone());
        record.progress.destination = Some(destination.to_string_lossy().to_string());
        record.progress.status = TransferStatus::Accepted;
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
        if record.received_items.insert(item_id.to_string()) {
            record.progress.completed_items = record.progress.completed_items.saturating_add(1);
            record.progress.completed_bytes = record.progress.completed_bytes.saturating_add(bytes);
        }
        if record.progress.completed_items >= record.progress.total_items {
            record.progress.status = TransferStatus::Completed;
        } else {
            record.progress.status = TransferStatus::Uploading;
        }
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
        record.progress.status = status;
        record.progress.error = Some(message);
        Some(record.progress.clone())
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
    };
    std::fs::write(&path, serde_json::to_vec_pretty(&config)?)?;
    Ok(config)
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
