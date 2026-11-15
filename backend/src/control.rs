use crate::autostart;
use crate::model::{
    ClipboardHistoryMode, ClipboardHistorySettings, ClipboardStatus, ReceiveSettings,
    TransferProgress, WireMessage,
};
use crate::runtime::{validate_receive_directory, NodeRuntime, UpdateFrequency};
use crate::updater::{UpdateCoordinator, UpdateStatus};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use tauri::AppHandle;

#[derive(Debug, Clone, Serialize)]
pub struct MonitorCapabilitiesSnapshot {
    pub backend: String,
    pub can_persist_position: bool,
    pub can_click_through: bool,
    pub can_keep_above: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct MonitorSettingsSnapshot {
    pub visible: bool,
    pub locked: bool,
    pub capabilities: MonitorCapabilitiesSnapshot,
}

#[derive(Debug, Clone, Serialize)]
pub struct SettingsSnapshot {
    pub clipboard_sync_enabled: bool,
    pub autostart_enabled: bool,
    pub update_frequency: String,
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

/// Shared control plane used by both Tauri commands and the remote HTTP API.
pub struct ControlPlane {
    runtime: Arc<NodeRuntime>,
    app: Option<AppHandle>,
    updater: OnceLock<Arc<UpdateCoordinator>>,
}

impl ControlPlane {
    pub fn runtime_only(runtime: Arc<NodeRuntime>) -> Arc<Self> {
        Arc::new(Self {
            runtime,
            app: None,
            updater: OnceLock::new(),
        })
    }

    pub fn with_app(runtime: Arc<NodeRuntime>, app: AppHandle) -> Arc<Self> {
        Arc::new(Self {
            runtime,
            app: Some(app),
            updater: OnceLock::new(),
        })
    }

    pub fn install_updater(&self, updater: Arc<UpdateCoordinator>) -> Result<(), String> {
        self.updater
            .set(updater)
            .map_err(|_| "更新服务已经初始化".to_string())
    }

    fn updater(&self) -> Option<&Arc<UpdateCoordinator>> {
        self.updater.get()
    }

    pub fn toggle_clipboard_sync(&self) -> bool {
        let enabled = !self
            .runtime
            .sync_enabled
            .load(std::sync::atomic::Ordering::SeqCst);
        self.runtime
            .sync_enabled
            .store(enabled, std::sync::atomic::Ordering::SeqCst);
        if let Some(app) = self.app.as_ref() {
            crate::sync_tray_labels(app);
        }
        enabled
    }

    pub fn set_clipboard_history_settings(
        &self,
        mode: ClipboardHistoryMode,
        value: u64,
    ) -> Result<ClipboardHistorySettings, String> {
        self.runtime
            .set_clipboard_history_settings(mode, value)
            .map_err(|error| error.to_string())
    }

    pub fn set_update_frequency(&self, frequency: UpdateFrequency) -> Result<String, String> {
        if let Some(updater) = self.updater() {
            updater.set_frequency(frequency)
        } else {
            self.runtime
                .set_update_frequency(frequency)
                .map_err(|error| format!("保存自动更新设置失败：{error}"))?;
            Ok(frequency.as_str().to_string())
        }
    }

    pub fn set_receive_settings(
        &self,
        mode: crate::model::ReceiveMode,
        directory: Option<PathBuf>,
    ) -> Result<ReceiveSettings, String> {
        self.runtime
            .set_receive_settings(mode, directory)
            .map_err(|error| error.to_string())
    }

    pub fn set_autostart_enabled(&self, enabled: bool) -> Result<bool, String> {
        self.set_autostart(enabled)?;
        Ok(enabled)
    }

    pub fn update_frequency(&self) -> String {
        self.snapshot().update_frequency
    }

    pub fn update_status(&self) -> Result<UpdateStatus, String> {
        self.updater()
            .map(|updater| updater.status())
            .ok_or_else(|| "更新服务尚未初始化".to_string())
    }

    fn monitor_capabilities(&self) -> MonitorCapabilitiesSnapshot {
        let Some(app) = self.app.as_ref() else {
            return MonitorCapabilitiesSnapshot {
                backend: "unknown".to_string(),
                can_persist_position: false,
                can_click_through: false,
                can_keep_above: false,
            };
        };
        let capabilities = crate::current_monitor_capabilities(app);
        MonitorCapabilitiesSnapshot {
            backend: format!("{:?}", capabilities.backend).to_lowercase(),
            can_persist_position: capabilities.can_persist_position,
            can_click_through: capabilities.can_click_through,
            can_keep_above: capabilities.can_keep_above,
        }
    }

    pub fn snapshot(&self) -> SettingsSnapshot {
        let (frequency, _, _) = self.runtime.update_settings();
        let monitor_capabilities = self.monitor_capabilities();
        let locked = self.app.as_ref().map_or_else(
            || self.runtime.monitor_locked(),
            |app| crate::effective_monitor_locked(app, self.runtime.as_ref()),
        );
        SettingsSnapshot {
            clipboard_sync_enabled: self
                .runtime
                .sync_enabled
                .load(std::sync::atomic::Ordering::SeqCst),
            autostart_enabled: self.runtime.autostart_enabled(),
            update_frequency: self
                .updater()
                .map(|updater| updater.frequency().as_str().to_string())
                .unwrap_or_else(|| frequency.as_str().to_string()),
            update_status: self.updater().map(|updater| updater.status()),
            receive_settings: self.runtime.receive_settings(),
            clipboard_history_settings: self.runtime.clipboard_history_settings(),
            clipboard_status: self.runtime.clipboard_status(),
            monitor: MonitorSettingsSnapshot {
                visible: self.runtime.monitor_visible(),
                locked,
                capabilities: monitor_capabilities,
            },
        }
    }

