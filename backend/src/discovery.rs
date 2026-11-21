use crate::model::{NodeInfo, PeerInfo, WireMessage, PROTOCOL_VERSION};
use crate::runtime::{NodeRuntime, PeerEndpoint, PeerEndpointSource};
use crate::server;
use futures_util::{SinkExt, StreamExt};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tokio::runtime::Handle;
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::{connect_async, tungstenite::Message, MaybeTlsStream, WebSocketStream};
use uuid::Uuid;

#[cfg(not(target_os = "android"))]
mod tailscale;

type PeerSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

const SERVICE_TYPE: &str = "_meshshare._tcp.local.";

pub fn start(runtime: Arc<NodeRuntime>) {
    let handle = Handle::current();
    #[cfg(not(target_os = "android"))]
    handle.spawn(tailscale::discover(runtime.clone()));
    thread::Builder::new()
        .name("meshshare-mdns".to_string())
        .spawn(move || {
            let mut observed_addresses = crate::runtime::bind_addresses();
            let mut observed_network_rebind_generation = runtime.network_rebind_generation();
            while !runtime.stopping.load(Ordering::SeqCst) {
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
                while !runtime.stopping.load(Ordering::SeqCst) {
                    // mdns-sd 为每个接口持有一个 socket。
                    // 网络变化（VPN/Wi-Fi）后重建守护进程，
                    // 避免它原有的物理 socket 留在旧的进程网络绑定上。
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
                        // 地址变化直接重新注册：mdns-sd 对同一实例名做更新，
                        // 且 fullname 由 "<实例>.<服务类型>" 组成，手动拼接容易漏分隔点。
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
                        // 在 Wi-Fi、VPN 与访客网络之间，mDNS 可见性可能是不对称的。
                        // 因此允许任意一方发起拨号；add_peer_channel 会拒绝重复会话，
                        // 同时发现也能收敛到一条稳定的 WebSocket 通道。
                        if node_id == node.node_id
                            || node_id.trim().is_empty()
                            || node_id.len() > 256
                        {
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
                        queue_peer_connection(
                            &handle,
                            &runtime,
                            node_id,
                            PeerEndpoint {
                                source: PeerEndpointSource::Mdns,
                                url: endpoint,
                            },
                        );
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
    let mut peer = server::peer_from_hello(node, &runtime, Some(endpoint.clone()))
        .ok_or_else(|| "节点信息无效".to_string())?;
    peer.source = "manual".to_string();
    queue_peer_connection(
        &Handle::current(),
        &runtime,
        peer.node_id.clone(),
        PeerEndpoint {
            source: PeerEndpointSource::Manual,
            url: endpoint,
        },
    );
    Ok(runtime.peer(&peer.node_id).unwrap_or(peer))
}
fn normalize_http_base_url(value: &str) -> Result<String, String> {
    let candidate = value
        .trim()
        .trim_end_matches('/')
        .trim_end_matches("/mobile");
    let path = reqwest::Url::parse(candidate)
        .map_err(|_| "节点地址必须使用有效的 http:// 或 https:// 地址".to_string())?
        .path()
        .trim_end_matches('/')
        .to_string();
    if !path.is_empty() && path != "/mobile" {
        return Err("节点地址只能包含根路径或 /mobile".to_string());
    }
    crate::runtime::normalize_peer_http_base(candidate).ok_or_else(|| {
        "节点地址必须是不含凭据与查询参数的本机、局域网、链路本地或 Tailscale 地址".to_string()
    })
}

fn is_allowed_ipv4(address: Ipv4Addr) -> bool {
    address.is_loopback() || crate::runtime::is_network_ipv4(address)
}

fn websocket_endpoint(base_url: &str) -> Result<String, String> {
    let mut url = reqwest::Url::parse(base_url)
        .map_err(|_| "节点地址必须使用 http:// 或 https://".to_string())?;
    let scheme = match url.scheme() {
        "http" => "ws",
        "https" => "wss",
        _ => return Err("节点地址必须使用 http:// 或 https://".to_string()),
    };
    url.set_scheme(scheme)
        .map_err(|_| "节点地址必须使用 http:// 或 https://".to_string())?;
    url.set_path("/api/v1/ws");
    Ok(url.to_string())
}

fn queue_peer_connection(
    handle: &Handle,
    runtime: &Arc<NodeRuntime>,
    node_id: String,
    endpoint: PeerEndpoint,
) {
    if let Some(candidates) = runtime.offer_peer_endpoint(&node_id, endpoint) {
        handle.spawn(dial_peer(runtime.clone(), node_id, candidates));
    }
}

async fn dial_peer(
    runtime: Arc<NodeRuntime>,
    node_id: String,
    mut candidates: watch::Receiver<Vec<PeerEndpoint>>,
) {
    let mut failures = 0u32;
    let mut last_connected = None;
    let mut last_connected_mdns = None;
    loop {
        let mut endpoints = candidates.borrow_and_update().clone();
        // A new mDNS advertisement can replace a working address during an active session.
        endpoints.extend(last_connected_mdns.iter().cloned());
        if runtime.stopping.load(Ordering::SeqCst) || endpoints.is_empty() {
            if runtime.retire_peer_dialer(&node_id) {
                return;
            }
            continue;
        }
        if runtime.has_peer_channel(&node_id) {
            failures = 0;
            // An incoming session can win while discovery is in progress.
            tokio::select! {
                _ = candidates.changed() => {}
                _ = tokio::time::sleep(Duration::from_millis(500)) => {}
            }
            continue;
        }

        endpoints.sort_by_key(|endpoint| {
            (
                last_connected.as_ref() != Some(&endpoint.url),
                endpoint.source,
            )
        });
        let mut attempted = HashSet::new();
        let mut connected = false;
        for endpoint in endpoints {
            if runtime.stopping.load(Ordering::SeqCst) || runtime.has_peer_channel(&node_id) {
                break;
            }
            let is_candidate = || {
                last_connected_mdns.as_ref() == Some(&endpoint)
                    || candidates.borrow().contains(&endpoint)
            };
            if !is_candidate() || !attempted.insert(endpoint.url.clone()) {
                continue;
            }
            match tokio::time::timeout(Duration::from_secs(5), connect_async(&endpoint.url)).await {
                Ok(Ok((socket, _))) => {
                    if run_peer_session(&runtime, &node_id, &endpoint, is_candidate, socket).await {
                        if endpoint.source == PeerEndpointSource::Mdns {
                            last_connected_mdns = Some(endpoint.clone());
                        }
                        last_connected = Some(endpoint.url);
                        connected = true;
                        break;
                    }
                }
                Ok(Err(error)) if failures < 3 => eprintln!("连接 {} 失败：{error}", endpoint.url),
                Err(_) if failures < 3 => eprintln!("连接 {} 超时", endpoint.url),
                _ => {}
            }
        }
        failures = if connected {
            0
        } else {
            failures.saturating_add(1)
        };
        let backoff = (1u64 << failures.min(5)) * 1_000;
        let jitter = (Uuid::new_v4().as_u128() % 400) as u64;
        tokio::select! {
            _ = candidates.changed() => {}
            _ = tokio::time::sleep(Duration::from_millis(backoff + jitter)) => {}
        }
    }
}

async fn run_peer_session(
    runtime: &Arc<NodeRuntime>,
    node_id: &str,
    endpoint: &PeerEndpoint,
    is_candidate: impl FnOnce() -> bool,
    socket: PeerSocket,
) -> bool {
    let (mut writer, mut reader) = socket.split();
    let Ok(Some(Ok(Message::Text(body)))) =
        tokio::time::timeout(Duration::from_secs(10), reader.next()).await
    else {
        return false;
    };
    let Ok(WireMessage::Hello { node }) = serde_json::from_str(&body) else {
        return false;
    };
    // An mDNS dial can finish validation when a newer advertisement replaces its address.
    if node.node_id != node_id
        || runtime.stopping.load(Ordering::SeqCst)
        || (endpoint.source != PeerEndpointSource::Mdns && !is_candidate())
    {
        return false;
    }
    #[cfg(not(target_os = "android"))]
    if endpoint.source == PeerEndpointSource::Tailscale && !tailscale::is_desktop_node(&node) {
        return false;
    }
    let Some(peer) = server::peer_from_hello(node, runtime, Some(endpoint.url.clone())) else {
        return false;
    };
    let (tx, mut rx) = mpsc::channel::<WireMessage>(64);
    let Some(_session) = runtime.attach_peer_session(peer, tx) else {
        return false;
    };
    let hello = WireMessage::Hello {
        node: runtime.node_info(),
    };
    if !matches!(
        tokio::time::timeout(
            Duration::from_secs(10),
            writer.send(Message::Text(
                serde_json::to_string(&hello).unwrap_or_default()
            )),
        )
        .await,
        Ok(Ok(()))
    ) {
        return false;
    }
    server::sync_peer_state(runtime, node_id).await;

    let mut heartbeat = tokio::time::interval(Duration::from_secs(10));
    let mut clipboard_resync = tokio::time::interval(Duration::from_secs(1));
    let mut last_pong = tokio::time::Instant::now();
    while !runtime.stopping.load(Ordering::SeqCst) {
        tokio::select! {
            _ = heartbeat.tick() => {
                if last_pong.elapsed() > Duration::from_secs(30) {
                    break;
                }
                let body = serde_json::to_string(&WireMessage::Ping { at_ms: crate::clock::now_millis() }).unwrap_or_default();
                if !matches!(
                    tokio::time::timeout(Duration::from_secs(10), writer.send(Message::Text(body))).await,
                    Ok(Ok(()))
                ) { break; }
            }
            _ = clipboard_resync.tick() => {
                let _ = runtime.flush_clipboard_resync(node_id);
                let _ = runtime.flush_history_deletions(node_id);
            }
            outbound = rx.recv() => {
                let Some(message) = outbound else { break };
                let Ok(body) = serde_json::to_string(&message) else { break };
                if !matches!(
                    tokio::time::timeout(Duration::from_secs(10), writer.send(Message::Text(body))).await,
                    Ok(Ok(()))
                ) { break; }
            }
            inbound = reader.next() => {
                match inbound {
                    Some(Ok(Message::Text(body))) => {
                        if let Ok(message) = serde_json::from_str::<WireMessage>(&body) {
                            if matches!(message, WireMessage::Pong { .. }) {
                                last_pong = tokio::time::Instant::now();
                            }
                            let is_hello = matches!(&message, WireMessage::Hello { .. });
                            if server::handle_wire_message(message, runtime, Some(node_id)).await.is_none() && is_hello {
                                break;
                            }
                        }
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        if !matches!(
                            tokio::time::timeout(Duration::from_secs(10), writer.send(Message::Pong(payload))).await,
                            Ok(Ok(()))
                        ) { break; }
                    }
                    Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                    Some(Ok(_)) => {}
                }
            }
        }
    }
    true
}
#[cfg(test)]
mod tests {
    use super::{dial_peer, normalize_http_base_url, websocket_endpoint};
    use crate::model::WireMessage;
    use crate::runtime::{NodeProfile, NodeRuntime, PeerEndpoint, PeerEndpointSource};
    use crate::server;
    use futures_util::{SinkExt, StreamExt};
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    use tokio::net::TcpListener;
    use tokio::time::timeout;
    use tokio_tungstenite::{accept_async, tungstenite::Message};
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
    fn manual_address_accepts_trailing_dot_mdns_host() {
        let base = normalize_http_base_url("http://meshshare-device.local.:37880")
            .expect("normalize trailing-dot mdns host");
        assert_eq!(base, "http://meshshare-device.local.:37880");
        assert_eq!(
            websocket_endpoint(&base).expect("convert websocket address"),
            "ws://meshshare-device.local.:37880/api/v1/ws"
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
    async fn mdns_rediscovery_preserves_a_handshake_in_progress() {
        let data = std::env::temp_dir().join(format!("meshshare-mdns-hello-{}", Uuid::new_v4()));
        let runtime =
            NodeRuntime::new_with_profile(data.clone(), 0, "test".into(), NodeProfile::Android)
                .expect("create runtime");
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind peer");
        let address = listener.local_addr().expect("peer address");
        let unavailable = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind stale route");
        let stale_address = unavailable.local_addr().expect("stale address");
        let working_url = format!("ws://{address}/api/v1/ws");
        let mut peer = runtime.node_info();
        peer.node_id = Uuid::new_v4().to_string();
        peer.port = address.port();
        let peer_id = peer.node_id.clone();
        let candidates = runtime
            .offer_peer_endpoint(
                &peer_id,
                PeerEndpoint {
                    source: PeerEndpointSource::Mdns,
                    url: working_url.clone(),
                },
            )
            .expect("start dialer");
        let dialer = tokio::spawn(dial_peer(runtime.clone(), peer_id.clone(), candidates));
        let mut socket = timeout(Duration::from_secs(5), async {
            let (stream, _) = listener.accept().await.expect("accept TCP connection");
            accept_async(stream).await.expect("upgrade WebSocket")
        })
        .await
        .expect("connect to initial address");

        assert!(runtime
            .offer_peer_endpoint(
                &peer_id,
                PeerEndpoint {
                    source: PeerEndpointSource::Mdns,
                    url: format!("ws://{stale_address}/api/v1/ws"),
                },
            )
            .is_none());
        let hello =
            serde_json::to_string(&WireMessage::Hello { node: peer }).expect("encode hello");
        socket.send(Message::Text(hello)).await.expect("send hello");
        let reply = timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("dialer responds to hello");
        let Some(Ok(Message::Text(body))) = reply else {
            panic!("expected hello response, got {reply:?}");
        };
        let WireMessage::Hello { node } = serde_json::from_str(&body).expect("decode hello") else {
            panic!("expected a hello message");
        };
        assert_eq!(node.node_id, runtime.node_id());
        assert_eq!(
            runtime.peer(&peer_id).and_then(|peer| peer.endpoint),
            Some(working_url)
        );

        runtime.stopping.store(true, Ordering::SeqCst);
        dialer.abort();
        let _ = dialer.await;
        let _ = std::fs::remove_dir_all(data);
    }

    #[tokio::test]
    async fn peer_reconnects_to_last_working_mdns_address() {
        let server_data =
            std::env::temp_dir().join(format!("meshshare-reconnect-server-{}", Uuid::new_v4()));
        let client_data =
            std::env::temp_dir().join(format!("meshshare-reconnect-client-{}", Uuid::new_v4()));
        let static_root = server_data.join("web");
        std::fs::create_dir_all(&static_root).expect("create static directory");
        std::fs::write(static_root.join("index.html"), "<html></html>")
            .expect("write static index");
        // 重连断言只需要远端的快照存储。使用只读配置，
        // 该网络测试就无法通过正常对端路径
        // 把测试文本写进宿主系统剪贴板。
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
        let working_url = format!("ws://{address}/api/v1/ws");
        let candidates = client_runtime
            .offer_peer_endpoint(
                &peer_id,
                PeerEndpoint {
                    source: PeerEndpointSource::Mdns,
                    url: working_url.clone(),
                },
            )
            .expect("start dialer");
        let client_task = tokio::spawn(dial_peer(
            client_runtime.clone(),
            peer_id.clone(),
            candidates,
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

        for ip in ["127.0.0.2", "127.0.0.3"] {
            assert!(client_runtime
                .offer_peer_endpoint(
                    &peer_id,
                    PeerEndpoint {
                        source: PeerEndpointSource::Mdns,
                        url: format!("ws://{ip}:{}/api/v1/ws", address.port()),
                    },
                )
                .is_none());
        }

        assert_eq!(
            client_runtime.peer(&peer_id).and_then(|peer| peer.endpoint),
            Some(working_url.clone()),
        );

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
        .expect("peer should reconnect to the last working mDNS address");

        assert_eq!(
            client_runtime.peer(&peer_id).and_then(|peer| peer.endpoint),
            Some(working_url),
        );

        let peer = super::connect_manual(client_runtime.clone(), format!("http://{address}"))
            .await
            .expect("manual connection resolves the same peer");
        assert_eq!(peer.node_id, peer_id);
        assert_eq!(client_runtime.peers_snapshot().len(), 1);

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
