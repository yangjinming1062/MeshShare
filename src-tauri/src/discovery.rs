use crate::model::{PeerInfo, WireMessage, PROTOCOL_VERSION};
use crate::runtime::NodeRuntime;
use crate::server;
use futures_util::{SinkExt, StreamExt};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use uuid::Uuid;

const SERVICE_TYPE: &str = "_meshshare._tcp.local.";

pub fn start(runtime: Arc<NodeRuntime>) {
    let handle = tokio::runtime::Handle::current();
    let node = runtime.node_info();
    thread::Builder::new()
        .name("meshshare-mdns".to_string())
        .spawn(move || {
            let Ok(daemon) = ServiceDaemon::new() else {
                return;
            };
            let host = format!("{}.local.", node.node_id.replace('-', ""));
            let address = node
                .addresses
                .first()
                .map(|item| item.address.clone())
                .unwrap_or_else(|| "127.0.0.1".to_string());
            let protocol = PROTOCOL_VERSION.to_string();
            let properties = [
                ("node_id", node.node_id.as_str()),
                ("node_name", node.node_name.as_str()),
                ("protocol", protocol.as_str()),
                ("features", "clipboard,transfer,web,metrics"),
            ];
            if let Ok(info) = ServiceInfo::new(
                SERVICE_TYPE,
                &node.node_id,
                &host,
                address.as_str(),
                node.port,
                &properties[..],
            ) {
                let _ = daemon.register(info);
            }
            let Ok(receiver) = daemon.browse(SERVICE_TYPE) else {
                return;
            };
            while !runtime.stopping.load(std::sync::atomic::Ordering::SeqCst) {
                let Ok(event) = receiver.recv() else { break };
                if let ServiceEvent::ServiceResolved(info) = event {
                    let properties = info.get_properties();
                    let Some(node_id) = properties.iter().find_map(|item| {
                        (item.key() == "node_id").then(|| item.val_str().to_string())
                    }) else {
                        continue;
                    };
                    let protocol = properties
                        .iter()
                        .find_map(|item| (item.key() == "protocol").then(|| item.val_str()))
                        .and_then(|value| value.parse::<u32>().ok());
                    if protocol != Some(PROTOCOL_VERSION) {
                        continue;
                    }
                    if node_id == node.node_id || node.node_id.as_str() > node_id.as_str() {
                        continue;
                    }
                    let Some(address) = info.get_addresses().iter().next() else {
                        continue;
                    };
                    let endpoint = format!("ws://{}:{}/api/v1/ws", address, info.get_port());
                    let remote_name = properties
                        .iter()
                        .find_map(|item| {
                            (item.key() == "node_name").then(|| item.val_str().to_string())
                        })
                        .unwrap_or_else(|| node_id.clone());
                    let runtime = runtime.clone();
                    handle.spawn(async move {
                        connect_peer(runtime, node_id, remote_name, endpoint).await;
                    });
                }
            }
            let _ = daemon.shutdown();
        })
        .expect("mDNS discovery thread");
}

pub async fn connect_manual(
    runtime: Arc<NodeRuntime>,
    base_url: String,
) -> Result<PeerInfo, String> {
    let base_url = normalize_http_base_url(&base_url)?;
    let info_url = format!("{base_url}/api/v1/info");
    let node: crate::model::NodeInfo = runtime
        .http
        .get(info_url)
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?
        .json()
        .await
        .map_err(|error| error.to_string())?;
    if node.node_id == runtime.node_id() {
        return Err("不能连接本机地址".to_string());
    }
    if node.protocol_version != PROTOCOL_VERSION {
        return Err(format!(
            "协议版本不兼容：对端 {}，本机 {}",
            node.protocol_version, PROTOCOL_VERSION
        ));
    }
    let endpoint = websocket_endpoint(&base_url)?;
    let peer = PeerInfo {
        node_id: node.node_id.clone(),
        node_name: node.node_name.clone(),
        platform: node.platform.clone(),
        addresses: node.addresses.clone(),
        capabilities: node.capabilities.clone(),
        online: true,
        last_seen_ms: crate::clock::now_millis(),
        endpoint: Some(endpoint.clone()),
        source: "manual".to_string(),
        metrics: None,
        metrics_received_at_ms: None,
        metrics_stale: false,
    };
    runtime.set_peer(peer.clone());
    let runtime_for_task = runtime.clone();
    tokio::spawn(async move {
        connect_peer(runtime_for_task, node.node_id, node.node_name, endpoint).await;
    });
    Ok(peer)
}

fn normalize_http_base_url(value: &str) -> Result<String, String> {
    let base_url = value
        .trim()
        .trim_end_matches('/')
        .trim_end_matches("/mobile")
        .to_string();
    let Some((scheme, _)) = base_url.split_once("://") else {
        return Err("节点地址必须使用 http:// 或 https://".to_string());
    };
    if !matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https") {
        return Err("节点地址必须使用 http:// 或 https://".to_string());
    }
    Ok(base_url)
}

fn websocket_endpoint(base_url: &str) -> Result<String, String> {
    let Some((scheme, rest)) = base_url.split_once("://") else {
        return Err("节点地址必须使用 http:// 或 https://".to_string());
    };
    let websocket_scheme = match scheme.to_ascii_lowercase().as_str() {
        "http" => "ws",
        "https" => "wss",
        _ => return Err("节点地址必须使用 http:// 或 https://".to_string()),
    };
    if rest.is_empty() {
        return Err("节点地址缺少主机名".to_string());
    }
    Ok(format!("{websocket_scheme}://{rest}/api/v1/ws"))
}