    pub fn apply_patch(&self, patch: SettingsPatch) -> Result<SettingsSnapshot, String> {
        if let Some(value) = patch.clipboard_sync_enabled {
            self.runtime
                .sync_enabled
                .store(value, std::sync::atomic::Ordering::SeqCst);
            if let Some(app) = self.app.as_ref() {
                crate::sync_tray_labels(app);
            }
        }

        if let Some(value) = patch.autostart_enabled {
            self.set_autostart(value)?;
        }

        if let Some(value) = patch.update_frequency {
            let frequency =
                UpdateFrequency::parse(&value).ok_or_else(|| "自动更新频率无效".to_string())?;
            self.set_update_frequency(frequency)?;
        }

        if let Some(settings) = patch.receive_settings {
            let directory = settings.default_path.map(PathBuf::from);
            self.set_receive_settings(settings.mode, directory)?;
        }

        if let Some(settings) = patch.clipboard_history_settings {
            self.set_clipboard_history_settings(settings.mode, settings.value)?;
        }

        if let Some(monitor) = patch.monitor {
            if let Some(visible) = monitor.visible {
                self.set_monitor_visible(visible)?;
            }
            if let Some(locked) = monitor.locked {
                self.set_monitor_locked(locked)?;
            }
        }

        Ok(self.snapshot())
    }

    pub fn request_update_check(&self) -> Result<SettingsSnapshot, String> {
        let Some(updater) = self.updater() else {
            return Err("更新服务尚未初始化".to_string());
        };
        updater.request_manual_check()?;
        Ok(self.snapshot())
    }

    fn set_autostart(&self, enabled: bool) -> Result<(), String> {
        let Some(_app) = self.app.as_ref() else {
            self.runtime
                .set_autostart_enabled(enabled)
                .map_err(|error| error.to_string())?;
            return Ok(());
        };
        if cfg!(debug_assertions) {
            return Err("开发构建不注册开机自启动".to_string());
        }
        let registration = autostart::Registration::current()
            .map_err(|error| format!("无法准备开机自启动设置：{error}"))?;
        let previous_registration = registration
            .is_enabled()
            .map_err(|error| format!("无法读取当前开机自启动状态：{error}"))?;
        let previous_preference = self.runtime.autostart_enabled();
        self.runtime
            .set_autostart_enabled(enabled)
            .map_err(|error| format!("保存开机自启动设置失败：{error}"))?;
        if let Err(error) = apply_autostart_registration(&registration, enabled) {
            let restore_registration =
                apply_autostart_registration(&registration, previous_registration);
            let restore_preference = self.runtime.set_autostart_enabled(previous_preference);
            return Err(format_registration_failure(
                error,
                restore_registration,
                restore_preference,
            ));
        }
        match registration.is_enabled() {
            Ok(actual) if actual == enabled => Ok(()),
            Ok(actual) => {
                let error =
                    anyhow::anyhow!("系统报告的开机自启动状态为 {actual}，期望状态为 {enabled}");
                let restore_registration =
                    apply_autostart_registration(&registration, previous_registration);
                let restore_preference = self.runtime.set_autostart_enabled(previous_preference);
                Err(format_registration_failure(
                    error,
                    restore_registration,
                    restore_preference,
                ))
            }
            Err(error) => {
                let restore_registration =
                    apply_autostart_registration(&registration, previous_registration);
                let restore_preference = self.runtime.set_autostart_enabled(previous_preference);
                Err(format_registration_failure(
                    error,
                    restore_registration,
                    restore_preference,
                ))
            }
        }
    }

    fn set_monitor_visible(&self, visible: bool) -> Result<(), String> {
        match self.app.as_ref() {
            Some(app) => crate::apply_monitor_visible(app, self.runtime.as_ref(), visible),
            None => self
                .runtime
                .set_monitor_visible(visible)
                .map_err(|error| error.to_string()),
        }
    }

    fn set_monitor_locked(&self, locked: bool) -> Result<(), String> {
        if let Some(app) = self.app.as_ref() {
            crate::apply_monitor_locked(app, self.runtime.as_ref(), locked)?;
        } else {
            self.runtime
                .set_monitor_locked(locked)
                .map_err(|error| error.to_string())?;
        }
        Ok(())
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
            action: "accept".to_string(),
            reason: None,
            retry_count: None,
        };
        if !runtime.send_to_peer(&source_node_id, message.clone()) {
            let runtime = runtime.clone();
            tauri::async_runtime::spawn(async move {
                let _ = runtime
                    .send_to_peer_with_retry(&source_node_id, message)
                    .await;
            });
        }
    }
    Ok(progress)
}

fn apply_autostart_registration(
    registration: &autostart::Registration,
    enabled: bool,
) -> anyhow::Result<()> {
    if enabled {
        registration
            .enable()
            .map_err(|error| anyhow::anyhow!("启用开机自启动失败：{error}"))
    } else {
        registration
            .disable()
            .map_err(|error| anyhow::anyhow!("关闭开机自启动失败：{error}"))
    }
}

fn format_registration_failure(
    error: anyhow::Error,
    restore_registration: anyhow::Result<()>,
    restore_preference: anyhow::Result<()>,
) -> String {
    let mut message = format!("更新开机自启动失败：{error}");
    if let Err(restore_error) = restore_registration {
        message.push_str(&format!("；恢复系统注册状态失败：{restore_error}"));
    }
    if let Err(restore_error) = restore_preference {
        message.push_str(&format!("；恢复配置失败：{restore_error}"));
    }
    message
}
