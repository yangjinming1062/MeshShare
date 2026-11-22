mod liveness;

use super::*;
use crate::runtime::NodeRuntime;
use crate::terminal::{Input, Output, SessionIo};
use std::fs;
use std::net::SocketAddr;
use tokio::io::AsyncReadExt;

fn inject_host_key(node: &NodeRuntime) {
    let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
    node.ssh.host.inject_key(key);
}

async fn wait_for_peer(node: &NodeRuntime, peer: &NodeRuntime, enabled: bool) {
    let id = peer.node_id();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !node.terminal_peer_keys(&id).is_some_and(|keys| {
            keys.client_fingerprint == peer.ssh.client_fingerprint()
                && keys.host_fingerprint.is_some() == enabled
        }) || node.peer_supports(&id, crate::model::TERMINAL_SSH_CAPABILITY) != enabled
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("both the peer channel and current terminal advertisement must be ready");
}

async fn connect_nodes(dialer: &Arc<NodeRuntime>, other: &Arc<NodeRuntime>, address: SocketAddr) {
    crate::discovery::connect_manual(dialer.clone(), format!("http://{address}"))
        .await
        .unwrap();
    wait_for_peer(dialer, other, other.ssh.host_fingerprint().is_some()).await;
    wait_for_peer(other, dialer, dialer.ssh.host_fingerprint().is_some()).await;
}

async fn open_shell(source: &Arc<NodeRuntime>, target: &NodeRuntime) -> SessionIo {
    let mut session = source
        .connect_terminal(target.node_id(), 80, 25)
        .await
        .unwrap();
    #[cfg(windows)]
    crate::terminal::tests::wait_for_windows_shell(&mut session).await;
    session
        .input
        .send(Input::Resize {
            cols: 103,
            rows: 31,
        })
        .await
        .unwrap();
    expect_shell_response(&mut session).await;
    session
}

async fn serve_host(
    host: &Arc<NodeRuntime>,
    dir: &std::path::Path,
) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    host.node.write().port = address.port();
    let app = crate::server::router(host.clone(), dir.join("www"));
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap()
    });
    (address, task)
}

async fn expect_shell_response(session: &mut SessionIo) {
    session
        .input
        .send(Input::Data(b"echo \"MESH-PTY-$((6*7))\"\r".to_vec()))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(8), async {
        let mut seen = Vec::new();
        while let Some(Output::Data(bytes)) = session.output.recv().await {
            seen.extend(bytes);
            if String::from_utf8_lossy(&seen).contains("MESH-PTY-42") {
                return;
            }
        }
        panic!("shell exited before producing command result");
    })
    .await
    .expect("interactive shell output");
}

