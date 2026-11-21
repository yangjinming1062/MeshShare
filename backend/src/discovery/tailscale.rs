use super::queue_peer_connection;
use crate::model::{NodeInfo, PROTOCOL_VERSION};
use crate::runtime::{
    is_tailscale_ipv4, node_http_client, NodeRuntime, PeerEndpoint, PeerEndpointSource,
};
use crate::server::validate_node_info;
use crate::DEFAULT_NODE_PORT;
use anyhow::{ensure, Context, Result};
use futures_util::{stream, StreamExt};
use serde::Deserialize;
use std::collections::{BTreeSet, HashMap};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;
use tokio::runtime::Handle;
use tokio::time::{self, Instant, MissedTickBehavior};

const SCAN_INTERVAL: Duration = Duration::from_secs(20);
const STATUS_TIMEOUT: Duration = Duration::from_secs(3);
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
// Leave time for the CLI status lookup before the next scan.
const PROBE_ROUND_BUDGET: Duration = Duration::from_secs(16);
const MAX_CONCURRENT_PROBES: usize = 8;
const MAX_INFO_BYTES: usize = 128 * 1024;

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct TailscaleStatus {
    backend_state: String,
    #[serde(rename = "TailscaleIPs")]
    tailscale_ips: Option<Vec<String>>,
    peer: Option<HashMap<String, TailscalePeer>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct TailscalePeer {
    #[serde(default)]
    online: bool,
    #[serde(rename = "TailscaleIPs")]
    tailscale_ips: Option<Vec<String>>,
}

impl TailscaleStatus {
    fn online_ipv4_peers(self, listeners: &BTreeSet<Ipv4Addr>) -> BTreeSet<Ipv4Addr> {
        let local_ips: BTreeSet<_> = self
            .tailscale_ips
            .iter()
            .flatten()
            .filter_map(|address| tailscale_ipv4(address))
            .collect();
        if self.backend_state != "Running" || local_ips.is_disjoint(listeners) {
            return BTreeSet::new();
        }
        self.peer
            .unwrap_or_default()
            .into_values()
            .filter(|peer| peer.online)
            .flat_map(|peer| peer.tailscale_ips.into_iter().flatten())
            .filter_map(|address| tailscale_ipv4(&address))
            .filter(|ip| !local_ips.contains(ip) && !listeners.contains(ip))
            .collect()
    }
}

fn tailscale_ipv4(address: &str) -> Option<Ipv4Addr> {
    address.parse().ok().filter(|ip| is_tailscale_ipv4(*ip))
}

fn listening_ipv4s(node: &NodeInfo) -> BTreeSet<Ipv4Addr> {
    node.addresses
        .iter()
        .filter_map(|address| tailscale_ipv4(&address.address))
        .collect()
}

#[derive(Default)]
struct PeerProbe {
    node_id: Option<String>,
    failures: u8,
    retry_at: Option<Instant>,
}

#[derive(Default)]
struct TailnetDiscovery {
    probes: HashMap<SocketAddrV4, PeerProbe>,
    last_started: Option<SocketAddrV4>,
}

impl TailnetDiscovery {
    fn retain_endpoints(&mut self, runtime: &NodeRuntime, endpoints: &BTreeSet<SocketAddrV4>) {
        self.probes.retain(|address, probe| {
            if endpoints.contains(address) {
                return true;
            }
            if let Some(node_id) = &probe.node_id {
                runtime.forget_tailscale_endpoint(node_id, &websocket_url(*address));
            }
            false
        });
    }

    async fn refresh(
        &mut self,
        runtime: &Arc<NodeRuntime>,
        client: &reqwest::Client,
        endpoints: BTreeSet<SocketAddrV4>,
        budget: Duration,
    ) {
        let now = Instant::now();
        let deadline = now + budget;
        self.retain_endpoints(runtime, &endpoints);
        let mut due: Vec<_> = endpoints
            .into_iter()
            .filter(|address| {
                self.probes.get(address).is_none_or(|probe| {
                    probe.retry_at.is_none_or(|deadline| now >= deadline)
                        && probe
                            .node_id
                            .as_deref()
                            .is_none_or(|id| !runtime.has_peer_channel(id))
                })
            })
            .collect();
        if let Some(last_started) = self.last_started {
            let start = due.partition_point(|address| *address <= last_started);
            due.rotate_left(start);
        }
        let own_id = runtime.node_id();
        let own_id = own_id.as_str();
        // Give every admitted request its full timeout and resume after the last one started.
        let due = due
            .into_iter()
            .take_while(|_| deadline.saturating_duration_since(Instant::now()) >= PROBE_TIMEOUT);
        let last_started = &mut self.last_started;
        let mut probes = stream::iter(due)
            .map(|address| {
                *last_started = Some(address);
                async move { (address, probe_node(client, own_id, address).await) }
            })
            .buffer_unordered(MAX_CONCURRENT_PROBES);
        while let Ok(Some((address, found))) = time::timeout_at(deadline, probes.next()).await {
            if runtime.stopping.load(Ordering::SeqCst) {
                break;
            }
            let probe = self.probes.entry(address).or_default();
            let url = websocket_url(address);
            if probe.node_id != found {
                if let Some(previous) = probe.node_id.take() {
                    runtime.forget_tailscale_endpoint(&previous, &url);
                }
            }
            if let Some(node_id) = found {
                queue_peer_connection(
                    &Handle::current(),
                    runtime,
                    node_id.clone(),
                    PeerEndpoint {
                        source: PeerEndpointSource::Tailscale,
                        url,
                    },
                );
                *probe = PeerProbe {
                    node_id: Some(node_id),
                    ..PeerProbe::default()
                };
            } else {
                probe.failures = (probe.failures + 1).min(3);
                probe.retry_at =
                    Some(Instant::now() + Duration::from_secs(30 * (1 << (probe.failures - 1))));
            }
        }
    }
}

pub(super) async fn discover(runtime: Arc<NodeRuntime>) {
    let client = match probe_client() {
        Ok(client) => client,
        Err(error) => {
            eprintln!("初始化 Tailscale 发现失败：{error}");
            return;
        }
    };
    let mut interval = time::interval(SCAN_INTERVAL);
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut discovery = TailnetDiscovery::default();
    let mut last_error = None;
    loop {
        interval.tick().await;
        if runtime.stopping.load(Ordering::SeqCst) {
            break;
        }
        let listeners = listening_ipv4s(&runtime.node_info());
        let mut peers = BTreeSet::new();
        if !listeners.is_empty() {
            match read_status().await {
                Ok(status) => {
                    peers = status.online_ipv4_peers(&listening_ipv4s(&runtime.node_info()));
                    last_error = None;
                }
                Err(error) => {
                    let error = format!("{error:#}");
                    if last_error.as_ref() != Some(&error) {
                        eprintln!("读取 Tailscale 状态失败，将重试：{error}");
                        last_error = Some(error);
                    }
                }
            }
        }
        if runtime.stopping.load(Ordering::SeqCst) {
            break;
        }
        discovery
            .refresh(
                &runtime,
                &client,
                peers
                    .into_iter()
                    .map(|ip| SocketAddrV4::new(ip, DEFAULT_NODE_PORT))
                    .collect(),
                PROBE_ROUND_BUDGET,
            )
            .await;
    }
    discovery.retain_endpoints(&runtime, &BTreeSet::new());
}

fn probe_client() -> reqwest::Result<reqwest::Client> {
    node_http_client().timeout(PROBE_TIMEOUT).build()
}

fn websocket_url(address: SocketAddrV4) -> String {
    format!("ws://{address}/api/v1/ws")
}

pub(super) fn is_desktop_node(node: &NodeInfo) -> bool {
    matches!(node.platform.as_str(), "windows" | "macos" | "linux")
}

async fn probe_node(
    client: &reqwest::Client,
    own_id: &str,
    address: SocketAddrV4,
) -> Option<String> {
    let mut response = client
        .get(format!("http://{address}/api/v1/info"))
        .send()
        .await
        .ok()?;
    if !response.status().is_success()
        || response
            .content_length()
            .is_some_and(|len| len > MAX_INFO_BYTES as u64)
    {
        return None;
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if chunk.len() > MAX_INFO_BYTES - body.len() {
            return None;
        }
        body.extend_from_slice(&chunk);
    }
    let node: NodeInfo = serde_json::from_slice(&body).ok()?;
    (validate_node_info(&node)
        && node.node_id != own_id
        && node.protocol_version == PROTOCOL_VERSION
        && node.port == address.port()
        && is_desktop_node(&node))
    .then_some(node.node_id)
}

fn cli_path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    let installed = std::env::var_os("ProgramFiles")
        .map(|directory| PathBuf::from(directory).join(r"Tailscale\tailscale.exe"));
    #[cfg(target_os = "macos")]
    let installed = Some(PathBuf::from(
        "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
    ));
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let installed: Option<PathBuf> = None;
    // Finder launches may omit the Homebrew installation directories from PATH.
    #[cfg(target_os = "macos")]
    let fallback = ["/opt/homebrew/bin/tailscale", "/usr/local/bin/tailscale"]
        .into_iter()
        .map(PathBuf::from);
    #[cfg(not(target_os = "macos"))]
    let fallback = std::iter::empty();
    let executable = if cfg!(windows) {
        "tailscale.exe"
    } else {
        "tailscale"
    };
    let path = std::env::var_os("PATH").unwrap_or_default();
    installed
        .into_iter()
        .chain(std::env::split_paths(&path).map(|directory| directory.join(executable)))
        .chain(fallback)
        .find(|candidate| candidate.is_absolute() && candidate.is_file())
}

