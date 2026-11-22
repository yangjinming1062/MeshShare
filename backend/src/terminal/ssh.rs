mod client;
mod server;
mod trust;

use crate::clock::now_millis;
use anyhow::{anyhow, bail, ensure, Context, Result};
use parking_lot::Mutex;
use russh::keys::{HashAlg, PrivateKey};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{oneshot, OnceCell, Semaphore};
use tokio_util::sync::CancellationToken;

const PAIR_WINDOW: Duration = Duration::from_secs(120);
#[cfg(not(test))]
const KEEPALIVE: Duration = Duration::from_secs(15);
#[cfg(test)]
const KEEPALIVE: Duration = Duration::from_secs(1);
const MISSED_HEARTBEATS: u32 = 3;
const UPGRADE: &str = "meshshare-ssh-v2";
const ROUTE: &str = "/api/v1/terminal/ssh";
// SSH 私用扩展数据类型。双方以一个零字节请求和确认前一批输出已进入界面队列。
const OUTPUT_BARRIER: u32 = 0xfe00_0000;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Direction {
    Incoming,
    Outgoing,
}

#[derive(Default, Clone, Serialize, Deserialize)]
struct Policy {
    enabled: bool,
    incoming: BTreeMap<String, String>,
    outgoing: BTreeMap<String, String>,
}

impl Policy {
    fn grants(&self, direction: Direction) -> &BTreeMap<String, String> {
        match direction {
            Direction::Incoming => &self.incoming,
            Direction::Outgoing => &self.outgoing,
        }
    }
    fn grants_mut(&mut self, direction: Direction) -> &mut BTreeMap<String, String> {
        match direction {
            Direction::Incoming => &mut self.incoming,
            Direction::Outgoing => &mut self.outgoing,
        }
    }
}

#[derive(Serialize)]
struct Device {
    node_id: String,
    fingerprint: String,
}

#[derive(Clone, Serialize)]
struct Request {
    id: String,
    node_id: String,
    direction: Direction,
    fingerprint: String,
}

struct Approval {
    request: Request,
    expires: Instant,
    answer: oneshot::Sender<bool>,
}

#[derive(Default, Serialize)]
pub(crate) struct Settings {
    pub enabled: bool,
    fingerprint: Option<String>,
    error: Option<String>,
    pairing_until_ms: Option<i64>,
    incoming: Vec<Device>,
    outgoing: Vec<Device>,
    pending: Vec<Request>,
}

#[derive(Default)]
struct Access {
    policy: Policy,
    window: Option<Instant>,
    pending: BTreeMap<String, Approval>,
    leases: HashMap<(Direction, String), CancellationToken>,
    listener: CancellationToken,
}

pub(crate) struct SshService {
    data_dir: PathBuf,
    restore_enabled: bool,
    node_id: String,
    key: OnceCell<Arc<PrivateKey>>,
    key_creation: Arc<tokio::sync::Mutex<()>>,
    access: Mutex<Access>,
    stopped: CancellationToken,
    sockets: Arc<Semaphore>,
}

impl SshService {
    pub(crate) fn new(data_dir: PathBuf, node_id: String) -> Self {
        Self {
            restore_enabled: trust::stored_host_enabled(&data_dir),
            data_dir,
            node_id,
            key: OnceCell::new(),
            key_creation: Arc::default(),
            access: Mutex::default(),
            stopped: CancellationToken::new(),
            sockets: Arc::new(Semaphore::new(16)),
        }
    }

    async fn identity(&self) -> Result<&Arc<PrivateKey>> {
        self.key
            .get_or_try_init(|| async {
                let node_id = self.node_id.clone();
                let dir = self.data_dir.clone();
                let guard = self.key_creation.clone().lock_owned().await;
                // 取消等待不会取消已经开始的凭据库操作；序列化读取和首次创建。
                let (key, policy) = tokio::task::spawn_blocking(move || {
                    let _guard = guard;
                    trust::load(&dir, &node_id)
                })
                .await??;
                self.access.lock().policy = policy;
                Ok(Arc::new(key))
            })
            .await
    }

    pub(crate) async fn state(&self) -> Settings {
        let key = match self.identity().await {
            Ok(key) => key,
            Err(error) => {
                return Settings {
                    error: Some(format!("{error:#}")),
                    ..Default::default()
                }
            }
        };
        let access = self.access.lock();
        let devices = |grants: &BTreeMap<String, String>| {
            grants
                .iter()
                .map(|(node_id, fingerprint)| Device {
                    node_id: node_id.clone(),
                    fingerprint: fingerprint.clone(),
                })
                .collect()
        };
        let now = Instant::now();
        Settings {
            enabled: access.policy.enabled,
            fingerprint: Some(key.public_key().fingerprint(HashAlg::Sha256).to_string()),
            error: None,
            pairing_until_ms: access
                .window
                .filter(|end| *end > now)
                .map(|end| now_millis() + (end - now).as_millis() as i64),
            incoming: devices(&access.policy.incoming),
            outgoing: devices(&access.policy.outgoing),
            pending: access
                .pending
                .values()
                .filter(|p| p.expires > now && !p.answer.is_closed())
                .map(|p| p.request.clone())
                .collect(),
        }
    }

    fn save(&self, access: &mut Access, policy: Policy) -> Result<()> {
        trust::save(
            &self.data_dir,
            self.key.get().context("终端密钥尚未加载")?,
            &policy,
        )?;
        access.policy = policy;
        Ok(())
    }

