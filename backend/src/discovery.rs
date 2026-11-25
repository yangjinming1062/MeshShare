use crate::model::{NodeInfo, WireMessage, PROTOCOL_VERSION};
use crate::runtime::{is_allowed_peer_address, NodeRuntime, PeerEndpoint, PeerEndpointSource};
use crate::server;
use futures_util::{SinkExt, StreamExt};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use std::collections::{BTreeSet, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tokio::runtime::Handle;
use tokio::sync::watch;
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
            let own_id = runtime.node_id();
            let fullname = format!("{own_id}.{SERVICE_TYPE}");
            let mut observed_addresses = crate::runtime::bind_addresses();
            let mut observed_network_rebind_generation = runtime.network_rebind_generation();
            while !runtime.stopping.load(Ordering::SeqCst) {
                let Ok(daemon) = ServiceDaemon::new() else {
                    thread::sleep(Duration::from_secs(1));
                    continue;
                };
                let Ok(receiver) = daemon.browse(SERVICE_TYPE) else {
                    if let Ok(status) = daemon.shutdown() {
                        let _ = status.recv_timeout(Duration::from_secs(1));
                    }
                    thread::sleep(Duration::from_secs(1));
                    continue;
                };
                let mut restart = false;
                let mut registered_addresses = Vec::new();
                let mut registered_capabilities = Vec::new();
                while !runtime.stopping.load(Ordering::SeqCst) {
                    // VPN 或 Wi-Fi 变化时重建 mDNS 守护进程，
                    // 让每个接口的 socket 使用当前进程的网络绑定。
                    let current_addresses = crate::runtime::bind_addresses();
                    let current_network_rebind_generation = runtime.network_rebind_generation();
                    if current_addresses != observed_addresses
                        || current_network_rebind_generation != observed_network_rebind_generation
                    {
                        restart = true;
                        break;
                    }
                    let node = runtime.node_info();
                    let addresses: Vec<_> = node
                        .addresses
                        .iter()
                        .filter_map(|address| address.address.parse().ok())
                        .filter(|address| current_addresses.contains(address))
                        .map(IpAddr::V4)
                        .collect();
                    if addresses != registered_addresses
                        || (!addresses.is_empty() && node.capabilities != registered_capabilities)
                    {
                        // Re-register the same instance to publish the new listener set.
                        let result = if addresses.is_empty() {
                            daemon.unregister(&fullname).map(|_| ())
                        } else {
                            service_info(&node, &addresses).and_then(|info| daemon.register(info))
                        };
                        if result.is_ok() {
                            registered_addresses = addresses;
                            registered_capabilities = node.capabilities;
                        }
                    }
                    if let Ok(ServiceEvent::ServiceResolved(info)) =
                        receiver.recv_timeout(Duration::from_secs(1))
                    {
                        offer_mdns_service(&handle, &runtime, &own_id, &info);
                    }
                }
                if !registered_addresses.is_empty() {
                    if let Ok(status) = daemon.unregister(&fullname) {
                        let _ = status.recv_timeout(Duration::from_secs(1));
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

fn service_info(node: &NodeInfo, addresses: &[IpAddr]) -> mdns_sd::Result<ServiceInfo> {
    let host = format!("{}.local.", node.node_id.replace('-', ""));
    let protocol = PROTOCOL_VERSION.to_string();
    let features = node.capabilities.join(",");
    let properties = [
        ("node_id", node.node_id.as_str()),
        ("node_name", node.node_name.as_str()),
        ("protocol", protocol.as_str()),
        ("features", features.as_str()),
    ];
    ServiceInfo::new(
        SERVICE_TYPE,
        &node.node_id,
        &host,
        addresses,
        node.port,
        &properties[..],
    )
}

fn offer_mdns_service(
    handle: &Handle,
    runtime: &Arc<NodeRuntime>,
    own_id: &str,
    info: &ServiceInfo,
) {
    let Some(node_id) = info.get_property_val_str("node_id") else {
        return;
    };
    let protocol = info
        .get_property_val_str("protocol")
        .and_then(|value| value.parse::<u32>().ok());
    let port = info.get_port();
    if node_id == own_id
        || node_id.trim().is_empty()
        || node_id.len() > 256
        || protocol != Some(PROTOCOL_VERSION)
        || port == 0
    {
        return;
    }
    let addresses: BTreeSet<_> = info
        .get_addresses()
        .iter()
        .copied()
        .filter(|ip| is_allowed_peer_address(*ip))
        .collect();
    if addresses.is_empty() {
        return;
    }
    let urls = addresses
        .into_iter()
        .map(|ip| format!("ws://{}/api/v1/ws", SocketAddr::new(ip, port)))
        .collect();
    // Either peer can dial when multicast visibility is asymmetric.
    queue_peer_connection(handle, runtime, node_id, PeerEndpointSource::Mdns, urls);
}

pub(crate) fn queue_peer_connection(
    handle: &Handle,
    runtime: &Arc<NodeRuntime>,
    node_id: &str,
    source: PeerEndpointSource,
    urls: Vec<String>,
) {
    if let Some(candidates) = runtime.offer_peer_endpoints(node_id, source, urls) {
        handle.spawn(dial_peer(runtime.clone(), node_id.to_owned(), candidates));
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
    let tcp = match socket.get_ref() {
        MaybeTlsStream::Plain(stream) => stream,
        MaybeTlsStream::Rustls(stream) => stream.get_ref().0,
        _ => return false,
    };
    if !tcp
        .peer_addr()
        .is_ok_and(|peer| is_allowed_peer_address(peer.ip()))
    {
        return false;
    }
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
    let (tx, mut rx) = crate::runtime::peer_channel(64);
    let Some(_session) = runtime.attach_peer_session(peer, tx) else {
        return false;
    };
    let mut host_changes = runtime.ssh.subscribe_host();
    let hello = WireMessage::Hello {
        node: runtime.node_info(),
    };
    let Ok(body) = serde_json::to_string(&hello) else {
        return false;
    };
    if !send_frame(&mut writer, Message::Text(body)).await {
        return false;
    }
    server::sync_peer_state(runtime, node_id).await;

    let mut heartbeat = tokio::time::interval(Duration::from_secs(10));
    let mut clipboard_resync = tokio::time::interval(Duration::from_secs(1));
    let mut last_pong = tokio::time::Instant::now();
    while !runtime.stopping.load(Ordering::SeqCst) {
        let frame = tokio::select! {
            _ = heartbeat.tick() => {
                if last_pong.elapsed() > Duration::from_secs(30) { break; }
                let ping = WireMessage::Ping { at_ms: crate::clock::now_millis() };
                let Ok(body) = serde_json::to_string(&ping) else { break; };
                Message::Text(body)
            }
            _ = clipboard_resync.tick() => {
                let _ = runtime.flush_clipboard_resync(node_id);
                let _ = runtime.flush_history_deletions(node_id);
                continue;
            }
            _ = host_changes.changed() => {
                let hello = WireMessage::Hello { node: runtime.node_info() };
                let Ok(body) = serde_json::to_string(&hello) else { break };
                Message::Text(body)
            }
            outbound = rx.recv() => {
                let Some(message) = outbound else { break };
                let Ok(body) = serde_json::to_string(&message) else { break };
                Message::Text(body)
            }
            inbound = reader.next() => match inbound {
                Some(Ok(Message::Text(body))) => {
                    let Ok(message) = serde_json::from_str::<WireMessage>(&body) else { continue };
                    if matches!(message, WireMessage::Pong { .. }) {
                        last_pong = tokio::time::Instant::now();
                    }
                    let is_hello = matches!(&message, WireMessage::Hello { .. });
                    if server::handle_wire_message(message, runtime, Some(node_id)).await.is_none() && is_hello {
                        break;
                    }
                    continue;
                }
                Some(Ok(Message::Ping(payload))) => Message::Pong(payload),
                Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                Some(Ok(_)) => continue,
            }
        };
        if !send_frame(&mut writer, frame).await {
            break;
        }
    }
    true
}

async fn send_frame(
    writer: &mut futures_util::stream::SplitSink<PeerSocket, Message>,
    frame: Message,
) -> bool {
    matches!(
        tokio::time::timeout(Duration::from_secs(10), writer.send(frame)).await,
        Ok(Ok(()))
    )
}

#[cfg(test)]
mod tests {
    use super::{dial_peer, service_info};
    use crate::model::WireMessage;
    use crate::runtime::{NodeProfile, NodeRuntime, PeerEndpointSource};
    use crate::server;
    use futures_util::{SinkExt, StreamExt};
    use std::collections::HashSet;
    use std::net::IpAddr;
    use std::sync::atomic::Ordering;
    use std::time::Duration;
    use tokio::net::TcpListener;
    use tokio::time::timeout;
    use tokio_tungstenite::{accept_async, tungstenite::Message};
    use uuid::Uuid;

    #[test]
    fn mdns_service_advertises_each_address_on_the_selected_port() {
        let data = std::env::temp_dir().join(format!("meshshare-mdns-service-{}", Uuid::new_v4()));
        let runtime =
            NodeRuntime::new_with_profile(data.clone(), 37881, "test".into(), NodeProfile::Android)
                .expect("create runtime");
        let addresses = [
            IpAddr::from([192, 168, 1, 2]),
            IpAddr::from([192, 168, 2, 2]),
        ];
        let node = runtime.node_info();
        let info = service_info(&node, &addresses).expect("compose mDNS service");
        assert_eq!(info.get_port(), 37881);
        assert_eq!(
            info.get_property_val_str("node_id"),
            Some(node.node_id.as_str())
        );
        assert_eq!(info.get_addresses(), &HashSet::from(addresses));
        let _ = std::fs::remove_dir_all(data);
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
            .offer_peer_endpoints(
                &peer_id,
                PeerEndpointSource::Mdns,
                vec![working_url.clone()],
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
            .offer_peer_endpoints(
                &peer_id,
                PeerEndpointSource::Mdns,
                vec![format!("ws://{stale_address}/api/v1/ws")],
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
            axum::serve(
                listener,
                server_app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .expect("serve server router");
        });
        let peer_id = server_runtime.node_id();
        let working_url = format!("ws://{address}/api/v1/ws");
        let unavailable = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind unreachable WebSocket route");
        let rejected_url = format!(
            "ws://{}/api/v1/ws",
            unavailable.local_addr().expect("unreachable route address")
        );
        let rejecting_task = tokio::spawn(async move {
            while let Ok((socket, _)) = unavailable.accept().await {
                drop(socket);
            }
        });
        let candidates = client_runtime
            .offer_peer_endpoints(
                &peer_id,
                PeerEndpointSource::Mdns,
                vec![rejected_url.clone(), working_url.clone()],
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

        assert!(client_runtime
            .offer_peer_endpoints(&peer_id, PeerEndpointSource::Mdns, vec![rejected_url],)
            .is_none());

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
            axum::serve(
                listener,
                server_app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
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
        rejecting_task.abort();
        let _ = client_task.await;
        let _ = server_task.await;
        let _ = rejecting_task.await;
        let _ = std::fs::remove_dir_all(server_data);
        let _ = std::fs::remove_dir_all(client_data);
    }
}
