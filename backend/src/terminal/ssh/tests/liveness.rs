use super::*;
use tokio::{io, net::TcpListener};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_and_backpressured_shell_survives_until_silent_link_loss() {
    let dir = std::env::temp_dir().join(format!("meshshare-ssh-idle-{}", uuid::Uuid::new_v4()));
    let host = NodeRuntime::new(dir.join("host"), 0, "test".into()).unwrap();
    let client = NodeRuntime::new(dir.join("client"), 0, "test".into()).unwrap();
    let host_key = inject_identity(&host.ssh);
    let client_key = inject_identity(&client.ssh);
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

    host.ssh.set_enabled(true).await.unwrap();
    host.ssh.begin_pairing().await.unwrap();
    let target = host.node_id();
    let ssh = client.ssh.clone();
    let opening = tokio::spawn(async move { ssh.connect(target, vec![base], 80, 25).await });
    approve(&client.ssh, Direction::Outgoing, &host_key).await;
    approve(&host.ssh, Direction::Incoming, &client_key).await;
    let mut session = opening.await.unwrap().unwrap();
    #[cfg(windows)]
    crate::terminal::tests::wait_for_windows_prompt(&mut session).await;

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
