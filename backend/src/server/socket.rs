use super::{validate_offer, AppState, MAX_WIRE_ERROR_LENGTH};
use crate::model::{
    AddressInfo, ClipboardEvent, HardwareMetrics, NodeInfo, PeerInfo, TransferAction,
    TransferStatus, WireMessage, CLIPBOARD_VIEW_CAPABILITY, METRICS_NETWORK_CAPABILITY,
    METRICS_SYSTEM_CAPABILITY,
};
use crate::runtime::{valid_history_delete_ids, NodeRuntime, PeerSession};
use axum::{
    extract::{
        ws::{Message, WebSocket},
        ConnectInfo, State, WebSocketUpgrade,
    },
    http::{header, HeaderMap},
    response::IntoResponse,
};
use futures_util::{stream::SplitSink, SinkExt, StreamExt};
use std::{
    collections::HashSet,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use tokio::sync::mpsc;

pub(super) const PEER_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_METRIC_GPU_COUNT: usize = 64;
const MAX_METRIC_GPU_NAME_LENGTH: usize = 256;
const MAX_METRIC_REASON_LENGTH: usize = 512;
const MAX_METRIC_CPU_MODEL_LENGTH: usize = 256;
const MAX_METRIC_DISK_COUNT: usize = 128;
const MAX_METRIC_DISK_NAME_LENGTH: usize = 256;
const MAX_METRIC_DISK_PATH_LENGTH: usize = 1_024;
const MAX_METRIC_FILESYSTEM_LENGTH: usize = 128;

async fn send_message(writer: &mut SplitSink<WebSocket, Message>, message: WireMessage) -> bool {
    let Ok(body) = serde_json::to_string(&message) else {
        return false;
    };
    matches!(
        tokio::time::timeout(Duration::from_secs(10), writer.send(Message::Text(body))).await,
        Ok(Ok(()))
    )
}

pub(super) async fn websocket(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    headers: HeaderMap,
    connect_info: Option<ConnectInfo<SocketAddr>>,
) -> impl IntoResponse {
    ws.max_message_size(32 * 1024 * 1024)
        .max_frame_size(32 * 1024 * 1024)
        .on_upgrade(move |socket| {
            handle_socket(
                socket,
                state.runtime,
                connect_info.map(|info| info.0),
                !headers.contains_key(header::ORIGIN),
            )
        })
}

async fn handle_socket(
    socket: WebSocket,
    runtime: Arc<NodeRuntime>,
    remote_address: Option<SocketAddr>,
    native_peer: bool,
) {
    let (mut sender, mut receiver) = socket.split();
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel::<WireMessage>(64);
    let mut events = runtime.subscribe();
    let mut host_changes = runtime.ssh.subscribe_host();
    let hello = WireMessage::Hello {
        node: runtime.node_info(),
    };
    if !send_message(&mut sender, hello).await {
        return;
    }
    let mut peer_session = None;
    let mut ui_client = false;
    let mut clipboard_resync = tokio::time::interval(Duration::from_secs(1));
    // 客户端必须尽快表明身份。未认证的连接一旦保留，
    // 接收任务与广播订阅就会一直挂着，
    // 而该连接永远不会成为可用的对端通道。
    let handshake_deadline = tokio::time::sleep(PEER_HANDSHAKE_TIMEOUT);
    tokio::pin!(handshake_deadline);

    loop {
        let peer_id = peer_session.as_ref().map(PeerSession::node_id);
        tokio::select! {
            _ = &mut handshake_deadline, if peer_id.is_none() && !ui_client => break,
            incoming = receiver.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        let Ok(message) = serde_json::from_str::<WireMessage>(&text) else {
                            break;
                        };
                        match message {
                            WireMessage::Ping { at_ms } if peer_id.is_none() => {
                                // UI 通道用 Ping 表明身份，并继续接收本地事件。
                                ui_client = true;
                                if !send_message(&mut sender, WireMessage::Pong { at_ms }).await {
                                    break;
                                }
                            }
                            WireMessage::Hello { .. } if !native_peer || ui_client => break,
                            WireMessage::Hello { node } if peer_id.is_none() => {
                                let Some(peer) = incoming_peer_from_hello(node, &runtime, remote_address) else {
                                    break;
                                };
                                let Some(session) = runtime.attach_peer_session(peer, outgoing_tx.clone()) else {
                                    break;
                                };
                                sync_peer_state(&runtime, session.node_id()).await;
                                peer_session = Some(session);
                            }
                            message => {
                                let is_hello = matches!(&message, WireMessage::Hello { .. });
                                if handle_wire_message(message, &runtime, peer_id).await.is_none() && is_hello {
                                    break;
                                }
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
            _ = host_changes.changed(), if peer_id.is_some() || ui_client => {
                if !send_message(&mut sender, WireMessage::Hello { node: runtime.node_info() }).await { break; }
            }
            outbound = outgoing_rx.recv() => {
                let Some(message) = outbound else { break };
                if !send_message(&mut sender, message).await { break; }
            }
            _ = clipboard_resync.tick() => {
                if runtime.stopping.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
                if let Some(peer_id) = peer_id {
                    if runtime
                        .peers
                        .read()
                        .get(peer_id)
                        .is_some_and(|peer| {
                            crate::clock::now_millis().saturating_sub(peer.last_seen_ms) > 30_000
                        })
                    {
                        break;
                    }
                    let _ = runtime.flush_clipboard_resync(peer_id);
                    let _ = runtime.flush_history_deletions(peer_id);
                }
            }
            event = events.recv() => {
                match event {
                    Ok(message) => {
                        if peer_id.is_some() {
                            continue;
                        }
                        if !send_message(&mut sender, message).await {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}

/// 接受远端剪贴板内容。能写系统剪贴板的节点同时向其他对端转发，只读节点仅更新本地快照。
fn accept_remote_clipboard(
    runtime: &Arc<NodeRuntime>,
    peer_id: &str,
    event: ClipboardEvent,
    source_name: String,
) {
    let can_write_system_clipboard = runtime.supports_capability("clipboard.text");
    if !can_write_system_clipboard && !runtime.supports_capability(CLIPBOARD_VIEW_CAPABILITY) {
        return;
    }
    if !runtime.accept_clipboard(event.clone(), source_name) {
        return;
    }
    runtime.publish_event(WireMessage::ClipboardUpdate {
        event: event.clone(),
    });
    if can_write_system_clipboard {
        runtime.send_to_peers_except(Some(peer_id), WireMessage::ClipboardUpdate { event });
    }
}

pub(super) fn incoming_peer_from_hello(
    node: NodeInfo,
    runtime: &NodeRuntime,
    remote_address: Option<SocketAddr>,
) -> Option<PeerInfo> {
    let endpoint = remote_address
        .filter(|address| is_allowed_peer_address(address.ip()))
        .map(|address| format!("ws://{}:{}/api/v1/ws", address.ip(), node.port));
    peer_from_hello(node, runtime, endpoint)
}

pub(crate) fn peer_from_hello(
    node: NodeInfo,
    runtime: &NodeRuntime,
    endpoint: Option<String>,
) -> Option<PeerInfo> {
    if !validate_node_info(&node)
        || node.node_id == runtime.node_id()
        || node.protocol_version != crate::model::PROTOCOL_VERSION
    {
        return None;
    }
    let mobile_addresses = node.mobile_addresses;
    let addresses = node
        .addresses
        .into_iter()
        .filter_map(|mut address| {
            is_allowed_advertised_address(&address).then(|| {
                address.url = format!("http://{}:{}", address.address, node.port);
                address
            })
        })
        .collect::<Vec<_>>();
    let endpoint = endpoint.or_else(|| {
        addresses
            .first()
            .map(|address| format!("ws://{}:{}/api/v1/ws", address.address, node.port))
    });
    Some(PeerInfo {
        node_id: node.node_id,
        node_name: node.node_name,
        platform: node.platform,
        addresses,
        mobile_addresses,
        capabilities: node.capabilities,
        terminal: node.terminal,
        online: true,
        last_seen_ms: crate::clock::now_millis(),
        endpoint,
        source: "websocket".to_string(),
        metrics: None,
        metrics_received_at_ms: None,
        metrics_stale: false,
    })
}

pub(crate) async fn sync_peer_state(runtime: &Arc<NodeRuntime>, node_id: &str) {
    if let Some(snapshot) = runtime.clipboard_snapshot() {
        let _ = runtime
            .send_to_peer_with_retry(node_id, WireMessage::ClipboardSnapshot { snapshot })
            .await;
    }
    replay_active_offers(runtime, node_id).await;
    let _ = runtime.flush_history_deletions(node_id);
}

pub(crate) async fn handle_wire_message(
    message: WireMessage,
    runtime: &Arc<NodeRuntime>,
    current_peer: Option<&str>,
) -> Option<String> {
    if let Some(peer_id) = current_peer {
        runtime.touch_peer(peer_id);
    }
    match message {
        WireMessage::Hello { node } => {
            if current_peer.map_or_else(
                || runtime.has_peer_channel(&node.node_id),
                |peer_id| peer_id != node.node_id,
            ) {
                return None;
            }
            let peer = peer_from_hello(node, runtime, None)?;
            let node_id = peer.node_id.clone();
            runtime.set_peer(peer);
            Some(node_id)
        }
        WireMessage::Ping { at_ms } => {
            if let Some(peer_id) = current_peer {
                let _ = runtime.send_to_peer(peer_id, WireMessage::Pong { at_ms });
            }
            current_peer.map(str::to_string)
        }
        WireMessage::Pong { .. } => current_peer.map(str::to_string),
        WireMessage::ClipboardUpdate { event } => {
            let peer_id = current_peer?;
            let source_name = runtime
                .peer_name(peer_id)
                .unwrap_or_else(|| "远端设备".to_string());
            accept_remote_clipboard(runtime, peer_id, event, source_name);
            Some(peer_id.to_string())
        }
        WireMessage::ClipboardSnapshot { snapshot } => {
            let peer_id = current_peer?;
            if snapshot.source_name.len() > 256 {
                return Some(peer_id.to_string());
            }
            let source_name = if snapshot.source_name.trim().is_empty() {
                runtime
                    .peer_name(peer_id)
                    .unwrap_or_else(|| "远端设备".to_string())
            } else {
                snapshot.source_name.clone()
            };
            let event = crate::model::ClipboardEvent {
                event_id: snapshot.event_id,
                origin_node_id: snapshot.source_node_id,
                clock: snapshot.clock,
                text: snapshot.text,
                image_png: snapshot.image_png,
                updated_at_ms: snapshot.updated_at_ms,
            };
            accept_remote_clipboard(runtime, peer_id, event, source_name);
            Some(peer_id.to_string())
        }
        WireMessage::TransferOffer { offer } => {
            let peer_id = current_peer?;
            if offer.source_node_id != peer_id
                || offer.target_node_id != runtime.node_id()
                || validate_offer(&offer, true).is_err()
            {
                return Some(peer_id.to_string());
            }
            let transfer_id = offer.transfer_id.clone();
            let added = runtime.add_transfer(offer);
            if let Some(record) = runtime.transfer(&transfer_id) {
                let progress = record.progress.clone();
                let _ = runtime
                    .send_to_peer_with_retry(
                        peer_id,
                        WireMessage::TransferProgress {
                            progress: progress.clone(),
                        },
                    )
                    .await;
                if added
                    && matches!(
                        progress.status,
                        TransferStatus::Accepted | TransferStatus::Uploading
                    )
                {
                    let _ = runtime
                        .send_to_peer_with_retry(
                            peer_id,
                            WireMessage::TransferAction {
                                transfer_id,
                                action: TransferAction::Accept,
                                reason: None,
                                retry_count: progress.retry_count,
                            },
                        )
                        .await;
                }
            }
            Some(peer_id.to_string())
        }
        WireMessage::TransferAction {
            transfer_id,
            action,
            reason,
            retry_count,
        } => {
            if reason
                .as_ref()
                .is_some_and(|value| value.len() > MAX_WIRE_ERROR_LENGTH)
            {
                return current_peer.map(str::to_string);
            }
            apply_transfer_action(
                runtime,
                current_peer?,
                &transfer_id,
                action,
                reason,
                retry_count,
            );
            current_peer.map(str::to_string)
        }
        WireMessage::TransferProgress { progress } => {
            let peer = current_peer?;
            runtime.apply_peer_progress(peer, progress, None);
            Some(peer.to_string())
        }
        WireMessage::TransferHistoryDeleted { transfer_ids } => {
            let peer_id = current_peer?;
            if valid_history_delete_ids(&transfer_ids) {
                let mut seen = HashSet::new();
                let ids = transfer_ids
                    .into_iter()
                    .filter(|id| seen.insert(id.clone()))
                    .collect::<Vec<_>>();
                if runtime.forget_transfer_history_from_peer(peer_id, &ids) > 0 {
                    // 界面连接会收到这条事件并立即重新读取历史。
                    runtime
                        .publish_event(WireMessage::TransferHistoryDeleted { transfer_ids: ids });
                }
            }
            Some(peer_id.to_string())
        }
        WireMessage::MetricsUpdate { metrics } => {
            let peer_id = current_peer?;
            if validate_metrics(&metrics)
                && (runtime.peer_supports(peer_id, METRICS_SYSTEM_CAPABILITY)
                    || runtime.peer_supports(peer_id, METRICS_NETWORK_CAPABILITY))
                && runtime.update_peer_metrics(peer_id, metrics.clone())
            {
                runtime.publish_event(WireMessage::MetricsUpdate { metrics });
            }
            Some(peer_id.to_string())
        }
    }
}

async fn replay_active_offers(runtime: &Arc<NodeRuntime>, peer_id: &str) {
    for progress in runtime.transfers_snapshot() {
        let Some(offer) = progress.offer else {
            continue;
        };
        if offer.source_node_id != runtime.node_id()
            || offer.target_node_id != peer_id
            || !matches!(
                progress.status,
                TransferStatus::Waiting | TransferStatus::Accepted | TransferStatus::Uploading
            )
        {
            continue;
        }
        let _ = runtime
            .send_to_peer_with_retry(peer_id, WireMessage::TransferOffer { offer })
            .await;
    }
}

pub(super) fn validate_metrics(metrics: &HardwareMetrics) -> bool {
    !metrics.node_id.trim().is_empty()
        && metrics.node_id.len() <= 256
        && metrics.cpu_model.as_ref().is_none_or(|value| {
            !value.trim().is_empty() && value.len() <= MAX_METRIC_CPU_MODEL_LENGTH
        })
        && metrics
            .cpu_logical_cores
            .is_none_or(|value| (1..=4096).contains(&value))
        && metrics
            .cpu_physical_cores
            .is_none_or(|value| (1..=4096).contains(&value))
        && metrics
            .cpu_frequency_mhz
            .is_none_or(|value| (1..=100_000).contains(&value))
        && metrics.memory_available_bytes.is_none_or(|value| {
            metrics
                .memory_total_bytes
                .is_none_or(|total| value <= total)
        })
        && metrics
            .swap_used_bytes
            .is_none_or(|value| metrics.swap_total_bytes.is_none_or(|total| value <= total))
        && metrics.load_average.as_ref().is_none_or(|value| {
            [value.one, value.five, value.fifteen]
                .iter()
                .all(|item| item.is_finite() && *item >= 0.0)
        })
        && metrics.gpus.len() <= MAX_METRIC_GPU_COUNT
        && metrics.gpus.iter().all(|gpu| {
            !gpu.name.trim().is_empty()
                && gpu.name.len() <= MAX_METRIC_GPU_NAME_LENGTH
                && gpu
                    .unavailable_reason
                    .as_ref()
                    .is_none_or(|value| value.len() <= MAX_METRIC_REASON_LENGTH)
                && gpu
                    .utilization_percent
                    .is_none_or(|value| value.is_finite() && (0.0..=100.0).contains(&value))
        })
        && metrics.disks.len() <= MAX_METRIC_DISK_COUNT
        && metrics.disks.iter().all(|disk| {
            !disk.name.trim().is_empty()
                && disk.name.len() <= MAX_METRIC_DISK_NAME_LENGTH
                && disk.mount_point.len() <= MAX_METRIC_DISK_PATH_LENGTH
                && disk.file_system.len() <= MAX_METRIC_FILESYSTEM_LENGTH
                && disk.available_bytes <= disk.total_bytes
        })
}

fn valid_terminal_keys(node: &NodeInfo) -> bool {
    let offers_terminal = node
        .capabilities
        .iter()
        .any(|cap| cap == crate::model::TERMINAL_SSH_CAPABILITY);
    let Some(keys) = &node.terminal else {
        return !offers_terminal;
    };
    let valid_pin = |pin: &str| {
        pin.len() == 50
            && pin
                .parse::<russh::keys::ssh_key::Fingerprint>()
                .is_ok_and(|fp| fp.algorithm() == russh::keys::HashAlg::Sha256)
    };
    !node.node_id.chars().any(char::is_control)
        && valid_pin(&keys.client_fingerprint)
        && offers_terminal == keys.host_fingerprint.is_some()
        && keys
            .host_fingerprint
            .as_deref()
            .is_none_or(|pin| node.platform != "android" && valid_pin(pin))
}

pub(crate) fn validate_node_info(node: &NodeInfo) -> bool {
    !node.node_id.trim().is_empty()
        && node.node_id.len() <= 256
        && node.node_name.len() <= 256
        && node.version.len() <= 64
        && node.platform.len() <= 64
        && node.addresses.len() <= 32
        && node.mobile_addresses.len() <= 32
        && node.capabilities.len() <= 64
        && valid_terminal_keys(node)
        && node.addresses.iter().all(|address| {
            address.address.len() <= 64
                && address.url.len() <= 512
                && address.label.len() <= 256
                && address.kind.len() <= 64
        })
        && node.mobile_addresses.iter().all(|address| {
            address.address.len() <= 64
                && address.url.len() <= 512
                && address.label.len() <= 256
                && address.kind.len() <= 64
                && is_allowed_advertised_address(address)
                && address
                    .url
                    .starts_with(&format!("https://{}:", address.address))
        })
        && node
            .capabilities
            .iter()
            .all(|capability| !capability.is_empty() && capability.len() <= 128)
}

fn is_allowed_advertised_address(address: &AddressInfo) -> bool {
    address.address.parse().is_ok_and(is_allowed_peer_address)
}

fn is_allowed_peer_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => address.is_loopback() || crate::runtime::is_network_ipv4(address),
        IpAddr::V6(_) => false,
    }
}

fn apply_transfer_action(
    runtime: &NodeRuntime,
    peer_id: &str,
    transfer_id: &str,
    action: TransferAction,
    reason: Option<String>,
    retry_count: u32,
) {
    let Some((source, target)) = runtime.transfer_peer_ids(transfer_id) else {
        return;
    };
    if peer_id != source && peer_id != target {
        return;
    }
    let progress = match action {
        TransferAction::Accept if peer_id == target => {
            runtime.mark_transfer_accepted(transfer_id, retry_count)
        }
        TransferAction::Accept => return,
        terminal => {
            let message = reason.unwrap_or_else(|| {
                match terminal {
                    TransferAction::Reject => "接收端拒绝了传输",
                    TransferAction::Cancel => "传输已取消",
                    _ => "文件上传失败，已停止重试",
                }
                .into()
            });
            runtime.fail_transfer_at_generation(
                transfer_id,
                message,
                terminal.status(),
                Some(retry_count),
            )
        }
    };
    if let Some(progress) = progress {
        runtime.publish_event(WireMessage::TransferProgress { progress });
    }
}
