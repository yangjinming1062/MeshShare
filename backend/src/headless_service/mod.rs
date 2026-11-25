//! 无界面节点的更新调度和本机服务控制。系统权限只存在于 socket 激活的助手中。
mod helper;
mod release;

use crate::clock::now_millis;
use crate::model::NodeInfo;
use crate::runtime::{NodeRuntime, UpdateFrequency};
use crate::updater::{is_due, UpdateInstallGuard, UpdateState, UpdateStatus};
use anyhow::{ensure, Context, Result};
use helper::Action;
use parking_lot::Mutex;
use semver::Version;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{oneshot, Mutex as AsyncMutex, Notify};

pub(crate) enum RestartError {
    Busy,
    Unavailable(String),
}

type EnableState = Result<bool, String>;

#[derive(Default)]
struct Requests {
    running: bool,
    manual: bool,
}

pub(crate) struct Coordinator {
    runtime: Arc<NodeRuntime>,
    status: Mutex<UpdateStatus>,
    requests: Mutex<Requests>,
    wake: Notify,
    systemd: AsyncMutex<Option<(Instant, EnableState)>>,
}

impl Coordinator {
    pub fn start(runtime: Arc<NodeRuntime>) -> Arc<Self> {
        let status = UpdateStatus {
            state: if runtime.update_settings().0 == UpdateFrequency::Disabled {
                UpdateState::Disabled
            } else {
                UpdateState::Idle
            },
            current_version: runtime.node_info().version,
            available_version: None,
            downloaded_bytes: 0,
            content_length: None,
            error: None,
        };
        let service = Arc::new(Self {
            runtime,
            status: Mutex::new(status),
            requests: Mutex::default(),
            wake: Notify::new(),
            systemd: AsyncMutex::new(None),
        });
        tokio::spawn(service.clone().schedule());
        service
    }

    pub fn status(&self) -> UpdateStatus {
        self.status.lock().clone()
    }

    pub async fn autostart(&self) -> EnableState {
        let mut cache = self.systemd.lock().await;
        if let Some((at, result)) = cache.as_ref() {
            if at.elapsed() < Duration::from_secs(5) {
                return result.clone();
            }
        }
        let result = helper::enabled().await.map_err(|e| format!("{e:#}"));
        *cache = Some((Instant::now(), result.clone()));
        result
    }

    pub async fn set_autostart(&self, enabled: bool) -> Result<(), String> {
        let mut cache = self.systemd.lock().await;
        *cache = None;
        helper::invoke(
            if enabled {
                &Action::Enable
            } else {
                &Action::Disable
            },
            &[],
        )
        .await
        .map_err(|e| format!("{e:#}"))?;
        let actual = helper::enabled().await.map_err(|e| format!("{e:#}"))?;
        *cache = Some((Instant::now(), Ok(actual)));
        if actual != enabled {
            return Err("systemd 未应用新的开机自启状态".into());
        }
        Ok(())
    }

    pub fn request(&self) -> Result<(), String> {
        let mut requests = self.requests.lock();
        if requests.running {
            return Err("更新检查正在进行中".into());
        }
        requests.manual = true;
        self.mark(UpdateState::Checking, None, None);
        self.wake.notify_one();
        Ok(())
    }

    pub fn set_frequency(&self, frequency: UpdateFrequency) -> Result<(), String> {
        self.runtime
            .set_update_frequency(frequency)
            .map_err(|e| e.to_string())?;
        let requests = self.requests.lock();
        if !requests.running && !requests.manual {
            if frequency == UpdateFrequency::Disabled {
                self.mark(UpdateState::Disabled, None, None);
            } else if self.status.lock().state == UpdateState::Disabled {
                self.mark(UpdateState::Idle, None, None);
            }
        }
        self.wake.notify_one();
        Ok(())
    }

    pub async fn restart(self: &Arc<Self>) -> Result<(), RestartError> {
        // 后台任务持有维护锁，HTTP 调用者断开时它仍会继续执行。
        let runtime = self.runtime.clone();
        let (result, response) = oneshot::channel();
        tokio::spawn(async move {
            let Some(_guard) = UpdateInstallGuard::acquire(&runtime) else {
                let _ = result.send(Err(RestartError::Busy));
                return;
            };
            match helper::invoke(&Action::Restart, &[]).await {
                Ok(()) => {
                    let _ = result.send(Ok(()));
                    tokio::time::sleep(Duration::from_secs(25)).await;
                    if !runtime.stopping.load(Ordering::SeqCst) {
                        eprintln!("systemd 已接受重启，但旧节点仍在运行");
                    }
                }
                Err(error) => {
                    let _ = result.send(Err(RestartError::Unavailable(format!("{error:#}"))));
                }
            }
        });
        response
            .await
            .unwrap_or_else(|_| Err(RestartError::Unavailable("重启任务意外结束".into())))
    }

    fn mark(&self, state: UpdateState, version: Option<&Version>, error: Option<String>) {
        let mut status = self.status.lock();
        status.state = state;
        status.available_version = version.map(ToString::to_string);
        status.error = error;
        status.downloaded_bytes = 0;
        status.content_length = None;
    }

