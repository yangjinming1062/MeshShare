use super::*;
use crate::terminal::SessionIo;
use russh::{client, keys::PublicKeyOrCertificate};
use tokio::sync::{mpsc, oneshot};

struct FixtureHost {
    relay: Option<(SessionIo, oneshot::Sender<Result<()>>)>,
}

impl ssh::Handler for FixtureHost {
    type Error = anyhow::Error;

    async fn auth_none(&mut self, _: &str) -> Result<ssh::Auth> {
        Ok(ssh::Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<ssh::Msg>,
        reply: ssh::ChannelOpenHandle,
        session: &mut ssh::Session,
    ) -> Result<()> {
        let (io, done) = self.relay.take().unwrap();
        let handle = session.handle();
        reply.accept().await;
        tokio::spawn(async move {
            let _ = done.send(relay_channel(channel, handle, io).await);
        });
        Ok(())
    }
}

struct FixtureClient(String);

impl client::Handler for FixtureClient {
    type Error = anyhow::Error;

    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool> {
        Ok(
            matches!(key, PublicKeyOrCertificate::PublicKey { key, .. } if key.fingerprint(HashAlg::Sha256).to_string() == self.0),
        )
    }
}

async fn open_relay(
    io: SessionIo,
) -> (
    client::Handle<FixtureClient>,
    Channel<client::Msg>,
    oneshot::Receiver<Result<()>>,
) {
    let key = PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519).unwrap();
    let pin = key.public_key().fingerprint(HashAlg::Sha256).to_string();
    let server = Arc::new(ssh::Config {
        keys: vec![key],
        ..Default::default()
    });
    let (local, remote) = tokio::io::duplex(65536);
    let (done, closed) = oneshot::channel();
    tokio::spawn(async move {
        let host = FixtureHost {
            relay: Some((io, done)),
        };
        let running = ssh::run_stream(server, remote, host).await.unwrap();
        let _ = running.await;
    });
    let mut session = client::connect_stream(Default::default(), local, FixtureClient(pin))
        .await
        .unwrap();
    assert!(session.authenticate_none("test").await.unwrap().success());
    let channel = session.channel_open_session().await.unwrap();
    (session, channel, closed)
}

#[tokio::test]
async fn late_input_allows_the_last_pty_output_and_channel_close() {
    let (input, receiver) = mpsc::channel(1);
    // PTY 输入任务已经退出，最后的屏幕输出仍在读取中。
    drop(receiver);
    let (output, receiver) = mpsc::channel(1);
    let io = SessionIo {
        input,
        output: receiver,
        cancel: CancellationToken::new(),
    };
    let (_session, mut channel, done) = open_relay(io).await;
    channel.window_change(120, 42, 0, 0).await.unwrap();
    channel.data_bytes(vec![3]).await.unwrap();
    // 请求回复作为有序屏障，确保迟到的尺寸修改与按键均先于 PTY 的最后一段输出。
    channel.set_env(true, "unused", "unused").await.unwrap();
    let barrier = tokio::time::timeout(Duration::from_secs(2), channel.wait()).await;
    assert!(
        matches!(barrier, Ok(Some(ChannelMsg::Failure))),
        "relay stopped before draining the PTY"
    );
    output
        .send(Output::Data(b"final line".to_vec()))
        .await
        .unwrap();
    output
        .send(Output::Exit {
            reason: "done".into(),
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        assert!(matches!(channel.wait().await, Some(ChannelMsg::Data { data }) if &data[..] == b"final line"));
        assert!(matches!(channel.wait().await, Some(ChannelMsg::Eof)));
        assert!(matches!(channel.wait().await, Some(ChannelMsg::Close) | None));
        done.await.unwrap().unwrap();
    }).await.expect("the SSH channel must flush and close");
}

#[tokio::test]
async fn peer_closing_before_pty_exit_cancels_the_shell() {
    let (input, _receiver) = mpsc::channel(1);
    let (_keep_pty_alive, output) = mpsc::channel(1);
    let cancel = CancellationToken::new();
    let io = SessionIo {
        input,
        output,
        cancel: cancel.clone(),
    };
    let (_session, channel, done) = open_relay(io).await;
    channel.close().await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), done)
        .await
        .expect("peer close must cancel shell")
        .unwrap()
        .unwrap();
    assert!(cancel.is_cancelled());
}

#[tokio::test]
async fn invalid_resize_reports_channel_end_and_releases_the_pty() {
    let (input, _receiver) = mpsc::channel(1);
    let (_keep_pty_alive, output) = mpsc::channel(1);
    let cancel = CancellationToken::new();
    let io = SessionIo {
        input,
        output,
        cancel: cancel.clone(),
    };
    let (_session, mut channel, done) = open_relay(io).await;
    channel.window_change(u32::MAX, 10, 0, 0).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        assert!(matches!(channel.wait().await, Some(ChannelMsg::Eof)));
        assert!(matches!(
            channel.wait().await,
            Some(ChannelMsg::Close) | None
        ));
        assert!(done.await.unwrap().is_err());
        assert!(cancel.is_cancelled());
    })
    .await
    .expect("invalid channel input must end the session");
}
