use super::*;
#[cfg(not(target_os = "android"))]
use tokio_util::sync::{CancellationToken, DropGuard};

pub(super) struct PeerChannel {
    sender: mpsc::Sender<WireMessage>,
    #[cfg(not(target_os = "android"))]
    terminal_client: DropGuard,
}

impl PeerChannel {
    fn new(sender: mpsc::Sender<WireMessage>) -> Self {
        Self {
            sender,
            #[cfg(not(target_os = "android"))]
            terminal_client: CancellationToken::new().drop_guard(),
        }
    }
}

impl NodeRuntime {
    pub fn publish_metrics(&self, metrics: HardwareMetrics) {
        *self.local_metrics.write() = Some(metrics.clone());
        let _ = self.events.send(WireMessage::MetricsUpdate {
            metrics: metrics.clone(),
        });
        let peers = self.peers.read();
        let channels = self.peer_channels.lock();
        for (id, channel) in channels.iter() {
            let Some(peer) = peers.get(id).filter(|peer| peer.online) else {
                continue;
            };
            let supports = |name| peer.capabilities.iter().any(|value| value == name);
            let system =
                supports(METRICS_SYSTEM_CAPABILITY) || supports(METRICS_SYSTEM_VIEW_CAPABILITY);
            let gpu = supports(METRICS_GPU_CAPABILITY) || supports(METRICS_GPU_VIEW_CAPABILITY);
            let network = supports(METRICS_NETWORK_CAPABILITY);
            if !system && !network {
                continue;
            }
            let mut outgoing = if system {
                metrics.clone()
            } else {
                HardwareMetrics {
                    node_id: metrics.node_id.clone(),
                    sampled_at_ms: metrics.sampled_at_ms,
                    gpus: if gpu {
                        metrics.gpus.clone()
                    } else {
                        Vec::new()
                    },
                    network_down_bytes_per_sec: metrics.network_down_bytes_per_sec,
                    network_up_bytes_per_sec: metrics.network_up_bytes_per_sec,
                    ..Default::default()
                }
            };
            if !gpu {
                outgoing.gpus.clear();
            }
            if !network {
                outgoing.network_up_bytes_per_sec = None;
                outgoing.network_down_bytes_per_sec = None;
            }
            let _ = channel
                .sender
                .try_send(WireMessage::MetricsUpdate { metrics: outgoing });
        }
    }

    #[cfg(test)]
    pub fn add_peer_channel(&self, node_id: String, channel: mpsc::Sender<WireMessage>) -> bool {
        let mut channels = self.peer_channels.lock();
        if channels.contains_key(&node_id) {
            return false;
        }
        channels.insert(node_id, PeerChannel::new(channel));
        true
    }

    pub fn has_peer_channel(&self, node_id: &str) -> bool {
        self.peer_channels.lock().contains_key(node_id)
    }

    pub(crate) fn attach_peer_session(
        &self,
        peer: PeerInfo,
        channel: mpsc::Sender<WireMessage>,
    ) -> Option<PeerSession<'_>> {
        let node_id = peer.node_id.clone();
        {
            let mut peers = self.peers.write();
            let mut channels = self.peer_channels.lock();
            if channels.contains_key(&node_id) {
                return None;
            }
            channels.insert(node_id.clone(), PeerChannel::new(channel));
            peers.insert(node_id.clone(), peer);
        }
        self.publish_event(WireMessage::Hello {
            node: self.node_info(),
        });
        Some(PeerSession {
            runtime: self,
            node_id,
        })
    }

    pub fn send_to_peer(&self, node_id: &str, message: WireMessage) -> bool {
        let is_clipboard = is_clipboard_message(&message);
        if is_clipboard && !self.peer_supports_clipboard(node_id) {
            self.pending_clipboard_resync.lock().remove(node_id);
            return true;
        }
        let channel = self.peer_sender(node_id);
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
        let peers: Vec<_> = self.peer_channels.lock().keys().cloned().collect();
        for peer in peers {
            if Some(peer.as_str()) != except {
                self.send_to_peer(&peer, message.clone());
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
        let Some(channel) = self.peer_sender(node_id) else {
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
            #[cfg(not(target_os = "android"))]
            if previous.online != peer.online
                || previous
                    .terminal
                    .as_ref()
                    .map(|keys| &keys.client_fingerprint)
                    != peer.terminal.as_ref().map(|keys| &keys.client_fingerprint)
            {
                if let Some(channel) = self.peer_channels.lock().get_mut(&peer.node_id) {
                    // 更换守卫会撤销旧指纹下的全部入站终端，即使下一条 Hello 又换回原指纹。
                    channel.terminal_client = CancellationToken::new().drop_guard();
                }
            }
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

    pub(super) fn remove_peer(&self, node_id: &str) {
        let mut peers = self.peers.write();
        let mut channels = self.peer_channels.lock();
        self.pending_clipboard_resync.lock().remove(node_id);
        channels.remove(node_id);
        peers.remove(node_id);
    }

    pub fn touch_peer(&self, node_id: &str) {
        if let Some(peer) = self.peers.write().get_mut(node_id) {
            peer.online = true;
            peer.last_seen_ms = now_millis();
        }
    }

    fn peer_sender(&self, node_id: &str) -> Option<mpsc::Sender<WireMessage>> {
        self.peer_channels
            .lock()
            .get(node_id)
            .map(|channel| channel.sender.clone())
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn authorize_terminal_client(
        &self,
        node_id: &str,
        fingerprint: &str,
    ) -> Option<CancellationToken> {
        let peers = self.peers.read();
        let peer = peers.get(node_id)?;
        if !peer.online || peer.terminal.as_ref()?.client_fingerprint != fingerprint {
            return None;
        }
        let channels = self.peer_channels.lock();
        Some(channels.get(node_id)?.terminal_client.token().clone())
    }

    #[cfg(feature = "gui")]
    pub(crate) fn terminal_peer_keys(&self, node_id: &str) -> Option<crate::model::TerminalKeys> {
        let peers = self.peers.read();
        let peer = peers.get(node_id)?;
        if !peer.online || !self.has_peer_channel(node_id) {
            return None;
        }
        peer.terminal.clone()
    }

    pub fn peer_supports(&self, node_id: &str, capability: &str) -> bool {
        self.peers
            .read()
            .get(node_id)
            .is_some_and(|peer| peer.capabilities.iter().any(|item| item == capability))
    }

    pub fn peer_supports_clipboard(&self, node_id: &str) -> bool {
        self.peers.read().get(node_id).is_some_and(|peer| {
            peer.capabilities
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
}