pub(crate) async fn connect_peer(
    runtime: Arc<NodeRuntime>,
    node_id: String,
    node_name: String,
    endpoint: String,
) {
    let mut attempt = 0u32;
    loop {
        if runtime.stopping.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }
        if !runtime.begin_peer_connection(&node_id) {
            return;
        }
        let connected = match connect_async(&endpoint).await {
            Ok((socket, _)) => {
                let Some(connected) =
                    run_peer_session(&runtime, &node_id, &node_name, &endpoint, socket).await
                else {
                    return;
                };
                connected
            }
            Err(_) => false,
        };
        runtime.remove_peer(&node_id);
        if connected {
            attempt = 0;
        } else {
            attempt = attempt.saturating_add(1);
        }
        if runtime.stopping.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }
        let backoff = 1u64 << attempt.min(5);
        let jitter = (Uuid::new_v4().as_u128() % 400) as u64;
        tokio::time::sleep(Duration::from_millis(backoff * 1_000 + jitter)).await;
    }
}

async fn run_peer_session(
    runtime: &Arc<NodeRuntime>,
    node_id: &str,
    node_name: &str,
    endpoint: &str,
    socket: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> Option<bool> {
    let (mut writer, mut reader) = socket.split();
    let (tx, mut rx) = mpsc::channel::<WireMessage>(64);
    if !runtime.add_peer_channel(node_id.to_string(), tx) {
        return None;
    }
    let _ = writer
        .send(tokio_tungstenite::tungstenite::Message::Text(
            serde_json::to_string(&WireMessage::Hello {
                node: runtime.node_info(),
            })
            .unwrap_or_default(),
        ))
        .await;
    runtime.set_peer(PeerInfo {
        node_id: node_id.to_string(),
        node_name: node_name.to_string(),
        platform: "unknown".to_string(),
        addresses: Vec::new(),
        capabilities: Vec::new(),
        online: true,
        last_seen_ms: crate::clock::now_millis(),
        endpoint: Some(endpoint.to_string()),
        source: "mdns".to_string(),
        metrics: None,
        metrics_received_at_ms: None,
        metrics_stale: false,
    });
    let mut heartbeat = tokio::time::interval(Duration::from_secs(10));
    let mut clipboard_resync = tokio::time::interval(Duration::from_secs(1));
    let mut last_pong = tokio::time::Instant::now();
    let mut received_hello = false;
    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                if last_pong.elapsed() > Duration::from_secs(30) {
                    break;
                }
                let body = serde_json::to_string(&WireMessage::Ping { at_ms: crate::clock::now_millis() }).unwrap_or_default();
                if writer.send(tokio_tungstenite::tungstenite::Message::Text(body)).await.is_err() { break; }
            }
            _ = clipboard_resync.tick() => {
                let _ = runtime.flush_clipboard_resync(node_id);
            }
            outbound = rx.recv() => {
                let Some(message) = outbound else { break };
                let Ok(body) = serde_json::to_string(&message) else { break };
                if writer.send(tokio_tungstenite::tungstenite::Message::Text(body)).await.is_err() { break; }
            }
            inbound = reader.next() => {
                match inbound {
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Text(body))) => {
                        if let Ok(message) = serde_json::from_str::<WireMessage>(&body) {
                            if let WireMessage::Hello { node } = &message {
                                if node.node_id != node_id {
                                    break;
                                }
                            }
                            if matches!(message, WireMessage::Pong { .. }) { last_pong = tokio::time::Instant::now(); }
                            let is_hello = matches!(message, WireMessage::Hello { .. });
                            let _ = server::handle_wire_message(message, runtime, Some(node_id)).await;
                            if is_hello && !received_hello {
                                received_hello = true;
                                if let Some(snapshot) = runtime.clipboard_snapshot() {
                                    let _ = runtime.send_to_peer(node_id, WireMessage::ClipboardSnapshot { snapshot });
                                }
                            }
                        }
                    }
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Ping(payload))) => {
                        if writer.send(tokio_tungstenite::tungstenite::Message::Pong(payload)).await.is_err() { break; }
                    }
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
        }
    }
    runtime.remove_peer(node_id);
    Some(true)
}

#[cfg(test)]
mod tests {
    use super::{normalize_http_base_url, websocket_endpoint};

    #[test]
    fn manual_http_address_becomes_ws_endpoint() {
        let base = normalize_http_base_url(" http://192.168.1.10:37880/mobile/ ")
            .expect("normalize http address");
        assert_eq!(base, "http://192.168.1.10:37880");
        assert_eq!(
            websocket_endpoint(&base).expect("convert websocket address"),
            "ws://192.168.1.10:37880/api/v1/ws"
        );
    }

    #[test]
    fn manual_https_address_becomes_wss_endpoint() {
        let base = normalize_http_base_url("https://meshshare.example.test")
            .expect("normalize https address");
        assert_eq!(
            websocket_endpoint(&base).expect("convert websocket address"),
            "wss://meshshare.example.test/api/v1/ws"
        );
    }

    #[test]
    fn manual_address_rejects_non_http_schemes() {
        assert!(normalize_http_base_url("ws://192.168.1.10:37880").is_err());
        assert!(websocket_endpoint("ftp://192.168.1.10:37880").is_err());
    }
}
