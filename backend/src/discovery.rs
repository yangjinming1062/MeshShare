use crate::model::{NodeInfo, PeerInfo, WireMessage, PROTOCOL_VERSION};
use crate::runtime::NodeRuntime;
use crate::server;
use futures_util::{SinkExt, StreamExt};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use uuid::Uuid;

const SERVICE_TYPE: &str = "_meshshare._tcp.local.";

pub fn start(runtime: Arc<NodeRuntime>) {
    let handle = tokio::runtime::Handle::current();
    thread::Builder::new()
        .name("meshshare-mdns".to_string())
        .spawn(move || {
            let mut observed_addresses = crate::runtime::bind_addresses();
            let mut observed_network_rebind_generation = runtime.network_rebind_generation();
            while !runtime.stopping.load(std::sync::atomic::Ordering::SeqCst) {
                let Ok(daemon) = ServiceDaemon::new() else {
                    thread::sleep(Duration::from_secs(1));
                    continue;
                };
                let node = runtime.node_info();
                let mut registered_address = register_service(&daemon, &node);
                let Ok(receiver) = daemon.browse(SERVICE_TYPE) else {
                    if let Ok(status) = daemon.shutdown() {
                        let _ = status.recv_timeout(Duration::from_secs(1));
                    }
                    thread::sleep(Duration::from_secs(1));
                    continue;
                };
                let mut restart = false;
                while !runtime.stopping.load(std::sync::atomic::Ordering::SeqCst) {
                    // mdns-sd owns one socket per interface. Recreate the daemon
                    // after a VPN/Wi-Fi change so its existing physical socket is
                    // not left on the old process network.
                    let current_addresses = crate::runtime::bind_addresses();
                    let current_network_rebind_generation = runtime.network_rebind_generation();
                    if current_addresses != observed_addresses
                        || current_network_rebind_generation != observed_network_rebind_generation
                    {
                        restart = true;
                        break;
                    }
                    let event = receiver.recv_timeout(Duration::from_secs(1));
                    let current_node = runtime.node_info();
                    let current_address = service_address(&current_node);
                    if current_address != registered_address {
                        if registered_address.is_some() {
                            let fullname = format!("{}{}", current_node.node_id, SERVICE_TYPE);
                            let _ = daemon.unregister(&fullname);
                        }
                        registered_address = register_service(&daemon, &current_node);
                    }
                    if let Ok(ServiceEvent::ServiceResolved(info)) = event {
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
                        // mDNS visibility can be asymmetric across Wi-Fi, VPN and guest networks.
                        // Let either side dial; add_peer_channel rejects the duplicate session so
                        // simultaneous discovery still settles on one stable WebSocket channel.
                        if node_id == node.node_id {
                            continue;
                        }
                        let Some(address) = info.get_addresses().iter().find_map(|address| {
                            let IpAddr::V4(address) = address else {
                                return None;
                            };
                            is_allowed_ipv4(*address).then_some(*address)
                        }) else {
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
                if let Ok(status) = daemon.shutdown() {
                    let _ = status.recv_timeout(Duration::from_secs(1));
                }
                if !restart {
                    break;
                }
                observed_addresses = crate::runtime::bind_addresses();
                observed_network_rebind_generation = runtime.network_rebind_generation();
                thread::sleep(Duration::from_millis(100));
            }
        })
        .expect("mDNS discovery thread");
}

fn service_address(node: &NodeInfo) -> Option<String> {
    node.addresses
        .first()
        .map(|address| address.address.clone())
}

fn register_service(daemon: &ServiceDaemon, node: &NodeInfo) -> Option<String> {
    let address = service_address(node)?;
    let host = format!("{}.local.", node.node_id.replace('-', ""));
    let protocol = PROTOCOL_VERSION.to_string();
    let features = node.capabilities.join(",");
    let properties = [
        ("node_id", node.node_id.as_str()),
        ("node_name", node.node_name.as_str()),
        ("protocol", protocol.as_str()),
        ("features", features.as_str()),
    ];
    let info = ServiceInfo::new(
        SERVICE_TYPE,
        &node.node_id,
        &host,
        address.as_str(),
        node.port,
        &properties[..],
    )
    .ok()?;
    daemon.register(info).ok()?;
    Some(address)
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
    let mut peer = PeerInfo {
        node_id: node.node_id.clone(),
        node_name: node.node_name.clone(),
        platform: node.platform.clone(),
        addresses: node.addresses.clone(),
        mobile_addresses: node.mobile_addresses.clone(),
        capabilities: node.capabilities.clone(),
        online: true,
        last_seen_ms: crate::clock::now_millis(),
        endpoint: Some(endpoint.clone()),
        source: "manual".to_string(),
        metrics: None,
        metrics_received_at_ms: None,
        metrics_stale: false,
    };
    peer.addresses.retain(|address| {
        address
            .address
            .parse::<IpAddr>()
            .ok()
            .is_some_and(|address| match address {
                IpAddr::V4(address) => is_allowed_ipv4(address),
                IpAddr::V6(_) => false,
            })
    });
    peer.mobile_addresses.retain(|address| {
        address
            .url
            .starts_with(&format!("https://{}:", address.address))
            && address
                .address
                .parse::<IpAddr>()
                .ok()
                .is_some_and(|address| match address {
                    IpAddr::V4(address) => is_allowed_ipv4(address),
                    IpAddr::V6(_) => false,
                })
    });
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
    let parsed = reqwest::Url::parse(&base_url)
        .map_err(|_| "节点地址必须使用有效的 http:// 或 https:// 地址".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err("节点地址必须使用 http:// 或 https://".to_string());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("节点地址不能包含用户名或密码".to_string());
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err("节点地址不能包含查询参数或片段".to_string());
    }
    let Some(host) = parsed.host_str() else {
        return Err("节点地址缺少主机名".to_string());
    };
    if !is_allowed_manual_host(host) {
        return Err("节点地址必须是本机、局域网、链路本地或 Tailscale 地址".to_string());
    }
    let path = parsed.path().trim_end_matches('/');
    if !path.is_empty() && path != "/mobile" {
        return Err("节点地址只能包含根路径或 /mobile".to_string());
    }
    let mut normalized = parsed;
    normalized.set_path("");
    Ok(normalized.to_string().trim_end_matches('/').to_string())
}

fn is_allowed_manual_host(host: &str) -> bool {
    let host = host.trim_end_matches('.');
    if host.eq_ignore_ascii_case("localhost") || host.ends_with(".local") {
        return true;
    }
    host.parse::<IpAddr>()
        .ok()
        .is_some_and(|address| match address {
            IpAddr::V4(address) => is_allowed_ipv4(address),
            IpAddr::V6(_) => false,
        })
}

fn is_allowed_ipv4(address: Ipv4Addr) -> bool {
    address.is_loopback()
        || address.is_private()
        || address.is_link_local()
        || (u32::from(address) & 0xffc0_0000) == 0x6440_0000
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
    if !runtime.begin_peer_connection(&node_id) {
        return;
    }
    loop {
        if runtime.stopping.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }
        let connected =
            match tokio::time::timeout(Duration::from_secs(5), connect_async(&endpoint)).await {
                Ok(Ok((socket, _))) => {
                    let Some(connected) =
                        run_peer_session(&runtime, &node_id, &node_name, &endpoint, socket).await
                    else {
                        // Another socket won the race between the connection
                        // guard and the handshake. Keep this discovery task
                        // alive as a watcher so that it can resume dialing if
                        // the winning channel later disappears. The winning
                        // socket registration clears the original guard;
                        // release any remaining marker before waiting so this
                        // watcher can resume dialing after that socket closes.
                        runtime.cancel_peer_connection(&node_id);
                        while !runtime.stopping.load(std::sync::atomic::Ordering::SeqCst)
                            && runtime.has_peer_channel(&node_id)
                        {
                            tokio::time::sleep(Duration::from_millis(500)).await;
                        }
                        if runtime.stopping.load(std::sync::atomic::Ordering::SeqCst)
                            || !runtime.begin_peer_connection(&node_id)
                        {
                            return;
                        }
                        attempt = 0;
                        continue;
                    };
                    connected
                }
                Ok(Err(_)) | Err(_) => false,
            };
        // Keep the connection slot reserved until this task exits.  mDNS may
        // resolve the same peer repeatedly while it is offline; releasing the
        // slot before sleeping would defeat the retry backoff.
        runtime.remove_peer_for_retry(&node_id);
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
    runtime.cancel_peer_connection(&node_id);
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
    // `add_peer_channel` clears the handshake guard. Reclaim it for the
    // dialer's entire session so mDNS cannot bypass reconnect backoff after a
    // disconnect.
    runtime.reserve_peer_connection(node_id);
    if !matches!(
        tokio::time::timeout(
            Duration::from_secs(10),
            writer.send(tokio_tungstenite::tungstenite::Message::Text(
                serde_json::to_string(&WireMessage::Hello {
                    node: runtime.node_info(),
                })
                .unwrap_or_default(),
            )),
        )
        .await,
        Ok(Ok(()))
    ) {
        return Some(false);
    }
    runtime.set_peer(PeerInfo {
        node_id: node_id.to_string(),
        node_name: node_name.to_string(),
        platform: "unknown".to_string(),
        addresses: Vec::new(),
        mobile_addresses: Vec::new(),
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
    let handshake_deadline = tokio::time::sleep(Duration::from_secs(10));
    tokio::pin!(handshake_deadline);
    loop {
        tokio::select! {
            _ = &mut handshake_deadline, if !received_hello => break,
            _ = tokio::time::sleep(Duration::from_millis(250)) => {
                if runtime.stopping.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
            }
            _ = heartbeat.tick() => {
                if last_pong.elapsed() > Duration::from_secs(30) {
                    break;
                }
                let body = serde_json::to_string(&WireMessage::Ping { at_ms: crate::clock::now_millis() }).unwrap_or_default();
                if !matches!(
                    tokio::time::timeout(
                        Duration::from_secs(10),
                        writer.send(tokio_tungstenite::tungstenite::Message::Text(body)),
                    )
                    .await,
                    Ok(Ok(()))
                ) { break; }
            }
            _ = clipboard_resync.tick() => {
                let _ = runtime.flush_clipboard_resync(node_id);
                if received_hello {
                    let _ = runtime.flush_history_deletions(node_id);
                }
            }
            outbound = rx.recv() => {
                let Some(message) = outbound else { break };
                let Ok(body) = serde_json::to_string(&message) else { break };
                if !matches!(
                    tokio::time::timeout(
                        Duration::from_secs(10),
                        writer.send(tokio_tungstenite::tungstenite::Message::Text(body)),
                    )
                    .await,
                    Ok(Ok(()))
                ) { break; }
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
                            let is_hello = matches!(&message, WireMessage::Hello { .. });
                            let handled =
                                server::handle_wire_message(message, runtime, Some(node_id)).await;
                            if is_hello && handled.is_none() {
                                break;
                            }
                            if is_hello && !received_hello {
                                received_hello = true;
                                if runtime.peer_supports_clipboard(node_id) {
                                    if let Some(snapshot) = runtime.clipboard_snapshot() {
                                        let _ = runtime
                                            .send_to_peer_with_retry(
                                                node_id,
                                                WireMessage::ClipboardSnapshot { snapshot },
                                            )
                                            .await;
                                    }
                                }
                                server::replay_active_offers(runtime, node_id).await;
                                // 对端离线期间排队的传输历史删除，在 Hello 之后补发。
                                let _ = runtime.flush_history_deletions(node_id);
                            }
                        }
                    }
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Ping(payload))) => {
                        if !matches!(
                            tokio::time::timeout(
                                Duration::from_secs(10),
                                writer.send(tokio_tungstenite::tungstenite::Message::Pong(payload)),
                            )
                            .await,
                            Ok(Ok(()))
                        ) { break; }
                    }
                    Some(Ok(tokio_tungstenite::tungstenite::Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
        }
    }
    Some(true)
}

#[cfg(test)]
mod tests {
    use super::{connect_peer, normalize_http_base_url, websocket_endpoint};
    use crate::runtime::{NodeProfile, NodeRuntime};
    use crate::server;
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    use uuid::Uuid;

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
        let base = normalize_http_base_url("https://100.64.0.2").expect("normalize https address");
        assert_eq!(
            websocket_endpoint(&base).expect("convert websocket address"),
            "wss://100.64.0.2/api/v1/ws"
        );
    }

    #[test]
    fn manual_address_rejects_non_http_schemes() {
        assert!(normalize_http_base_url("ws://192.168.1.10:37880").is_err());
        assert!(websocket_endpoint("ftp://192.168.1.10:37880").is_err());
        assert!(normalize_http_base_url("http://8.8.8.8").is_err());
        assert!(normalize_http_base_url("http://[::1]").is_err());
        assert!(normalize_http_base_url("http://meshshare.example.test").is_err());
    }

    #[tokio::test]
    async fn peer_reconnects_after_server_restart() {
        let server_data =
            std::env::temp_dir().join(format!("meshshare-reconnect-server-{}", Uuid::new_v4()));
        let client_data =
            std::env::temp_dir().join(format!("meshshare-reconnect-client-{}", Uuid::new_v4()));
        let static_root = server_data.join("web");
        std::fs::create_dir_all(&static_root).expect("create static directory");
        std::fs::write(static_root.join("index.html"), "<html></html>")
            .expect("write static index");
        // The reconnect assertion only needs the remote snapshot store.  Use
        // the view-only profile so this network test cannot write its fixture
        // text into the host system clipboard through the normal peer path.
        let server_runtime = NodeRuntime::new_with_profile(
            server_data.clone(),
            0,
            env!("CARGO_PKG_VERSION").into(),
            NodeProfile::Android,
        )
        .expect("create server runtime");
        let client_runtime =
            NodeRuntime::new(client_data.clone(), 0, env!("CARGO_PKG_VERSION").into())
                .expect("create client runtime");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind server listener");
        let address = listener.local_addr().expect("server address");
        let server_app = server::router(server_runtime.clone(), static_root.clone());
        let mut server_task = tokio::spawn(async move {
            axum::serve(listener, server_app)
                .await
                .expect("serve server router");
        });
        let peer_id = server_runtime.node_id();
        let client_task = tokio::spawn(connect_peer(
            client_runtime.clone(),
            peer_id.clone(),
            "server".to_string(),
            format!("ws://{address}/api/v1/ws"),
        ));

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if client_runtime
                    .peers
                    .read()
                    .get(&peer_id)
                    .is_some_and(|peer| peer.online)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("peer should connect");

        client_runtime
            .create_local_clipboard_named(
                Some("reconnect-state".to_string()),
                None,
                "client".to_string(),
            )
            .expect("create reconnect clipboard state");

        server_runtime.stopping.store(true, Ordering::SeqCst);
        server_task.abort();
        let _ = (&mut server_task).await;

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if client_runtime
                    .peers
                    .read()
                    .get(&peer_id)
                    .is_none_or(|peer| !peer.online)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("peer should be marked offline");

        server_runtime.stopping.store(false, Ordering::SeqCst);
        let listener = tokio::net::TcpListener::bind(address)
            .await
            .expect("rebind server listener");
        let server_app = server::router(server_runtime.clone(), static_root);
        server_task = tokio::spawn(async move {
            axum::serve(listener, server_app)
                .await
                .expect("serve restarted router");
        });

        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if client_runtime
                    .peers
                    .read()
                    .get(&peer_id)
                    .is_some_and(|peer| peer.online)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("peer should reconnect");

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if server_runtime
                    .clipboard_snapshot()
                    .is_some_and(|snapshot| snapshot.text.as_deref() == Some("reconnect-state"))
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("reconnect should exchange the latest clipboard snapshot");

        client_runtime.stopping.store(true, Ordering::SeqCst);
        server_runtime.stopping.store(true, Ordering::SeqCst);
        client_task.abort();
        server_task.abort();
        let _ = client_task.await;
        let _ = server_task.await;
        let _ = std::fs::remove_dir_all(server_data);
        let _ = std::fs::remove_dir_all(client_data);
    }
}
