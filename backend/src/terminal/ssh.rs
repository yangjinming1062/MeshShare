mod client;
#[cfg(not(target_os = "android"))]
pub(crate) mod migration;
#[cfg(not(target_os = "android"))]
mod server;

use crate::runtime::NodeProfile;
use anyhow::{anyhow, bail, ensure, Result};
use russh::keys::{Algorithm, HashAlg, PrivateKey};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
#[cfg(not(target_os = "android"))]
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

#[cfg(not(test))]
const KEEPALIVE: Duration = Duration::from_secs(15);
#[cfg(test)]
const KEEPALIVE: Duration = Duration::from_secs(1);
const MISSED_HEARTBEATS: u32 = 3;
const OPEN_TIMEOUT: Duration = Duration::from_secs(30);
const UPGRADE: &str = "meshshare-ssh-v3";
const ROUTE: &str = "/api/v1/terminal/ssh";
// SSH 私用扩展数据类型。双方以一个零字节请求和确认前一批输出已进入界面队列。
const OUTPUT_BARRIER: u32 = 0xfe00_0000;

pub(crate) struct SshService {
    node_id: String,
    client_key: Arc<PrivateKey>,
    host_key: Option<PrivateKey>,
    host_ready: watch::Sender<bool>,
    stopped: CancellationToken,
    #[cfg(not(target_os = "android"))]
    sockets: Arc<Semaphore>,
}

impl SshService {
    pub(crate) fn new(node_id: String, profile: NodeProfile, host_ready: bool) -> Result<Self> {
        let new_key = || PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519);
        Ok(Self {
            node_id,
            client_key: Arc::new(new_key()?),
            host_key: if profile.is_android() {
                None
            } else {
                Some(new_key()?)
            },
            host_ready: watch::Sender::new(host_ready && !profile.is_android()),
            stopped: CancellationToken::new(),
            #[cfg(not(target_os = "android"))]
            sockets: Arc::new(Semaphore::new(16)),
        })
    }

    pub(crate) fn client_fingerprint(&self) -> String {
        self.client_key
            .public_key()
            .fingerprint(HashAlg::Sha256)
            .to_string()
    }

    pub(crate) fn host_fingerprint(&self) -> Option<String> {
        self.active_host_key()
            .map(|key| key.public_key().fingerprint(HashAlg::Sha256).to_string())
    }

    fn active_host_key(&self) -> Option<&PrivateKey> {
        self.host_key.as_ref().filter(|_| *self.host_ready.borrow())
    }

    pub(crate) fn subscribe_host(&self) -> watch::Receiver<bool> {
        self.host_ready.subscribe()
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn activate_host(&self) {
        self.host_ready.send_if_modified(|ready| {
            let changed = !*ready;
            *ready = true;
            changed
        });
    }

    pub(crate) fn shutdown(&self) {
        self.stopped.cancel();
    }
}

// 中断传输可同时唤醒 russh 的内部任务和等待终端通道的调用方。
fn cancellable_stream<S>(stream: S, cancel: CancellationToken) -> tokio::io::DuplexStream
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (local, relay) = tokio::io::duplex(65536);
    tokio::spawn(async move {
        let (mut net_read, mut net_write) = tokio::io::split(stream);
        let (mut ssh_read, mut ssh_write) = tokio::io::split(relay);
        tokio::select! {
            _ = cancel.cancelled() => (),
            _ = tokio::io::copy(&mut net_read, &mut ssh_write) => (),
            _ = tokio::io::copy(&mut ssh_read, &mut net_write) => (),
        }
        cancel.cancel();
    });
    local
}

#[cfg(all(test, not(target_os = "android")))]
mod tests;
