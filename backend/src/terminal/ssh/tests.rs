mod liveness;

use super::*;
use crate::runtime::NodeRuntime;
use crate::terminal::{Input, Output, SessionIo};
use std::fs;

fn inject_identity(service: &SshService) -> String {
    let key = PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519).unwrap();
    let fingerprint = key.public_key().fingerprint(HashAlg::Sha256).to_string();
    assert!(service.key.set(Arc::new(key)).is_ok());
    fingerprint
}

async fn next_request(service: &SshService, direction: Direction) -> Request {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Some(req) = service
                .state()
                .await
                .pending
                .into_iter()
                .find(|req| req.direction == direction)
            {
                break req;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("pairing request")
}

async fn approve(service: &SshService, direction: Direction, expected: &str) {
    let pending = next_request(service, direction).await;
    assert_eq!(pending.fingerprint, expected);
    service.decide(&pending.id, true).unwrap();
}

async fn serve_host(
    host: &Arc<NodeRuntime>,
    dir: &std::path::Path,
) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = crate::server::router(host.clone(), dir.join("www"));
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
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
    .expect("revocation must close stream");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn authenticated_loopback_pair_reconnect_and_revoke() {
    let dir = std::env::temp_dir().join(format!("meshshare-ssh-test-{}", uuid::Uuid::new_v4()));
    let host = NodeRuntime::new(dir.join("host"), 0, "test".into()).unwrap();
    let local = NodeRuntime::new(dir.join("client"), 0, "test".into()).unwrap();
    let host_id = host.node_info().node_id;
    let local_id = local.node_info().node_id;
    let host_key = inject_identity(&host.ssh);
    let local_key = inject_identity(&local.ssh);
    let (address, server) = serve_host(&host, &dir).await;
    let base = format!("http://{address}");
    let connect = || {
        let client = local.ssh.clone();
        let base = base.clone();
        let target = host_id.clone();
        tokio::spawn(async move { client.connect(target, vec![base], 80, 25).await })
    };
    assert!(connect().await.unwrap().is_err());
    host.ssh.set_enabled(true).await.unwrap();
    let opening = connect();
    approve(&local.ssh, Direction::Outgoing, &host_key).await;
    assert!(opening.await.unwrap().is_err(), "closed pairing window");
    host.ssh.begin_pairing().await.unwrap();
    let abandoned = connect();
    let abandoned_request = next_request(&host.ssh, Direction::Incoming).await;
    abandoned.abort();
    let _ = abandoned.await;
    tokio::time::timeout(Duration::from_secs(4), async {
        while !host.ssh.state().await.pending.is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("closing the initiating tab cancels pending authorization");
    assert!(host.ssh.decide(&abandoned_request.id, true).is_err());
    let opening = connect();
    approve(&host.ssh, Direction::Incoming, &local_key).await;
    let mut session = opening.await.unwrap().unwrap();
    session
        .input
        .send(Input::Resize {
            cols: 103,
            rows: 31,
        })
        .await
        .unwrap();
    expect_shell_response(&mut session).await;
    let mut second = connect().await.unwrap().unwrap();
    assert!(
        !host.begin_update_install(),
        "inbound shell holds update gate"
    );
    host.ssh.revoke(Direction::Incoming, &local_id).unwrap();
    wait_disconnected(&mut session).await;
    wait_disconnected(&mut second).await;
    assert!(
        connect().await.unwrap().is_err(),
        "revoked key must not reconnect"
    );
    tokio::time::timeout(Duration::from_secs(4), async {
        while !host.begin_update_install() {
            tokio::time::sleep(Duration::from_millis(20)).await
        }
    })
    .await
    .expect("server released PTY and installation gate");
    host.end_update_install();

    let lease = CancellationToken::new();
    assert!(local
        .ssh
        .authorize(Direction::Outgoing, &host_id, "other-fingerprint", &lease)
        .await
        .is_err());
    assert!(
        local.ssh.state().await.pending.is_empty(),
        "rotated key must require explicit forget"
    );
    local.ssh.shutdown();
    host.ssh.shutdown();
    server.abort();
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn signed_grants_persist_and_reject_replacement() {
    let dir = std::env::temp_dir().join(format!("meshshare-ssh-policy-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    let key = PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519).unwrap();
    let policy = Policy {
        enabled: true,
        incoming: BTreeMap::from([("device".into(), "verified-pin".into())]),
        outgoing: BTreeMap::new(),
    };
    trust::save(&dir, &key, &policy).unwrap();
    let bytes = fs::read(dir.join(trust::FILE)).unwrap();
    let restored = trust::decode(&key, &bytes).unwrap();
    assert!(restored.enabled);
    assert_eq!(restored.incoming, policy.incoming);
    let other = PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519).unwrap();
    assert!(trust::decode(&other, &bytes).is_err());
    let forged = String::from_utf8(bytes)
        .unwrap()
        .replace("verified-pin", "unapproved-pin");
    assert!(trust::decode(&key, forged.as_bytes()).is_err());
    assert!(trust::stored_host_enabled(&dir));
    fs::write(dir.join(trust::FILE), b"{invalid").unwrap();
    assert!(!trust::stored_host_enabled(&dir));
    let file = fs::File::create(dir.join(trust::FILE)).unwrap();
    file.set_len(2 * 1024 * 1024).unwrap();
    assert!(!trust::stored_host_enabled(&dir));
    drop(file);
    fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
#[ignore = "requires an unlocked native desktop credential store"]
async fn native_vault_restores_device_identity_and_signed_grant() {
    struct Fixture {
        account: String,
        directory: PathBuf,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            if let Ok(entry) = keyring::Entry::new("com.meshshare.terminal.v1", &self.account) {
                let _ = entry.delete_credential();
            }
            let _ = fs::remove_dir_all(&self.directory);
        }
    }
    let account = format!("temporary-vault-test-{}", uuid::Uuid::new_v4());
    let fixture = Fixture {
        directory: std::env::temp_dir().join(&account),
        account,
    };
    eprintln!("Temporary keyring test account: {}", fixture.account);
    fs::create_dir_all(&fixture.directory).unwrap();
    let first = Arc::new(SshService::new(
        fixture.directory.clone(),
        fixture.account.clone(),
    ));
    first.set_enabled(true).await.unwrap();
    let fingerprint = first.state().await.fingerprint.unwrap();
    first.begin_pairing().await.unwrap();
    let peer = PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519)
        .unwrap()
        .public_key()
        .fingerprint(HashAlg::Sha256)
        .to_string();
    let request = tokio::spawn({
        let first = first.clone();
        let peer = peer.clone();
        async move {
            first
                .authorize(
                    Direction::Incoming,
                    "test-peer",
                    &peer,
                    &CancellationToken::new(),
                )
                .await
        }
    });
    approve(&first, Direction::Incoming, &peer).await;
    request.await.unwrap().unwrap();
    first.shutdown();
    drop(first);
    let restored = SshService::new(fixture.directory.clone(), fixture.account.clone());
    let settings = restored.state().await;
    assert!(settings.enabled);
    assert_eq!(settings.fingerprint.as_ref(), Some(&fingerprint));
    assert_eq!(settings.incoming.len(), 1);
    assert_eq!(settings.incoming[0].fingerprint, peer);
    assert!(settings.outgoing.is_empty());
    let entry = keyring::Entry::new("com.meshshare.terminal.v1", &fixture.account).unwrap();
    entry.delete_credential().unwrap();
    assert!(matches!(entry.get_secret(), Err(keyring::Error::NoEntry)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pairing_can_wait_past_the_heartbeat_interval_at_both_fingerprints() {
    let dir = std::env::temp_dir().join(format!("meshshare-ssh-pairing-{}", uuid::Uuid::new_v4()));
    let host = NodeRuntime::new(dir.join("host"), 0, "test".into()).unwrap();
    let local = NodeRuntime::new(dir.join("client"), 0, "test".into()).unwrap();
    let host_key = inject_identity(&host.ssh);
    let client_key = inject_identity(&local.ssh);
    let (address, server) = serve_host(&host, &dir).await;
    let base = format!("http://{address}");
    host.ssh.set_enabled(true).await.unwrap();
    host.ssh.begin_pairing().await.unwrap();
    let host_id = host.node_info().node_id;
    let control = local.ssh.clone();
    let opening = tokio::spawn(async move { control.connect(host_id, vec![base], 80, 25).await });
    for (service, direction, fingerprint) in [
        (&local.ssh, Direction::Outgoing, host_key),
        (&host.ssh, Direction::Incoming, client_key),
    ] {
        let request = next_request(service, direction).await;
        assert_eq!(request.fingerprint, fingerprint);
        tokio::time::sleep(KEEPALIVE + Duration::from_millis(150)).await;
        service
            .decide(&request.id, true)
            .expect("approval remains active until its pairing deadline");
    }
    drop(
        opening
            .await
            .unwrap()
            .expect("SSH shell after both approvals"),
    );
    host.ssh.shutdown();
    local.ssh.shutdown();
    server.abort();
    fs::remove_dir_all(dir).unwrap();
}