async fn wait_disconnected(session: &mut SessionIo) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(msg) = session.output.recv().await {
            if matches!(msg, Output::Exit { .. }) {
                return;
            }
        }
        panic!("lost terminal end status")
    })
    .await
    .expect("revoking terminal access must close the stream");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn desktop_and_android_follow_the_host_switch_on_live_node_connections() {
    let dir = std::env::temp_dir().join(format!("meshshare-ssh-switch-{}", uuid::Uuid::new_v4()));
    let host = NodeRuntime::new(dir.join("host"), 0, "test".into()).unwrap();
    let desktop = NodeRuntime::new(dir.join("desktop"), 0, "test".into()).unwrap();
    let phone = NodeRuntime::new_with_profile(
        dir.join("phone"),
        0,
        "test".into(),
        crate::runtime::NodeProfile::Android,
    )
    .unwrap();
    phone.node.write().platform = "android".into();
    assert!(!host.ssh.host_settings(None).await.unwrap());
    inject_host_key(&host);
    inject_host_key(&desktop);
    let (host_addr, host_task) = serve_host(&host, &dir).await;
    let (desktop_addr, desktop_task) = serve_host(&desktop, &dir).await;
    // 两种节点连接方向都要收到后续 Hello。
    connect_nodes(&host, &desktop, desktop_addr).await;
    connect_nodes(&phone, &host, host_addr).await;
    let host_id = host.node_id();
    let http = crate::runtime::node_http_client()
        .http1_only()
        .build()
        .unwrap();
    let upgrade = || {
        http.get(format!("http://{host_addr}{ROUTE}"))
            .header("Connection", "upgrade")
            .header("Upgrade", UPGRADE)
            .header("X-MeshShare-Terminal-Target", &host_id)
            .send()
    };
    assert_eq!(
        upgrade().await.unwrap().status(),
        reqwest::StatusCode::FORBIDDEN
    );
    assert!(desktop
        .connect_terminal(host_id.clone(), 80, 25)
        .await
        .is_err());

    host.ssh.host_settings(Some(true)).await.unwrap();
    wait_for_peer(&desktop, &host, true).await;
    wait_for_peer(&phone, &host, true).await;
    let mut first = open_shell(&desktop, &host).await;
    let mut second = open_shell(&phone, &host).await;
    assert!(!host.begin_update_install());
    let mut local = crate::terminal::spawn_pty(80, 25, host.reserve_terminal().unwrap()).unwrap();
    #[cfg(windows)]
    crate::terminal::tests::wait_for_windows_shell(&mut local).await;

    desktop.ssh.host_settings(Some(true)).await.unwrap();
    wait_for_peer(&host, &desktop, true).await;
    let mut outbound = open_shell(&host, &desktop).await;
    let mut handshake = upgrade().await.unwrap().upgrade().await.unwrap();
    host.ssh.host_settings(Some(false)).await.unwrap();
    wait_for_peer(&desktop, &host, false).await;
    wait_for_peer(&phone, &host, false).await;
    assert_eq!(
        upgrade().await.unwrap().status(),
        reqwest::StatusCode::FORBIDDEN
    );
    assert!(phone
        .connect_terminal(host_id.clone(), 80, 25)
        .await
        .is_err());
    // 快速重新开启时，先前代次的会话和握手仍须结束。
    host.ssh.host_settings(Some(true)).await.unwrap();
    tokio::join!(
        wait_disconnected(&mut first),
        wait_disconnected(&mut second)
    );
    let mut banner = Vec::new();
    // TCP EOF 和连接重置都表示旧握手已被中断。
    let _ = tokio::time::timeout(Duration::from_secs(3), handshake.read_to_end(&mut banner))
        .await
        .expect("old handshake must be closed");
    expect_shell_response(&mut local).await;
    drop(local);
    expect_shell_response(&mut outbound).await;
    wait_for_peer(&phone, &host, true).await;
    drop(open_shell(&phone, &host).await);
    drop(outbound);
    tokio::time::timeout(Duration::from_secs(5), async {
        while !host.begin_update_install() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("all inbound PTYs released their update reservations");
    for node in [&host, &desktop, &phone] {
        node.stopping
            .store(true, std::sync::atomic::Ordering::SeqCst);
        node.ssh.shutdown();
    }
    host_task.abort();
    desktop_task.abort();
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn legacy_nodes_share_the_node_protocol_and_browser_sockets_stay_ui_only() {
    use crate::model::WireMessage;
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::{
        connect_async,
        tungstenite::{client::IntoClientRequest, Message},
    };

    let dir = std::env::temp_dir().join(format!("meshshare-ssh-hello-{}", uuid::Uuid::new_v4()));
    let runtime = NodeRuntime::new(dir.clone(), 0, "test".into()).unwrap();
    let (address, server) = serve_host(&runtime, &dir).await;
    let url = format!("ws://{address}/api/v1/ws");
    let mut old = runtime.node_info();
    old.node_id = "previous-version".to_owned();
    old.terminal = None;
    old.capabilities.push("terminal.ssh.v2".to_string());
    let hello = Message::Text(serde_json::to_string(&WireMessage::Hello { node: old }).unwrap());
    let mut request = url.as_str().into_client_request().unwrap();
    request
        .headers_mut()
        .insert("Origin", "http://localhost".parse().unwrap());
    let (mut browser, _) = connect_async(request).await.unwrap();
    browser.next().await.unwrap().unwrap();
    browser.send(hello.clone()).await.unwrap();
    let closed = tokio::time::timeout(Duration::from_secs(2), browser.next())
        .await
        .unwrap();
    assert!(matches!(
        closed,
        None | Some(Err(_) | Ok(Message::Close(_)))
    ));
    assert!(!runtime.has_peer_channel("previous-version"));

    let (mut legacy, _) = connect_async(url).await.unwrap();
    legacy.next().await.unwrap().unwrap();
    legacy.send(hello).await.unwrap();
    let ping = WireMessage::Ping { at_ms: 42 };
    legacy
        .send(Message::Text(serde_json::to_string(&ping).unwrap()))
        .await
        .unwrap();
    let pong = tokio::time::timeout(Duration::from_secs(2), legacy.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(
        serde_json::from_str(pong.to_text().unwrap()),
        Ok(WireMessage::Pong { at_ms: 42 })
    ));
    assert!(runtime.has_peer_channel("previous-version"));
    assert!(runtime.terminal_peer_keys("previous-version").is_none());
    assert!(runtime
        .connect_terminal("previous-version".to_owned(), 80, 25)
        .await
        .is_err());
    legacy.close(None).await.unwrap();
    server.abort();
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn authentication_requires_both_keys_from_a_registered_node_channel() {
    let dir = std::env::temp_dir().join(format!("meshshare-ssh-identity-{}", uuid::Uuid::new_v4()));
    let host = NodeRuntime::new(dir.join("host"), 0, "test".into()).unwrap();
    let caller = NodeRuntime::new(dir.join("caller"), 0, "test".into()).unwrap();
    inject_host_key(&host);
    host.ssh.host_settings(Some(true)).await.unwrap();
    let (addr, task) = serve_host(&host, &dir).await;
    // 发起端拥有目标通道，但目标尚未登记此发起端。
    let peer = crate::server::peer_from_hello(
        host.node_info(),
        &caller,
        Some(format!("ws://{addr}/api/v1/ws")),
    )
    .unwrap();
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    let _session = caller.attach_peer_session(peer, tx).unwrap();
    let connect = || caller.connect_terminal(host.node_id(), 80, 25);
    assert!(connect().await.is_err());
    let mut peer = crate::server::peer_from_hello(caller.node_info(), &host, None).unwrap();
    peer.terminal.as_mut().unwrap().client_fingerprint = host.ssh.client_fingerprint();
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    let _registered = host.attach_peer_session(peer, tx).unwrap();
    assert!(connect().await.is_err());
    host.peers
        .write()
        .get_mut(&caller.node_id())
        .unwrap()
        .terminal
        .as_mut()
        .unwrap()
        .client_fingerprint = caller.ssh.client_fingerprint();
    caller
        .peers
        .write()
        .get_mut(&host.node_id())
        .unwrap()
        .terminal
        .as_mut()
        .unwrap()
        .host_fingerprint = Some(caller.ssh.client_fingerprint());
    assert!(connect().await.is_err());
    assert!(!host.update_has_active_work());
    task.abort();
    fs::remove_dir_all(dir).unwrap();
}
