use super::release::{Manifest, Release};
use crate::clock::now_millis;
use crate::model::WireMessage;
use crate::runtime::{NodeRuntime, UpdateFrequency};
use crate::updater::UpdateInstallGuard;
use anyhow::{ensure, Context, Result};
use parking_lot::Mutex;
use reqwest::{redirect, Client, Url};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::ipc::Channel;
use tauri::plugin::{PluginHandle, TauriPlugin};
use tauri::{AppHandle, Emitter, Manager, State, Wry};
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{broadcast, watch, Notify};

const ENABLED_BUILD: bool = cfg!(all(not(debug_assertions), target_arch = "aarch64"));
const RELEASES_URL: &str = "https://github.com/yangjinming1062/MeshShare/releases";
const RELEASES_PATH: &str = "/yangjinming1062/MeshShare/releases/";
const STATUS_EVENT: &str = "android-update-status";
const CHECK_TIMEOUT: Duration = Duration::from_secs(20);
const RETRY_MS: i64 = 60 * 60 * 1000;
const MANIFEST_LIMIT: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Unavailable,
    Idle,
    Disabled,
    Checking,
    NoUpdate,
    WaitingForTransfers,
    WaitingForNetwork,
    Metered,
    Downloading,
    Permission,
    Installing,
    Paused,
    Error,
}

