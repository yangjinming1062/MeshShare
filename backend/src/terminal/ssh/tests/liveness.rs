use super::*;
use tokio::{io, net::TcpListener};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn inbound_shells_end_with_the_authenticated_peer_session_or_client_key() {
    use crate::model::WireMessage;
    use futures_util::SinkExt;
    use tokio_tungstenite::{connect_async, tungstenite::Message};

    let dir = std::env::temp_dir().join(format!("meshshare-ssh-peer-{}", uuid::Uuid::new_v4()));
    let host = NodeRuntime::new(dir.join("host"), 0, "test".into()).unwrap();
    let client = NodeRuntime::new(dir.join("client"), 0, "test".into()).unwrap();
    let other = NodeRuntime::new(dir.join("other"), 0, "test".into()).unwrap();
    let (addr, server) = serve_host(&host, &dir).await;
    connect_nodes(&other, &host, addr).await;
    let mut independent = open_shell(&other, &host).await;

    // 手动驱动该客户端的节点 WebSocket，SSH 仍走完整的客户端、主机和 PTY。
    let url = format!("ws://{addr}/api/v1/ws");
    let target =
        crate::server::peer_from_hello(host.node_info(), &client, Some(url.clone())).unwrap();
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    let _target = client.attach_peer_session(target, tx).unwrap();
    let hello = |node| Message::Text(serde_json::to_string(&WireMessage::Hello { node }).unwrap());
    let mut legacy_disabled = client.node_info();
    legacy_disabled.terminal.as_mut().unwrap().host_fingerprint = None;
    legacy_disabled
        .capabilities
        .retain(|cap| cap != crate::model::TERMINAL_SSH_CAPABILITY);
    let connect = || async {
        let (mut ws, _) = connect_async(&url).await.unwrap();
        ws.send(hello(legacy_disabled.clone())).await.unwrap();
        wait_for_peer(&host, &client, false).await;
        ws
    };
    let mut ws = connect().await;
    let mut first = open_shell(&client, &host).await;
    // 旧版 v3 可以在同一通道更新主机能力，出站 shell 仍绑定原客户端密钥。
    ws.send(hello(client.node_info())).await.unwrap();
    wait_for_peer(&host, &client, true).await;
    ws.send(hello(legacy_disabled.clone())).await.unwrap();
    wait_for_peer(&host, &client, false).await;
    expect_shell_response(&mut first).await;

    let mut rotated = client.node_info();
    rotated.terminal.as_mut().unwrap().client_fingerprint = other.ssh.client_fingerprint();
    ws.send(hello(rotated)).await.unwrap();
    ws.send(hello(client.node_info())).await.unwrap();
    wait_disconnected(&mut first).await;
    wait_for_peer(&host, &client, true).await;
    let mut second = open_shell(&client, &host).await;

    ws.close(None).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while host.has_peer_channel(&client.node_id()) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the old node WebSocket must be removed");
    let mut replacement = connect().await;
    wait_disconnected(&mut second).await;
    drop(open_shell(&client, &host).await);
    replacement.close(None).await.unwrap();
    expect_shell_response(&mut independent).await;
    drop(independent);
    tokio::time::timeout(Duration::from_secs(5), async {
        while host.update_has_active_work() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("all host PTYs must release their update reservations");
    other
        .stopping
        .store(true, std::sync::atomic::Ordering::SeqCst);
    server.abort();
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_and_backpressured_shell_survives_until_silent_link_loss() {
    let dir = std::env::temp_dir().join(format!("meshshare-ssh-idle-{}", uuid::Uuid::new_v4()));
    let host = NodeRuntime::new(dir.join("host"), 0, "test".into()).unwrap();
    let client = NodeRuntime::new(dir.join("client"), 0, "test".into()).unwrap();
    let (host_addr, server) = serve_host(&host, &dir).await;

    let relay = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", relay.local_addr().unwrap());
    let outage = CancellationToken::new();
    let stop_forwarding = outage.clone();
    let proxy = tokio::spawn(async move {
        let (mut from_client, _) = relay.accept().await.unwrap();
        let mut from_host = tokio::net::TcpStream::connect(host_addr).await.unwrap();
        tokio::select! {
            _ = stop_forwarding.cancelled() => (),
            _ = io::copy_bidirectional(&mut from_client, &mut from_host) => panic!("SSH disconnected before the simulated outage"),
        }
        // 丢弃两个方向的数据，保留 TCP 连接；一端结束时，对端也必须自行检测断线。
        let mut sink_client = io::sink();
        let mut sink_host = io::sink();
        let _ = tokio::join!(
            io::copy(&mut from_client, &mut sink_client),
            io::copy(&mut from_host, &mut sink_host),
        );
    });

    let host_id = host.node_id();
    connect_nodes(&client, &host, host_addr).await;
    // 节点连接仍直连，只有 SSH 走可独立断流的中继。
    client.peers.write().get_mut(&host_id).unwrap().endpoint = Some(format!(
        "ws://{}/api/v1/ws",
        base.trim_start_matches("http://")
    ));
    let mut session = client.connect_terminal(host_id, 80, 25).await.unwrap();
    #[cfg(windows)]
    crate::terminal::tests::wait_for_windows_shell(&mut session).await;

    tokio::time::sleep(KEEPALIVE * 6).await;
    assert!(host.update_has_active_work(), "idle shell must stay active");
    expect_shell_response(&mut session).await;

    // 暂停界面消费，直到两端的普通数据队列足以填满，再跨过静默断线期限。
    #[cfg(unix)]
    let flood =
        b"head -c 4194304 /dev/zero | tr '\\000' x; printf 'MESH%s\\n' DRAINED\r".as_slice();
    #[cfg(windows)]
    let flood =
        b"[Console]::Write(('x' * 4194304)); Write-Output ('MESH' + 'DRAINED')\r".as_slice();
    session
        .input
        .send(Input::Data(flood.to_vec()))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(12), async {
        while session.output.len() < session.output.max_capacity() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("terminal output queue must fill");
    tokio::time::sleep(KEEPALIVE * (MISSED_HEARTBEATS + 2)).await;
    assert!(
        host.update_has_active_work(),
        "backpressure must not close the shell"
    );
    tokio::time::timeout(Duration::from_secs(35), async {
        let marker = b"MESHDRAINED";
        let mut seen = Vec::new();
        while let Some(Output::Data(bytes)) = session.output.recv().await {
            let start = seen.len().saturating_sub(marker.len() - 1);
            seen.extend(bytes);
            if seen[start..]
                .windows(marker.len())
                .any(|part| part == marker)
            {
                #[cfg(unix)]
                assert!(seen
                    .split(|byte| *byte != b'x')
                    .any(|run| run.len() == 4194304));
                return;
            }
        }
        panic!("shell exited before its output was drained");
    })
    .await
    .expect("resume all pending output");
    expect_shell_response(&mut session).await;

    outage.cancel();
    tokio::time::timeout(KEEPALIVE * 8 + Duration::from_secs(2), async {
        while !matches!(
            session.output.recv().await.expect("terminal end event"),
            Output::Exit { .. }
        ) {}
        while host.update_has_active_work() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        proxy.await.unwrap();
    })
    .await
    .expect("client, host PTY and both TCP sockets must close independently");
    server.abort();
    fs::remove_dir_all(dir).unwrap();
}