async fn read_status() -> Result<TailscaleStatus> {
    let mut command = Command::new(cli_path().context("找不到 Tailscale CLI")?);
    command
        .args(["status", "--json"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(target_os = "windows")]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    #[cfg(target_os = "macos")]
    command.env("TAILSCALE_BE_CLI", "1");
    let output = time::timeout(STATUS_TIMEOUT, command.output())
        .await
        .context("Tailscale CLI 超时")?
        .context("无法运行 Tailscale CLI")?;
    ensure!(
        output.status.success(),
        "Tailscale CLI 退出：{}",
        output.status
    );
    serde_json::from_slice(&output.stdout).context("无法解析 Tailscale 状态")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::WireMessage;
    use crate::runtime::NodeProfile;
    use axum::extract::ws::{Message, WebSocketUpgrade};
    use axum::routing::get;
    use axum::Router;
    use parking_lot::RwLock;
    use std::net::{SocketAddr, SocketAddrV4};
    use tokio::net::TcpListener;
    use tokio::sync::Notify;
    use uuid::Uuid;

    #[test]
    fn status_selects_unique_online_peer_ips_for_a_listening_tailnet() {
        let status = |state| {
            serde_json::from_value::<TailscaleStatus>(serde_json::json!({
                "BackendState": state,
                "TailscaleIPs": ["100.64.0.1", "fd7a:115c:a1e0::1"],
                "Peer": {
                    "first": { "Online": true, "TailscaleIPs": [
                        "100.64.0.1", "100.64.0.2", "100.64.0.9", "192.168.1.2",
                        "100.128.0.2", "fd7a:115c:a1e0::2", "invalid"
                    ] },
                    "second": { "Online": true, "TailscaleIPs": ["100.64.0.2"] },
                    "offline": { "Online": false, "TailscaleIPs": ["100.64.0.3"] },
                    "null": { "Online": true, "TailscaleIPs": null }
                }
            }))
            .expect("parse Tailscale status")
        };
        let listeners =
            BTreeSet::from([Ipv4Addr::new(100, 64, 0, 1), Ipv4Addr::new(100, 64, 0, 9)]);
        assert_eq!(
            status("Running").online_ipv4_peers(&listeners),
            BTreeSet::from([Ipv4Addr::new(100, 64, 0, 2)])
        );
        assert!(status("Stopped").online_ipv4_peers(&listeners).is_empty());
        assert!(status("Running")
            .online_ipv4_peers(&BTreeSet::from([Ipv4Addr::new(100, 64, 0, 9)]))
            .is_empty());
    }

    fn listen_address(listener: &TcpListener) -> SocketAddrV4 {
        let SocketAddr::V4(address) = listener.local_addr().expect("listener address") else {
            panic!("expected IPv4 listener");
        };
        address
    }

    #[tokio::test]
    async fn probe_round_leaves_time_for_status_and_resumes_later_addresses() {
        let data =
            std::env::temp_dir().join(format!("meshshare-tailnet-budget-{}", Uuid::new_v4()));
        let runtime =
            NodeRuntime::new_with_profile(data.clone(), 0, "test".into(), NodeProfile::Android)
                .expect("create runtime");
        let mut listeners = Vec::new();
        for _ in 0..=MAX_CONCURRENT_PROBES {
            // Keep these ports open without sending an HTTP response.
            listeners.push(
                TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
                    .await
                    .expect("bind"),
            );
        }
        let endpoints: BTreeSet<_> = listeners.iter().map(listen_address).collect();
        let last = *endpoints.last().expect("last address");
        let client = probe_client().expect("probe client");
        let mut discovery = TailnetDiscovery::default();
        let one_wave = PROBE_TIMEOUT + Duration::from_secs(1);
        discovery
            .refresh(&runtime, &client, endpoints.clone(), one_wave)
            .await;
        assert_eq!(discovery.probes.len(), MAX_CONCURRENT_PROBES);
        assert!(!discovery.probes.contains_key(&last));

        // Once the earlier peers are due again, the remaining peer still gets a turn first.
        for probe in discovery.probes.values_mut() {
            probe.retry_at = None;
        }
        discovery
            .refresh(&runtime, &client, endpoints, one_wave)
            .await;
        assert!(discovery.probes.contains_key(&last));
        let _ = std::fs::remove_dir_all(data);
    }

    #[tokio::test]
    async fn probe_requires_compatible_desktop_info_at_the_requested_port() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind");
        let address = listen_address(&listener);
        let body = Arc::new(RwLock::new("<html>Not MeshShare</html>".to_string()));
        let route_body = body.clone();
        let router = Router::new().route(
            "/api/v1/info",
            get(move || async move { route_body.read().clone() }),
        );
        let server_task = tokio::spawn(async move { axum::serve(listener, router).await });
        let client = probe_client().expect("probe client");
        assert!(probe_node(&client, "local", address).await.is_none());
        let good = NodeInfo {
            node_id: "remote".to_string(),
            node_name: "test peer".to_string(),
            version: "test".to_string(),
            protocol_version: PROTOCOL_VERSION,
            platform: "macos".to_string(),
            port: address.port(),
            addresses: Vec::new(),
            mobile_addresses: Vec::new(),
            capabilities: Vec::new(),
        };
        for invalid in [
            NodeInfo {
                protocol_version: PROTOCOL_VERSION + 1,
                ..good.clone()
            },
            NodeInfo {
                port: 0,
                ..good.clone()
            },
            NodeInfo {
                platform: "android".to_string(),
                ..good.clone()
            },
            NodeInfo {
                node_id: "local".to_string(),
                ..good.clone()
            },
            NodeInfo {
                node_id: " ".to_string(),
                ..good.clone()
            },
        ] {
            *body.write() = serde_json::to_string(&invalid).expect("serialize info");
            assert!(probe_node(&client, "local", address).await.is_none());
        }
        *body.write() = serde_json::to_string(&good).expect("serialize info");
        assert_eq!(
            probe_node(&client, "local", address).await.as_deref(),
            Some("remote")
        );
        server_task.abort();
        let _ = server_task.await;
    }

    async fn wait_for(description: &str, ready: impl Fn() -> bool) {
        time::timeout(Duration::from_secs(8), async {
            while !ready() {
                time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect(description);
    }

    #[tokio::test]
    async fn probe_success_does_not_publish_a_peer_with_a_mismatched_websocket_hello() {
        let data = std::env::temp_dir().join(format!("meshshare-hello-{}", Uuid::new_v4()));
        let runtime = NodeRuntime::new_with_profile(
            data.clone(),
            0,
            "test".to_string(),
            NodeProfile::Android,
        )
        .expect("local runtime");
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind");
        let address = listen_address(&listener);
        let mut info = runtime.node_info();
        info.node_id = "probed-peer".to_string();
        info.port = address.port();
        let mut other = info.clone();
        other.node_id = "different-peer".to_string();
        let hello =
            serde_json::to_string(&WireMessage::Hello { node: other }).expect("encode hello");
        let disconnected = Arc::new(Notify::new());
        let signal = disconnected.clone();
        let router = Router::new()
            .route("/api/v1/info", get(move || async move { axum::Json(info) }))
            .route(
                "/api/v1/ws",
                get(move |ws: WebSocketUpgrade| async move {
                    ws.on_upgrade(move |mut socket| async move {
                        socket
                            .send(Message::Text(hello))
                            .await
                            .expect("send mismatched hello");
                        assert!(!matches!(socket.recv().await, Some(Ok(Message::Text(_)))));
                        signal.notify_one();
                    })
                }),
            );
        let server = tokio::spawn(async move { axum::serve(listener, router).await });
        let mut events = runtime.subscribe();
        TailnetDiscovery::default()
            .refresh(
                &runtime,
                &probe_client().expect("client"),
                BTreeSet::from([address]),
                PROBE_ROUND_BUDGET,
            )
            .await;
        time::timeout(Duration::from_secs(5), disconnected.notified())
            .await
            .expect("reject mismatched hello");
        assert!(events.try_recv().is_err());
        assert!(runtime.peers_snapshot().is_empty());
        runtime.stopping.store(true, Ordering::SeqCst);
        server.abort();
        let _ = server.await;
        let _ = std::fs::remove_dir_all(data);
    }

    #[tokio::test]
    async fn tailnet_discovery_recovers_after_lan_failure_and_peer_reappearing() {
        let data = std::env::temp_dir().join(format!("meshshare-tailnet-{}", Uuid::new_v4()));
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind tailnet test peer");
        let address = listen_address(&listener);
        let unavailable = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind failed LAN route");
        let lan_url = websocket_url(listen_address(&unavailable));
        let unavailable_task = tokio::spawn(async move {
            while let Ok((socket, _)) = unavailable.accept().await {
                drop(socket);
            }
        });
        // The view-only clipboard profile keeps test traffic out of the system clipboard.
        let remote = NodeRuntime::new_with_profile(
            data.join("remote"),
            address.port(),
            "test".to_string(),
            NodeProfile::Android,
        )
        .expect("remote runtime");
        let local = NodeRuntime::new_with_profile(
            data.join("local"),
            0,
            "test".to_string(),
            NodeProfile::Android,
        )
        .expect("local runtime");
        let app = crate::server::router(remote.clone(), data.join("web"));
        let first_app = app.clone();
        let mut server_task = tokio::spawn(async move {
            axum::serve(
                listener,
                first_app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
        });
        let id = remote.node_id();
        let local_id = local.node_id();
        for _ in 0..2 {
            queue_peer_connection(
                &Handle::current(),
                &local,
                id.clone(),
                PeerEndpoint {
                    source: PeerEndpointSource::Mdns,
                    url: lan_url.clone(),
                },
            );
        }
        let client = probe_client().expect("probe client");
        let mut discovery = TailnetDiscovery::default();
        discovery
            .refresh(
                &local,
                &client,
                BTreeSet::from([address]),
                PROBE_ROUND_BUDGET,
            )
            .await;

        let expected_base = format!("http://{address}");
        wait_for("both peers connected", || {
            local.peer_http_base(&id).as_deref() == Some(&expected_base)
                && remote.has_peer_channel(&local_id)
        })
        .await;
        assert_eq!(local.peers_snapshot().len(), 1);
        discovery
            .refresh(&local, &client, BTreeSet::new(), PROBE_ROUND_BUDGET)
            .await;
        assert_eq!(
            local.peer_http_base(&id).as_deref(),
            Some(expected_base.as_str())
        );

        remote.stopping.store(true, Ordering::SeqCst);
        server_task.abort();
        let _ = (&mut server_task).await;
        wait_for("old channel closed", || {
            !local.has_peer_channel(&id) && !remote.has_peer_channel(&local_id)
        })
        .await;

        let listener = TcpListener::bind(address)
            .await
            .expect("restore tailnet peer");
        remote.stopping.store(false, Ordering::SeqCst);
        server_task = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
        });
        discovery
            .refresh(
                &local,
                &client,
                BTreeSet::from([address]),
                PROBE_ROUND_BUDGET,
            )
            .await;
        wait_for("tailnet peer rediscovered", || {
            local.peer_http_base(&id).as_deref() == Some(&expected_base)
                && remote.has_peer_channel(&local_id)
        })
        .await;
        assert_eq!(local.peers_snapshot().len(), 1);

        local.stopping.store(true, Ordering::SeqCst);
        remote.stopping.store(true, Ordering::SeqCst);
        server_task.abort();
        unavailable_task.abort();
        let _ = server_task.await;
        let _ = unavailable_task.await;
        let _ = std::fs::remove_dir_all(data);
    }
}