impl Phase {
    fn busy(self) -> bool {
        matches!(self, Self::Checking | Self::Downloading | Self::Installing)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Status {
    state: Phase,
    frequency: UpdateFrequency,
    current_version: String,
    available_version: Option<String>,
    downloaded_bytes: u64,
    content_length: Option<u64>,
    error: Option<String>,
}

#[derive(Default, Clone, Serialize, Deserialize)]
struct Preferences {
    frequency: UpdateFrequency,
    checked_at_ms: Option<i64>,
    pending: Option<Release>,
    paused_version_code: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Network {
    #[default]
    Offline,
    Metered,
    Unmetered,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, Deserialize)]
struct Environment {
    foreground: bool,
    network: Network,
    can_install: bool,
    version_code: u64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
enum InstallResult {
    Installed,
    Deferred,
    Permission,
    Cancelled,
    InvalidApk { message: String },
    Failed { message: String },
}

#[derive(Clone, Copy)]
enum Request {
    Check,
    Retry,
    DownloadOnMeteredNetwork(u64),
    Permission,
}

#[derive(Default)]
struct Work {
    check_now: bool,
    manual: bool,
    metered_version_code: Option<u64>,
    network: Network,
    retry_at_ms: i64,
    verified: Option<Release>,
}

struct Shared {
    prefs: Preferences,
    status: Status,
    request: Option<Request>,
}

pub(crate) struct AndroidUpdater {
    app: AppHandle,
    runtime: Arc<NodeRuntime>,
    platform: PluginHandle<Wry>,
    client: Client,
    config_path: PathBuf,
    cache: PathBuf,
    version: Version,
    shared: Mutex<Shared>,
    wake: Notify,
}

/// Android 插件句柄按插件分别包装托管。`App::manage` 对同一类型只保留第一个状态，
/// 直接托管 `PluginHandle` 会让后注册的插件拿到前一个插件的句柄。
pub(crate) struct AndroidUpdatePluginHandle(pub(crate) PluginHandle<Wry>);

pub(crate) fn plugin() -> TauriPlugin<Wry> {
    tauri::plugin::Builder::new("android-update")
        .setup(|app, api| {
            let handle =
                api.register_android_plugin("com.meshshare.android.update", "UpdatePlugin")?;
            if !app.manage(AndroidUpdatePluginHandle(handle)) {
                return Err("Android 更新插件句柄已存在".into());
            }
            Ok(())
        })
        .build()
}

pub(crate) fn start(app: &AppHandle, runtime: Arc<NodeRuntime>, data_dir: &Path) -> Result<()> {
    let config_path = data_dir.join("android-update.json");
    let prefs = match std::fs::read(&config_path) {
        // 一个坏字节不该让整块更新功能不可用：损坏时按默认设置启动。
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            eprintln!("Android 更新设置损坏，已使用默认值：{error}");
            Preferences::default()
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Preferences::default(),
        Err(error) => return Err(error).context("打开 Android 更新设置失败"),
    };
    let version = app.package_info().version.clone();
    let status = Status {
        state: if ENABLED_BUILD {
            Phase::Idle
        } else {
            Phase::Unavailable
        },
        frequency: prefs.frequency,
        current_version: version.to_string(),
        available_version: None,
        downloaded_bytes: 0,
        content_length: None,
        error: None,
    };
    let client = Client::builder()
        .https_only(true)
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(45))
        .timeout(Duration::from_secs(15 * 60))
        .redirect(redirect::Policy::custom(|attempt| {
            if attempt.previous().len() < 5 && trusted_redirect(attempt.url()) {
                attempt.follow()
            } else {
                attempt.error("Android 更新重定向到了不受信任的地址")
            }
        }))
        .build()?;
    let updater = Arc::new(AndroidUpdater {
        app: app.clone(),
        runtime,
        platform: app.state::<AndroidUpdatePluginHandle>().0.clone(),
        client,
        config_path,
        cache: app.path().app_cache_dir()?.join("android-update"),
        version,
        shared: Mutex::new(Shared {
            prefs,
            status,
            request: None,
        }),
        wake: Notify::new(),
    });
    app.manage(updater.clone());
    if ENABLED_BUILD {
        tauri::async_runtime::spawn(updater.run());
    }
    Ok(())
}

fn trusted_redirect(url: &Url) -> bool {
    url.scheme() == "https"
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && match url.host_str() {
            Some("github.com") => url.path().starts_with(RELEASES_PATH),
            Some("release-assets.githubusercontent.com" | "objects.githubusercontent.com") => true,
            _ => false,
        }
}

#[tauri::command]
pub(crate) fn android_update_status(state: State<'_, Arc<AndroidUpdater>>) -> Status {
    state.shared.lock().status.clone()
}

#[tauri::command]
pub(crate) fn android_set_update_frequency(
    state: State<'_, Arc<AndroidUpdater>>,
    frequency: UpdateFrequency,
) -> Result<UpdateFrequency, String> {
    state.ensure_available()?;
    state
        .update_preferences(|prefs| prefs.frequency = frequency)
        .map_err(|error| error.to_string())?;
    state.wake.notify_one();
    Ok(frequency)
}

#[tauri::command]
pub(crate) fn android_check_for_updates(
    state: State<'_, Arc<AndroidUpdater>>,
) -> Result<(), String> {
    state.queue(Some(Request::Check))
}

#[tauri::command]
pub(crate) fn android_continue_update(state: State<'_, Arc<AndroidUpdater>>) -> Result<(), String> {
    state.queue(None)
}

impl AndroidUpdater {
    fn ensure_available(&self) -> Result<(), String> {
        if self.shared.lock().status.state == Phase::Unavailable {
            Err("此构建无法使用 Android 应用更新".into())
        } else {
            Ok(())
        }
    }

    fn queue(&self, request: Option<Request>) -> Result<(), String> {
        self.ensure_available()?;
        let mut shared = self.shared.lock();
        if shared.status.state.busy() || shared.request.is_some() {
            return Err("更新正在进行中".into());
        }
        let request = match (request, shared.status.state) {
            (Some(request), _) => request,
            (None, Phase::Permission) => Request::Permission,
            (None, Phase::Metered) => {
                let release = shared.prefs.pending.as_ref().ok_or("没有待下载的更新")?;
                Request::DownloadOnMeteredNetwork(release.apk.version_code)
            }
            (None, Phase::Paused | Phase::Error) => Request::Retry,
            (None, _) => return Err("当前没有需要继续的更新".into()),
        };
        shared.request = Some(request);
        self.wake.notify_one();
        Ok(())
    }

    fn update_preferences(&self, change: impl FnOnce(&mut Preferences)) -> Result<()> {
        let mut shared = self.shared.lock();
        let mut next = shared.prefs.clone();
        change(&mut next);
        crate::fsutil::atomic_write(&self.config_path, &serde_json::to_vec(&next)?)
            .context("保存 Android 更新设置失败")?;
        shared.status.frequency = next.frequency;
        shared.prefs = next;
        let status = shared.status.clone();
        drop(shared);
        let _ = self.app.emit(STATUS_EVENT, &status);
        Ok(())
    }

    fn set_status(&self, state: Phase, downloaded_bytes: u64, error: Option<String>) {
        let mut shared = self.shared.lock();
        let pending = shared.prefs.pending.as_ref();
        let status = Status {
            state,
            frequency: shared.prefs.frequency,
            current_version: self.version.to_string(),
            available_version: pending.map(|release| release.version.to_string()),
            downloaded_bytes,
            content_length: pending.map(|release| release.apk.size),
            error,
        };
        if shared.status == status {
            return;
        }
        shared.status = status;
        let status = shared.status.clone();
        // 事件在锁外发出，监听器同步回调命令时不会自锁。
        drop(shared);
        let _ = self.app.emit(STATUS_EVENT, &status);
    }

    fn phase(&self, state: Phase) {
        self.set_status(state, 0, None);
    }

    async fn run(self: Arc<Self>) {
        let (send_environment, mut environment) = watch::channel(Environment::default());
        let channel = Channel::<Environment>::new(move |message| {
            let next = message.deserialize::<Environment>()?;
            send_environment.send_if_modified(|current| {
                let changed = *current != next;
                *current = next;
                changed
            });
            Ok(())
        });
        let subscription = self
            .platform
            .run_mobile_plugin_async::<()>("observe", serde_json::json!({ "channel": &channel }));
        if let Err(error) = tokio::time::timeout(CHECK_TIMEOUT, subscription)
            .await
            .context("读取 Android 更新环境超时")
            .and_then(|result| result.context("读取 Android 更新环境失败"))
        {
            self.set_status(Phase::Unavailable, 0, Some(format!("{error:#}")));
            return;
        }
        self.remove_cache_file("update.part").await;
        let has_pending = self.shared.lock().prefs.pending.is_some();
        if !has_pending {
            self.remove_cache_file("update.apk").await;
        }
        let mut work = Work::default();
        let mut transfers = self.runtime.subscribe();
        loop {
            let request = self.shared.lock().request.take();
            if let Some(Request::Permission) = request {
                let settings = self
                    .platform
                    .run_mobile_plugin_async::<()>("openInstallSettings", ());
                if let Err(error) = tokio::time::timeout(CHECK_TIMEOUT, settings)
                    .await
                    .context("打开系统安装权限设置超时")
                    .and_then(|result| result.context("打开系统安装权限设置失败"))
                {
                    self.set_status(Phase::Permission, 0, Some(format!("{error:#}")));
                }
            } else {
                let context = *environment.borrow_and_update();
                if work.network == Network::Offline && context.network != Network::Offline {
                    work.retry_at_ms = 0;
                }
                work.network = context.network;
                if let Some(request) = request {
                    work.manual = true;
                    work.check_now = matches!(request, Request::Check)
                        || self.shared.lock().prefs.pending.is_none();
                    work.metered_version_code = match request {
                        Request::DownloadOnMeteredNetwork(code) => Some(code),
                        _ => None,
                    };
                    work.retry_at_ms = 0;
                }
                if context.foreground {
                    if let Err(error) = self.advance(&mut environment, context, &mut work).await {
                        work.retry_at_ms = now_millis().saturating_add(RETRY_MS);
                        work.manual = false;
                        work.metered_version_code = None;
                        self.set_status(Phase::Error, 0, Some(format!("{error:#}")));
                    }
                }
            }

            let foreground = environment.borrow().foreground;
            let waiting =
                foreground && self.shared.lock().status.state == Phase::WaitingForTransfers;
            tokio::select! {
                _ = self.wake.notified() => {},
                result = environment.changed() => if result.is_err() { break; },
                _ = transfer_change(&mut transfers), if waiting => {},
                _ = tokio::time::sleep(Duration::from_secs(if waiting { 2 } else { 60 })), if foreground => {},
            }
        }
    }

    async fn advance(
        &self,
        environment: &mut watch::Receiver<Environment>,
        context: Environment,
        work: &mut Work,
    ) -> Result<()> {
        let mut prefs = self.shared.lock().prefs.clone();
        if let Some(release) = &prefs.pending {
            if !release.is_newer_than(&self.version, context.version_code)? {
                self.update_preferences(|prefs| {
                    prefs.pending = None;
                    prefs.paused_version_code = None;
                })?;
                prefs = self.shared.lock().prefs.clone();
                self.remove_cache_file("update.apk").await;
            }
        }
        if !work.manual && prefs.frequency == UpdateFrequency::Disabled {
            if !matches!(
                self.shared.lock().status.state,
                Phase::Error | Phase::Paused
            ) {
                self.phase(Phase::Disabled);
            }
            return Ok(());
        }
        let now = now_millis();
        if now < work.retry_at_ms {
            return Ok(());
        }
        let due = work.check_now
            || prefs.frequency.period_ms().is_some_and(|period| {
                prefs
                    .checked_at_ms
                    .is_none_or(|at| now < at || now.saturating_sub(at) >= period)
            });
        if due && context.network != Network::Offline {
            self.phase(Phase::Checking);
            let release =
                tokio::time::timeout(CHECK_TIMEOUT, self.check_release(context.version_code))
                    .await
                    .context("检查 Android 更新超时")??;
            self.update_preferences(|prefs| {
                if prefs.pending != release {
                    prefs.paused_version_code = None;
                }
                prefs.pending = release;
                prefs.checked_at_ms = Some(now_millis());
            })?;
            prefs = self.shared.lock().prefs.clone();
            work.check_now = false;
            if prefs.pending.is_none() {
                work.manual = false;
                work.metered_version_code = None;
                work.verified = None;
                self.remove_cache_file("update.apk").await;
                self.phase(Phase::NoUpdate);
                return Ok(());
            }
        } else if due && (work.check_now || prefs.pending.is_none()) {
            self.phase(Phase::WaitingForNetwork);
            return Ok(());
        }

        let Some(release) = prefs.pending else {
            self.phase(Phase::NoUpdate);
            return Ok(());
        };
        if prefs.paused_version_code == Some(release.apk.version_code) && !work.manual {
            if !matches!(
                self.shared.lock().status.state,
                Phase::Error | Phase::Paused
            ) {
                self.phase(Phase::Paused);
            }
            return Ok(());
        }
        if !self.allowed(*environment.borrow(), work.manual) {
            self.phase(if environment.borrow().foreground {
                Phase::Disabled
            } else {
                Phase::Idle
            });
            return Ok(());
        }
        if self.runtime.has_active_transfers() {
            self.phase(Phase::WaitingForTransfers);
            return Ok(());
        }

        let cached = self.cache.join("update.apk");
        let verified = work.verified.as_ref() == Some(&release) && fs::try_exists(&cached).await?;
        if !verified && !verify_cache(&cached, &release).await? {
            let context = *environment.borrow();
            if !self.allowed(context, work.manual) {
                self.wake.notify_one();
                return Ok(());
            }
            if context.network == Network::Offline {
                self.phase(Phase::WaitingForNetwork);
                return Ok(());
            }
            let allow_metered = work.metered_version_code == Some(release.apk.version_code);
            if context.network == Network::Metered && !allow_metered {
                self.phase(Phase::Metered);
                return Ok(());
            }
            if self.runtime.has_active_transfers() {
                self.phase(Phase::WaitingForTransfers);
                return Ok(());
            }
            if !self
                .download(&release, environment, work.manual, allow_metered)
                .await?
            {
                self.phase(Phase::Idle);
                self.wake.notify_one();
                return Ok(());
            }
        }
        work.verified = Some(release.clone());
        let context = *environment.borrow();
        if !self.allowed(context, work.manual) {
            self.phase(if context.foreground {
                Phase::Disabled
            } else {
                Phase::Idle
            });
            return Ok(());
        }
        if !context.can_install {
            self.phase(Phase::Permission);
            return Ok(());
        }
        self.install(&release, work).await
    }

    fn allowed(&self, context: Environment, manual: bool) -> bool {
        context.foreground
            && (manual || self.shared.lock().prefs.frequency != UpdateFrequency::Disabled)
    }

    async fn check_release(&self, version_code: u64) -> Result<Option<Release>> {
        let mut response = self
            .client
            .get(format!("{RELEASES_URL}/latest/download/latest.json"))
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(reqwest::Error::without_url)
            .context("无法获取 GitHub 稳定版更新清单")?;
        let mut body = Vec::new();
        while let Some(bytes) = response
            .chunk()
            .await
            .map_err(reqwest::Error::without_url)?
        {
            ensure!(body.len() + bytes.len() <= MANIFEST_LIMIT, "更新清单过大");
            body.extend_from_slice(&bytes);
        }
        let manifest: Manifest = serde_json::from_slice(&body).context("更新清单格式无效")?;
        manifest.select(&self.version, version_code)
    }

    async fn download(
        &self,
        release: &Release,
        environment: &mut watch::Receiver<Environment>,
        manual: bool,
        allow_metered: bool,
    ) -> Result<bool> {
        fs::create_dir_all(&self.cache)
            .await
            .context("创建 APK 缓存目录失败")?;
        let partial = self.cache.join("update.part");
        self.phase(Phase::Downloading);
        let result = {
            let download = self.write_apk(release, &partial);
            tokio::pin!(download);
            loop {
                tokio::select! {
                    result = &mut download => break result.map(|()| true),
                    _ = self.wake.notified() => {},
                    result = environment.changed() => if result.is_err() { break Ok(false); },
                }
                let context = *environment.borrow();
                if !self.allowed(context, manual)
                    || context.network == Network::Offline
                    || (context.network == Network::Metered && !allow_metered)
                {
                    break Ok(false);
                }
            }
        };
        if matches!(result, Ok(true)) {
            if let Err(error) = fs::rename(&partial, self.cache.join("update.apk")).await {
                self.remove_cache_file("update.part").await;
                return Err(error).context("保存 APK 缓存失败");
            }
        } else {
            self.remove_cache_file("update.part").await;
        }
        result
    }

    async fn write_apk(&self, release: &Release, partial: &Path) -> Result<()> {
        let version = &release.version;
        let mut response = self
            .client
            .get(format!(
                "{RELEASES_URL}/download/v{version}/MeshShare_{version}_android_arm64.apk"
            ))
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(reqwest::Error::without_url)
            .context("无法下载 Android 更新包")?;
        ensure!(
            response
                .content_length()
                .is_none_or(|size| size == release.apk.size),
            "APK 大小和更新清单不一致"
        );
        let mut file = fs::File::create(partial)
            .await
            .context("创建 APK 临时文件失败")?;
        let mut hasher = Sha256::new();
        let mut written = 0;
        let mut report = Instant::now();
        while let Some(bytes) = response
            .chunk()
            .await
            .map_err(reqwest::Error::without_url)
            .context("APK 下载中断")?
        {
            written += bytes.len() as u64;
            ensure!(written <= release.apk.size, "APK 超过更新清单声明的大小");
            file.write_all(&bytes)
                .await
                .context("写入 APK 临时文件失败")?;
            hasher.update(bytes);
            if report.elapsed() >= Duration::from_millis(250) || written == release.apk.size {
                self.set_status(Phase::Downloading, written, None);
                report = Instant::now();
            }
        }
        ensure!(
            written == release.apk.size
                && format!("{:x}", hasher.finalize()).eq_ignore_ascii_case(&release.apk.sha256),
            "APK 大小或 SHA-256 校验失败"
        );
        file.flush().await.context("写入 APK 临时文件失败")?;
        Ok(())
    }

    async fn install(&self, release: &Release, work: &mut Work) -> Result<()> {
        let Some(guard) = UpdateInstallGuard::acquire(&self.runtime) else {
            self.phase(Phase::WaitingForTransfers);
            return Ok(());
        };
        self.phase(Phase::Installing);
        let result = self
            .platform
            .run_mobile_plugin_async::<InstallResult>(
                "install",
                serde_json::json!({
                    "version": release.version,
                    "versionCode": release.apk.version_code,
                }),
            )
            .await;
        drop(guard);
        let result = result.unwrap_or_else(|error| InstallResult::Failed {
            message: error.to_string(),
        });
        match result {
            InstallResult::Deferred => self.phase(Phase::Idle),
            InstallResult::Permission => self.phase(Phase::Permission),
            InstallResult::Installed => {
                self.update_preferences(|prefs| {
                    prefs.pending = None;
                    prefs.paused_version_code = None;
                })?;
                work.manual = false;
                work.metered_version_code = None;
                work.verified = None;
                self.remove_cache_file("update.apk").await;
                self.phase(Phase::NoUpdate);
            }
            result => {
                if matches!(result, InstallResult::InvalidApk { .. }) {
                    self.remove_cache_file("update.apk").await;
                    work.verified = None;
                }
                self.update_preferences(|prefs| {
                    prefs.paused_version_code = Some(release.apk.version_code)
                })?;
                work.manual = false;
                work.metered_version_code = None;
                match result {
                    InstallResult::InvalidApk { message } | InstallResult::Failed { message } => {
                        self.set_status(Phase::Error, 0, Some(message));
                    }
                    _ => self.phase(Phase::Paused),
                }
            }
        }
        Ok(())
    }

    async fn remove_cache_file(&self, name: &str) {
        if let Err(error) = fs::remove_file(self.cache.join(name)).await {
            if error.kind() != std::io::ErrorKind::NotFound {
                eprintln!("清理 Android 更新缓存 {name} 失败：{error}");
            }
        }
    }
}

async fn transfer_change(receiver: &mut broadcast::Receiver<WireMessage>) {
    loop {
        match receiver.recv().await {
            Ok(WireMessage::TransferProgress { .. }) | Err(_) => return,
            Ok(_) => {}
        }
    }
}

async fn verify_cache(path: &Path, release: &Release) -> Result<bool> {
    let mut file = match fs::File::open(path).await {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).context("打开 APK 缓存失败"),
    };
    if file.metadata().await?.len() != release.apk.size {
        return Ok(false);
    }
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    let mut total = 0;
    loop {
        let read = file.read(&mut buffer).await.context("读取 APK 缓存失败")?;
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > release.apk.size {
            return Ok(false);
        }
        hasher.update(&buffer[..read]);
    }
    Ok(total == release.apk.size
        && format!("{:x}", hasher.finalize()).eq_ignore_ascii_case(&release.apk.sha256))
}
