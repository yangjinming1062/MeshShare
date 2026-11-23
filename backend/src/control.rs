#[cfg(feature = "gui")]
use crate::autostart;
use crate::model::{
    ClipboardHistoryMode, ClipboardHistorySettings, ClipboardStatus, ReceiveSettings,
    TransferProgress, WireMessage,
};
use crate::runtime::{validate_receive_directory, NodeRuntime, UpdateFrequency};
#[cfg(all(feature = "gui", not(target_os = "android")))]
use crate::updater::UpdateCoordinator;
use crate::updater::UpdateStatus;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
#[cfg(any(
    all(feature = "gui", not(target_os = "android")),
    all(feature = "headless", target_os = "linux")
))]
use std::sync::OnceLock;

#[cfg(feature = "gui")]
use tauri::AppHandle;

#[derive(Debug, Clone, Serialize)]
pub struct MonitorSettingsSnapshot {
    pub visible: bool,
    pub locked: bool,
    pub capabilities: crate::MonitorCapabilities,
}

#[derive(Debug, Clone, Serialize)]
pub struct SettingsSnapshot {
    pub clipboard_sync_enabled: bool,
    pub autostart_enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_control_error: Option<String>,
    pub update_frequency: UpdateFrequency,
    pub update_status: Option<UpdateStatus>,
    pub receive_settings: ReceiveSettings,
    pub clipboard_history_settings: ClipboardHistorySettings,
    pub clipboard_status: ClipboardStatus,
    pub monitor: MonitorSettingsSnapshot,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClipboardHistoryPatch {
    pub mode: ClipboardHistoryMode,
    pub value: u64,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct MonitorSettingsPatch {
    pub visible: Option<bool>,
    pub locked: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct SettingsPatch {
    pub clipboard_sync_enabled: Option<bool>,
    pub autostart_enabled: Option<bool>,
    pub update_frequency: Option<String>,
    pub receive_settings: Option<ReceiveSettings>,
    pub clipboard_history_settings: Option<ClipboardHistoryPatch>,
    pub monitor: Option<MonitorSettingsPatch>,
}

/// Tauri 命令与远程 HTTP API 共用的控制面。
pub struct ControlPlane {
    runtime: Arc<NodeRuntime>,
    #[cfg(feature = "gui")]
    app: Option<AppHandle>,
    #[cfg(all(feature = "headless", target_os = "linux"))]
    headless: OnceLock<Arc<crate::headless_service::Coordinator>>,
    #[cfg(all(feature = "gui", not(target_os = "android")))]
    updater: OnceLock<Arc<UpdateCoordinator>>,
}

impl ControlPlane {
    pub fn runtime_only(runtime: Arc<NodeRuntime>) -> Arc<Self> {
        Arc::new(Self {
            runtime,
            #[cfg(feature = "gui")]
            app: None,
            #[cfg(all(feature = "headless", target_os = "linux"))]
            headless: OnceLock::new(),
            #[cfg(all(feature = "gui", not(target_os = "android")))]
            updater: OnceLock::new(),
        })
    }

    #[cfg(all(feature = "headless", target_os = "linux"))]
    pub fn start_headless(&self) {
        self.headless
            .get_or_init(|| crate::headless_service::Coordinator::start(self.runtime.clone()));
    }

    #[cfg(all(feature = "headless", target_os = "linux"))]
    pub fn headless(&self) -> Option<&Arc<crate::headless_service::Coordinator>> {
        self.headless.get()
    }

    #[cfg(feature = "gui")]
    pub fn with_app(runtime: Arc<NodeRuntime>, app: AppHandle) -> Arc<Self> {
        Arc::new(Self {
            runtime,
            app: Some(app),
            #[cfg(all(feature = "headless", target_os = "linux"))]
            headless: OnceLock::new(),
            #[cfg(all(feature = "gui", not(target_os = "android")))]
            updater: OnceLock::new(),
        })
    }

    #[cfg(all(feature = "gui", not(target_os = "android")))]
    pub fn install_updater(&self, updater: Arc<UpdateCoordinator>) -> Result<(), String> {
        self.updater
            .set(updater)
            .map_err(|_| "更新服务已经初始化".to_string())
    }

    fn monitor_capabilities(&self) -> crate::MonitorCapabilities {
        #[cfg(not(feature = "gui"))]
        return crate::MonitorCapabilities::unknown();
        #[cfg(feature = "gui")]
        self.app.as_ref().map_or_else(
            crate::MonitorCapabilities::unknown,
            crate::current_monitor_capabilities,
        )
    }

    pub async fn snapshot(&self) -> SettingsSnapshot {
        let monitor_capabilities = self.monitor_capabilities();
        #[cfg(not(feature = "gui"))]
        let locked = false;
        #[cfg(feature = "gui")]
        let locked = self.app.as_ref().map_or_else(
            || self.runtime.monitor_locked(),
            |app| crate::effective_monitor_locked(app, self.runtime.as_ref()),
        );
        let autostart = Ok::<_, String>(self.runtime.autostart_enabled());
        #[cfg(all(feature = "headless", target_os = "linux"))]
        let autostart = match self.headless() {
            Some(service) => service.autostart().await,
            None => autostart,
        };
        #[cfg(all(feature = "gui", not(target_os = "android")))]
        let update_status = self.updater.get().map(|updater| updater.status());
        #[cfg(not(all(feature = "gui", not(target_os = "android"))))]
        let update_status = None;
        #[cfg(all(feature = "headless", target_os = "linux"))]
        let update_status = self
            .headless()
            .map(|service| service.status())
            .or(update_status);
        SettingsSnapshot {
            clipboard_sync_enabled: self
                .runtime
                .sync_enabled
                .load(std::sync::atomic::Ordering::SeqCst),
            autostart_enabled: autostart.as_ref().copied().unwrap_or(false),
            service_control_error: autostart.err(),
            update_frequency: self.runtime.update_settings().0,
            update_status,
            receive_settings: self.runtime.receive_settings(),
            clipboard_history_settings: self.runtime.clipboard_history_settings(),
            clipboard_status: self.runtime.clipboard_status(),
            monitor: MonitorSettingsSnapshot {
                visible: !self.runtime.supports_capability("node.headless.v1")
                    && self.runtime.monitor_visible(),
                locked,
                capabilities: monitor_capabilities,
            },
        }
    }

    pub async fn apply_patch(&self, patch: SettingsPatch) -> Result<SettingsSnapshot, String> {
        #[cfg(all(feature = "headless", target_os = "linux"))]
        if self.headless().is_some() && patch.monitor.is_some() {
            return Err("无界面服务没有监控窗口".into());
        }
        if let Some(value) = patch.clipboard_sync_enabled {
            self.runtime
                .sync_enabled
                .store(value, std::sync::atomic::Ordering::SeqCst);
            #[cfg(feature = "gui")]
            if let Some(app) = self.app.as_ref() {
                crate::sync_tray_labels(app);
            }
        }

        if let Some(value) = patch.autostart_enabled {
            self.set_autostart(value).await?;
        }

        if let Some(value) = patch.update_frequency {
            let frequency =
                UpdateFrequency::parse(&value).ok_or_else(|| "自动更新频率无效".to_string())?;
            self.set_update_frequency(frequency)?;
        }

        if let Some(settings) = patch.receive_settings {
            let directory = settings.default_path.map(PathBuf::from);
            self.runtime
                .set_receive_settings(settings.mode, directory)
                .map_err(|error| error.to_string())?;
        }

        if let Some(settings) = patch.clipboard_history_settings {
            self.runtime
                .set_clipboard_history_settings(settings.mode, settings.value)
                .map_err(|error| error.to_string())?;
        }

        if let Some(monitor) = patch.monitor {
            if let Some(visible) = monitor.visible {
                self.set_monitor_visible(visible)?;
            }
            if let Some(locked) = monitor.locked {
                self.set_monitor_locked(locked)?;
            }
        }

        Ok(self.snapshot().await)
    }

    pub async fn request_update_check(&self) -> Result<SettingsSnapshot, String> {
        #[cfg(all(feature = "gui", not(target_os = "android")))]
        if let Some(updater) = self.updater.get() {
            updater.request_manual_check()?;
            return Ok(self.snapshot().await);
        }
        #[cfg(all(feature = "headless", target_os = "linux"))]
        if let Some(service) = self.headless() {
            service.request()?;
            return Ok(self.snapshot().await);
        }
        Err("更新服务尚未初始化".to_string())
    }

    fn set_update_frequency(&self, frequency: UpdateFrequency) -> Result<(), String> {
        #[cfg(all(feature = "gui", not(target_os = "android")))]
        if let Some(updater) = self.updater.get() {
            return updater.set_frequency(frequency);
        }
        #[cfg(all(feature = "headless", target_os = "linux"))]
        if let Some(service) = self.headless() {
            return service.set_frequency(frequency);
        }
        self.runtime
            .set_update_frequency(frequency)
            .map_err(|error| format!("保存自动更新设置失败：{error}"))
    }

    async fn set_autostart(&self, enabled: bool) -> Result<(), String> {
        #[cfg(all(feature = "headless", target_os = "linux"))]
        if let Some(service) = self.headless() {
            return service.set_autostart(enabled).await;
        }
        #[cfg(not(feature = "gui"))]
        return self
            .runtime
            .set_autostart_enabled(enabled)
            .map_err(|e| e.to_string());
        #[cfg(feature = "gui")]
        self.set_gui_autostart(enabled)
    }

    #[cfg(feature = "gui")]
    fn set_gui_autostart(&self, enabled: bool) -> Result<(), String> {
        let Some(_app) = self.app.as_ref() else {
            self.runtime
                .set_autostart_enabled(enabled)
                .map_err(|error| error.to_string())?;
            return Ok(());
        };
        if cfg!(debug_assertions) {
            return Err("开发构建不注册开机自启动".to_string());
        }
        let previous_registration = autostart::is_enabled()
            .map_err(|error| format!("无法读取当前开机自启动状态：{error}"))?;
        let previous_preference = self.runtime.autostart_enabled();
        self.runtime
            .set_autostart_enabled(enabled)
            .map_err(|error| format!("保存开机自启动设置失败：{error}"))?;
        // 任一失败都同时恢复系统注册与配置，避免界面与真实状态分叉。
        let failure = |error: anyhow::Error| {
            let mut message = format!("更新开机自启动失败：{error}");
            if let Err(restore) = autostart::set_enabled(previous_registration) {
                message.push_str(&format!("；恢复系统注册状态失败：{restore}"));
            }
            if let Err(restore) = self.runtime.set_autostart_enabled(previous_preference) {
                message.push_str(&format!("；恢复配置失败：{restore}"));
            }
            message
        };
        if let Err(error) = autostart::set_enabled(enabled) {
            return Err(failure(error));
        }
        match autostart::is_enabled() {
            Ok(actual) if actual == enabled => Ok(()),
            Ok(actual) => Err(failure(anyhow::anyhow!(
                "系统报告的开机自启动状态为 {actual}，期望状态为 {enabled}"
            ))),
            Err(error) => Err(failure(error)),
        }
    }

    fn set_monitor_visible(&self, visible: bool) -> Result<(), String> {
        #[cfg(feature = "gui")]
        if let Some(app) = self.app.as_ref() {
            return crate::apply_monitor_visible(app, self.runtime.as_ref(), visible);
        }
        self.runtime
            .set_monitor_visible(visible)
            .map_err(|e| e.to_string())
    }

    fn set_monitor_locked(&self, locked: bool) -> Result<(), String> {
        #[cfg(feature = "gui")]
        if let Some(app) = self.app.as_ref() {
            return crate::apply_monitor_locked(app, self.runtime.as_ref(), locked);
        }
        self.runtime
            .set_monitor_locked(locked)
            .map_err(|e| e.to_string())
    }
}

pub fn accept_transfer_at(
    runtime: &Arc<NodeRuntime>,
    transfer_id: String,
    destination: PathBuf,
) -> Result<TransferProgress, String> {
    if runtime
        .transfer_peer_ids(&transfer_id)
        .is_some_and(|(_, target_node_id)| target_node_id != runtime.node_id())
    {
        return Err("只有目标节点可以接受该传输".to_string());
    }
    let destination =
        validate_receive_directory(&destination).map_err(|error| error.to_string())?;
    let progress = runtime
        .set_transfer_acceptance(&transfer_id, destination)
        .ok_or_else(|| "传输不存在或状态已改变".to_string())?;
    runtime.publish_event(WireMessage::TransferProgress {
        progress: progress.clone(),
    });
    if let Some(source_node_id) = runtime.transfer_source_id(&transfer_id) {
        let message = WireMessage::TransferAction {
            transfer_id,
            action: crate::model::TransferAction::Accept,
            reason: None,
            retry_count: progress.retry_count,
        };
        if !runtime.send_to_peer(&source_node_id, message.clone()) {
            let runtime = runtime.clone();
            tokio::spawn(async move {
                let _ = runtime
                    .send_to_peer_with_retry(&source_node_id, message)
                    .await;
            });
        }
    }
    Ok(progress)
}