    async fn schedule(self: Arc<Self>) {
        let mut retry_at = None;
        let mut deferred_manual = false;
        while !self.runtime.stopping.load(Ordering::SeqCst) {
            let now = now_millis();
            let (frequency, last, pending) = self.runtime.update_settings();
            let manual = {
                let mut requests = self.requests.lock();
                let manual =
                    requests.manual || deferred_manual && retry_at.is_some_and(|at| now >= at);
                if is_due(frequency, last, pending.is_some(), retry_at, now, manual) {
                    requests.manual = false;
                    requests.running = true;
                    Some(manual)
                } else {
                    None
                }
            };
            if let Some(manual) = manual {
                match self.check(manual).await {
                    Ok(Check::Deferred) => {
                        deferred_manual = manual;
                        retry_at = Some(now_millis() + 15_000);
                    }
                    Ok(Check::Done) => {
                        deferred_manual = false;
                        retry_at = None;
                    }
                    Err(error) => {
                        let reason = format!("{error:#}");
                        eprintln!("headless 更新失败：{reason}");
                        self.mark(UpdateState::Error, None, Some(reason));
                        deferred_manual = false;
                        retry_at = Some(now_millis() + 3_600_000);
                    }
                }
                self.requests.lock().running = false;
                continue;
            }
            tokio::select! {
                _ = self.wake.notified() => {}
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
            }
        }
    }

    fn automatic_check_cancelled(&self, manual: bool) -> bool {
        let disabled = !manual && self.runtime.update_settings().0 == UpdateFrequency::Disabled;
        if disabled {
            self.mark(UpdateState::Disabled, None, None);
        }
        disabled
    }

    async fn check(&self, manual: bool) -> Result<Check> {
        if self.automatic_check_cancelled(manual) {
            return Ok(Check::Done);
        }
        self.mark(UpdateState::Checking, None, None);
        let client = reqwest::Client::builder()
            .https_only(true)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(300))
            .build()?;
        let manifest = tokio::time::timeout(
            Duration::from_secs(25),
            release::download(
                &client,
                &release::endpoint()?,
                release::MAX_MANIFEST,
                |_, _| {},
            ),
        )
        .await??;
        let current = Version::parse(&self.status.lock().current_version)?;
        let Some((version, asset)) = release::candidate(&manifest, &current)? else {
            self.runtime
                .set_update_check_state(Some(now_millis()), None)?;
            self.mark(UpdateState::UpToDate, None, None);
            return Ok(Check::Done);
        };
        self.runtime
            .set_update_check_state(Some(now_millis()), Some(version.to_string()))?;
        if self.automatic_check_cancelled(manual) {
            return Ok(Check::Done);
        }
        let defer = || {
            self.mark(
                UpdateState::Deferred,
                Some(&version),
                Some("等待文件准备、传输或终端会话结束".into()),
            );
            Ok(Check::Deferred)
        };
        if self.runtime.update_has_active_work() {
            return defer();
        }
        self.mark(UpdateState::Downloading, Some(&version), None);
        let bytes = release::download(&client, &asset.url, release::MAX_DEB, |received, total| {
            let mut status = self.status.lock();
            status.downloaded_bytes = received;
            status.content_length = total;
        })
        .await?;
        release::verify(&bytes, &asset.signature, &version)?;
        if self.automatic_check_cancelled(manual) {
            return Ok(Check::Done);
        }
        let Some(_guard) = UpdateInstallGuard::acquire(&self.runtime) else {
            return defer();
        };
        self.mark(UpdateState::Installing, Some(&version), None);
        helper::invoke(
            &Action::Install {
                size: bytes.len(),
                version,
                signature: asset.signature,
            },
            &bytes,
        )
        .await?;
        anyhow::bail!("系统安装已返回，但旧节点仍在运行；请检查 systemd 状态")
    }
}

enum Check {
    Done,
    Deferred,
}

pub(crate) async fn cli(command: &str) -> Result<()> {
    if command == "helper" {
        return helper::run().await;
    }
    ensure!(
        matches!(command, "status" | "update"),
        "未知命令：{command}"
    );
    let (client, base, node) = local_node().await?;
    let response = if command == "update" {
        client.post(format!("{base}/settings/update-check"))
    } else {
        client.get(format!("{base}/settings"))
    }
    .send()
    .await?;
    let code = response.status();
    let bytes = release::bounded_body(response, release::MAX_MANIFEST, |_, _| {}).await?;
    let settings: serde_json::Value = serde_json::from_slice(&bytes)?;
    ensure!(code.is_success(), "{code}: {settings}");
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "node_id":node.node_id,"node_name":node.node_name,"version":node.version,
            "port":node.port,"addresses":node.addresses,
            "autostart_enabled":settings["autostart_enabled"],"service_control_error":settings["service_control_error"],
            "update_frequency":settings["update_frequency"],"update_status":settings["update_status"]
        }))?
    );
    Ok(())
}

/// 该记录由低权限服务账户写入。即使以 root 检查也只读取有界的普通文件，
/// 并只在回环上查询固定的只读接口。
async fn local_node() -> Result<(reqwest::Client, String, NodeInfo)> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(crate::headless::PORT_FILE)
        .context("无法找到运行中的 headless 节点")?;
    ensure!(file.metadata()?.is_file(), "无效的服务端口文件");
    let mut text = String::new();
    file.take(8).read_to_string(&mut text)?;
    ensure!(text.len() <= 6, "无效的服务端口文件");
    let port: u16 = text.trim().parse()?;
    let base = format!("http://127.0.0.1:{port}/api/v1");
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()?;
    let bytes = release::download(&client, &format!("{base}/info"), 65536, |_, _| {}).await?;
    let node: NodeInfo = serde_json::from_slice(&bytes)?;
    ensure!(
        node.capabilities.iter().any(|c| c == "node.headless.v1"),
        "目标并非 headless 节点"
    );
    Ok((client, base, node))
}