    pub(crate) async fn set_enabled(&self, enabled: bool) -> Result<()> {
        self.identity().await?;
        let mut access = self.access.lock();
        let mut policy = access.policy.clone();
        policy.enabled = enabled;
        self.save(&mut access, policy)?;
        if !enabled {
            access.listener.cancel();
            access.listener = CancellationToken::new();
            access.window = None;
            access
                .pending
                .retain(|_, p| p.request.direction != Direction::Incoming);
            access.leases.retain(|(d, _), token| {
                if *d == Direction::Incoming {
                    token.cancel();
                    false
                } else {
                    true
                }
            });
        }
        Ok(())
    }

    pub(crate) async fn begin_pairing(&self) -> Result<()> {
        self.identity().await?;
        let mut access = self.access.lock();
        ensure!(access.policy.enabled, "请先在本机开启远程终端");
        access
            .pending
            .retain(|_, p| p.request.direction != Direction::Incoming);
        access.window = Some(Instant::now() + PAIR_WINDOW);
        Ok(())
    }

    pub(crate) fn decide(&self, id: &str, allow: bool) -> Result<()> {
        let mut access = self.access.lock();
        let approval = access.pending.remove(id).context("配对请求已结束")?;
        let request = &approval.request;
        let now = Instant::now();
        ensure!(
            approval.expires > now && !approval.answer.is_closed(),
            "配对请求已过期"
        );
        if allow {
            if request.direction == Direction::Incoming {
                ensure!(
                    access.policy.enabled && access.window.is_some_and(|end| end > now),
                    "配对窗口已关闭"
                );
            }
            let mut policy = access.policy.clone();
            let grants = policy.grants_mut(request.direction);
            ensure!(
                grants
                    .get(&request.node_id)
                    .is_none_or(|pin| pin == &request.fingerprint),
                "设备密钥已更换，请先撤销旧配对"
            );
            grants.insert(request.node_id.clone(), request.fingerprint.clone());
            self.save(&mut access, policy)?;
            if request.direction == Direction::Incoming {
                access.window = None;
                access
                    .pending
                    .retain(|_, p| p.request.direction != Direction::Incoming);
            }
        }
        let _ = approval.answer.send(allow);
        Ok(())
    }

    pub(crate) fn revoke(&self, direction: Direction, node_id: &str) -> Result<()> {
        let mut access = self.access.lock();
        let mut policy = access.policy.clone();
        policy.grants_mut(direction).remove(node_id);
        self.save(&mut access, policy)?;
        if let Some(token) = access.leases.remove(&(direction, node_id.to_owned())) {
            token.cancel();
        }
        access
            .pending
            .retain(|_, p| p.request.direction != direction || p.request.node_id != node_id);
        Ok(())
    }

    async fn authorize(
        &self,
        direction: Direction,
        node_id: &str,
        fingerprint: &str,
        cancelled: &CancellationToken,
    ) -> Result<CancellationToken> {
        ensure!(
            !node_id.is_empty() && node_id.len() <= 256 && !node_id.chars().any(char::is_control),
            "无效设备身份"
        );
        let (id, receiver, expires) = {
            let mut access = self.access.lock();
            if direction == Direction::Incoming {
                ensure!(access.policy.enabled, "远程终端已关闭");
            }
            if let Some(pin) = access.policy.grants(direction).get(node_id) {
                ensure!(pin == fingerprint, "设备密钥已更换，请核对身份并撤销旧配对");
                return Ok(access
                    .leases
                    .entry((direction, node_id.to_owned()))
                    .or_default()
                    .clone());
            }
            let now = Instant::now();
            let expires = if direction == Direction::Incoming {
                access
                    .window
                    .filter(|end| *end > now)
                    .context("请在目标机开启配对窗口")?
            } else {
                now + PAIR_WINDOW
            };
            access
                .pending
                .retain(|_, p| p.expires > now && !p.answer.is_closed());
            ensure!(
                access.pending.len() < 8
                    && !access
                        .pending
                        .values()
                        .any(|p| p.request.direction == direction && p.request.node_id == node_id),
                "已有待处理的配对请求"
            );
            let (answer, receiver) = oneshot::channel();
            let id = uuid::Uuid::new_v4().to_string();
            let request = Request {
                id: id.clone(),
                direction,
                node_id: node_id.to_owned(),
                fingerprint: fingerprint.to_owned(),
            };
            access.pending.insert(
                id.clone(),
                Approval {
                    request,
                    expires,
                    answer,
                },
            );
            (id, receiver, expires)
        };
        let accepted = tokio::select! {
            _ = cancelled.cancelled() => false,
            _ = self.stopped.cancelled() => false,
            result = tokio::time::timeout_at(expires.into(),receiver) => matches!(result,Ok(Ok(true))),
        };
        let mut access = self.access.lock();
        access.pending.remove(&id);
        ensure!(
            accepted && !cancelled.is_cancelled() && !self.stopped.is_cancelled(),
            "配对已拒绝、取消或超时"
        );
        ensure!(
            direction != Direction::Incoming || access.policy.enabled,
            "远程终端已关闭"
        );
        ensure!(
            access
                .policy
                .grants(direction)
                .get(node_id)
                .is_some_and(|pin| pin == fingerprint),
            "配对已撤销"
        );
        Ok(access
            .leases
            .entry((direction, node_id.to_owned()))
            .or_default()
            .clone())
    }

    pub(crate) fn shutdown(&self) {
        self.stopped.cancel();
        let mut access = self.access.lock();
        access.listener.cancel();
        access.pending.clear();
        for token in access.leases.values() {
            token.cancel();
        }
    }
}

// russh 内部持有 SSH 任务。主动关闭传输可打断等待中的 I/O，
// 对端提前关闭时也要立即清理仍在等待本机确认的授权请求。
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

fn bind_grant(grant: CancellationToken, connection: CancellationToken) {
    tokio::spawn(async move {
        tokio::select! { _ = grant.cancelled() => connection.cancel(), _ = connection.cancelled() => () }
    });
}

#[cfg(test)]
mod tests;
