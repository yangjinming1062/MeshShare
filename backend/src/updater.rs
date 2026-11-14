use crate::clock::now_millis;
use crate::runtime::{NodeRuntime, UpdateFrequency};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tauri_plugin_updater::UpdaterExt;
use tokio::sync::Notify;

const UPDATE_STATUS_EVENT: &str = "update-status";
const CHECK_RETRY_MS: i64 = 60 * 60 * 1000;
const ACTIVE_TRANSFER_RETRY_MS: i64 = 15 * 60 * 1000;
const CHECK_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UpdateState {
    Unavailable,
    Disabled,
    Idle,
    Checking,
    Available,
    Deferred,
    Downloading,
    Installing,
    UpToDate,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateStatus {
    pub state: UpdateState,
    pub current_version: String,
    pub available_version: Option<String>,
    pub downloaded_bytes: u64,
    pub content_length: Option<u64>,
    pub error: Option<String>,
}

pub struct UpdateCoordinator {
    app: AppHandle,
    runtime: Arc<NodeRuntime>,
    notify: Notify,
    manual_requested: AtomicBool,
    busy: AtomicBool,
    status: Mutex<UpdateStatus>,
}

impl UpdateCoordinator {
    pub fn new(app: AppHandle, runtime: Arc<NodeRuntime>) -> Arc<Self> {
        let current_version = app.package_info().version.to_string();
        Arc::new(Self {
            app,
            runtime,
            notify: Notify::new(),
            manual_requested: AtomicBool::new(false),
            busy: AtomicBool::new(false),
            status: Mutex::new(UpdateStatus {
                state: if cfg!(debug_assertions) {
                    UpdateState::Unavailable
                } else {
                    UpdateState::Idle
                },
                current_version,
                available_version: None,
                downloaded_bytes: 0,
                content_length: None,
                error: None,
            }),
        })
    }

    pub fn start(self: &Arc<Self>) {
        if cfg!(debug_assertions) {
            return;
        }
        let coordinator = self.clone();
        tauri::async_runtime::spawn(async move {
            coordinator.run().await;
        });
    }

    pub fn status(&self) -> UpdateStatus {
        self.status.lock().clone()
    }

    pub fn frequency(&self) -> UpdateFrequency {
        self.runtime.update_settings().0
    }

    pub fn set_frequency(&self, frequency: UpdateFrequency) -> Result<String, String> {
        self.runtime
            .set_update_frequency(frequency)
            .map_err(|error| format!("保存自动更新设置失败：{error}"))?;
        self.notify.notify_one();
        if frequency == UpdateFrequency::Disabled && !self.busy.load(Ordering::SeqCst) {
            self.set_status(UpdateState::Disabled, None, 0, None, None);
        }
        Ok(frequency.as_str().to_string())
    }

    pub fn request_manual_check(&self) -> Result<(), String> {
        if cfg!(debug_assertions) {
            return Err("开发构建不执行自动更新检查".to_string());
        }
        if self.busy.load(Ordering::SeqCst) {
            return Err("更新检查正在进行中".to_string());
        }
        self.manual_requested.store(true, Ordering::SeqCst);
        self.notify.notify_one();
        Ok(())
    }

    async fn run(self: Arc<Self>) {
        let mut retry_at_ms = None;
        loop {
            if self
                .runtime
                .stopping
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                break;
            }
            let (frequency, last_checked_at_ms, pending_version) = self.runtime.update_settings();
            let manual = self.manual_requested.load(Ordering::SeqCst);
            let now = now_millis();
            let pending = pending_version.is_some();
            let due = is_due(
                frequency,
                last_checked_at_ms,
                pending,
                retry_at_ms,
                now,
                manual,
            );

            if due {
                match self.check_and_maybe_install().await {
                    CheckResult::RetryLater => {
                        retry_at_ms = Some(now_millis().saturating_add(CHECK_RETRY_MS));
                    }
                    CheckResult::Deferred => {
                        retry_at_ms = Some(now_millis().saturating_add(ACTIVE_TRANSFER_RETRY_MS));
                    }
                    CheckResult::Completed => {
                        retry_at_ms = None;
                    }
                }
                continue;
            }

            let keep_error = self.status.lock().state == UpdateState::Error;
            if frequency == UpdateFrequency::Disabled && !keep_error {
                self.set_status(UpdateState::Disabled, None, 0, None, None);
            }
            self.wait_until_next_check(frequency, last_checked_at_ms, pending, retry_at_ms)
                .await;
        }
    }

    async fn wait_until_next_check(
        &self,
        frequency: UpdateFrequency,
        last_checked_at_ms: Option<i64>,
        pending: bool,
        retry_at_ms: Option<i64>,
    ) {
        let now = now_millis();
        let wait_ms = if let Some(retry_at_ms) = retry_at_ms {
            retry_at_ms.saturating_sub(now).max(1)
        } else if pending && frequency != UpdateFrequency::Disabled {
            ACTIVE_TRANSFER_RETRY_MS
        } else if let (Some(last_checked_at_ms), Some(period_ms)) =
            (last_checked_at_ms, frequency.period_ms())
        {
            period_ms
                .saturating_sub(now.saturating_sub(last_checked_at_ms))
                .max(1)
        } else {
            60 * 60 * 1000
        };
        tokio::select! {
            _ = self.notify.notified() => {}
            _ = tokio::time::sleep(Duration::from_millis(wait_ms.min(1_000) as u64)) => {}
        }
    }

    async fn check_and_maybe_install(&self) -> CheckResult {
        if self.busy.swap(true, Ordering::SeqCst) {
            return CheckResult::Completed;
        }
        let result = self.check_and_maybe_install_inner().await;
        self.busy.store(false, Ordering::SeqCst);
        result
    }

    async fn check_and_maybe_install_inner(&self) -> CheckResult {
        let manual = self.manual_requested.swap(false, Ordering::SeqCst);
        let frequency = self.frequency();
        if frequency == UpdateFrequency::Disabled && !manual {
            self.set_status(UpdateState::Disabled, None, 0, None, None);
            return CheckResult::Completed;
        }

        self.set_status(UpdateState::Checking, None, 0, None, None);
        let updater = match self.app.updater_builder().timeout(CHECK_TIMEOUT).build() {
            Ok(updater) => updater,
            Err(error) => {
                return self.check_error_result(error);
            }
        };
        let update = match updater.check().await {
            Ok(update) => update,
            Err(error) => {
                return self.check_error_result(error);
            }
        };
        let checked_at_ms = now_millis();

        let Some(checked_update) = update else {
            if let Err(error) = self
                .runtime
                .set_update_check_state(Some(checked_at_ms), None)
            {
                self.set_error(format!("保存更新检查状态失败：{error}"));
                return CheckResult::RetryLater;
            }
            self.set_status(UpdateState::UpToDate, None, 0, None, None);
            return CheckResult::Completed;
        };

        // Keep the short timeout for the manifest request, but let the package
        // download use the updater's normal streaming request without that
        // timeout. Large installers may legitimately take longer to download.
        let update = match self
            .app
            .updater_builder()
            .build()
            .and_then(|updater| updater.restore_update(checked_update.raw_json.clone()))
        {
            Ok(Some(update)) => update,
            Ok(None) => {
                if let Err(error) = self
                    .runtime
                    .set_update_check_state(Some(checked_at_ms), None)
                {
                    self.set_error(format!("保存更新检查状态失败：{error}"));
                    return CheckResult::RetryLater;
                }
                self.set_status(UpdateState::UpToDate, None, 0, None, None);
                return CheckResult::Completed;
            }
            Err(error) => {
                return self.check_error_result(error);
            }
        };

        // A frequency change can happen while the manifest request is in
        // flight. Re-read it before an automatic install so turning updates
        // off takes effect before any package bytes are downloaded.
        if self.frequency() == UpdateFrequency::Disabled && !manual {
            self.set_status(UpdateState::Disabled, None, 0, None, None);
            return CheckResult::Completed;
        }

        let available_version = update.version.clone();
        if let Err(error) = self
            .runtime
            .set_update_check_state(Some(checked_at_ms), Some(available_version.clone()))
        {
            self.set_error(format!("保存待更新状态失败：{error}"));
            return CheckResult::RetryLater;
        }
        self.set_status(
            UpdateState::Available,
            Some(available_version.clone()),
            0,
            None,
            None,
        );

        if self.runtime.has_active_transfers() {
            self.set_status(
                UpdateState::Deferred,
                Some(available_version),
                0,
                None,
                Some("等待当前文件传输完成".to_string()),
            );
            return CheckResult::Deferred;
        }

        if self.frequency() == UpdateFrequency::Disabled && !manual {
            self.set_status(UpdateState::Disabled, None, 0, None, None);
            return CheckResult::Completed;
        }

        let coordinator = self;
        self.set_status(
            UpdateState::Downloading,
            Some(update.version.clone()),
            0,
            None,
            None,
        );
        let bytes = match update
            .download(
                |chunk_length, content_length| {
                    let current = coordinator.status();
                    coordinator.set_status(
                        UpdateState::Downloading,
                        current.available_version,
                        current.downloaded_bytes.saturating_add(chunk_length as u64),
                        content_length.or(current.content_length),
                        None,
                    );
                },
                || {},
            )
            .await
        {
            Ok(bytes) => bytes,
            Err(error) => {
                return self.handle_install_error(error, checked_at_ms, &update.version);
            }
        };

        if self.frequency() == UpdateFrequency::Disabled && !manual {
            self.set_status(UpdateState::Disabled, None, 0, None, None);
            return CheckResult::Completed;
        }

        if self.runtime.has_active_transfers() || !self.runtime.begin_update_install() {
            self.set_status(
                UpdateState::Deferred,
                Some(update.version),
                0,
                None,
                Some("等待当前文件传输完成".to_string()),
            );
            return CheckResult::Deferred;
        }

        self.set_status(
            UpdateState::Installing,
            Some(update.version.clone()),
            0,
            None,
            None,
        );
        let install_guard = UpdateInstallGuard {
            runtime: &self.runtime,
        };
        let result = update.install(&bytes);
        match result {
            Ok(()) => {
                let _ = self
                    .runtime
                    .set_update_check_state(Some(checked_at_ms), None);
                self.set_status(
                    UpdateState::Installing,
                    Some(update.version),
                    0,
                    None,
                    Some("更新已安装，正在重启".to_string()),
                );
                self.app.restart();
            }
            Err(error) => {
                drop(install_guard);
                self.handle_install_error(error, checked_at_ms, &update.version)
            }
        }
    }

    fn set_error(&self, error: String) {
        self.set_status(UpdateState::Error, None, 0, None, Some(error));
    }

    fn check_error_result(&self, error: tauri_plugin_updater::Error) -> CheckResult {
        let retry = should_retry_check_error(&error);
        let error_message = error.to_string();
        if !retry {
            if let Err(state_error) = self
                .runtime
                .set_update_check_state(Some(now_millis()), None)
            {
                self.set_error(format!(
                    "{error_message}；保存失败检查状态失败：{state_error}"
                ));
                return CheckResult::RetryLater;
            }
        }
        self.set_error(error_message);
        if retry {
            CheckResult::RetryLater
        } else {
            CheckResult::Completed
        }
    }

    fn handle_install_error(
        &self,
        error: tauri_plugin_updater::Error,
        checked_at_ms: i64,
        version: &str,
    ) -> CheckResult {
        let retry = should_retry_install_error(&error);
        let error_message = error.to_string();
        if let Err(state_error) = self
            .runtime
            .set_update_check_state(Some(checked_at_ms), retry.then(|| version.to_string()))
        {
            self.set_error(format!(
                "{error_message}；保存失败重试状态失败：{state_error}"
            ));
        } else {
            self.set_error(error_message);
        }
        if retry {
            CheckResult::RetryLater
        } else {
            CheckResult::Completed
        }
    }

    fn set_status(
        &self,
        state: UpdateState,
        available_version: Option<String>,
        downloaded_bytes: u64,
        content_length: Option<u64>,
        error: Option<String>,
    ) {
        let status = {
            let mut status = self.status.lock();
            status.state = state;
            status.available_version = available_version;
            status.downloaded_bytes = downloaded_bytes;
            status.content_length = content_length;
            status.error = error;
            status.clone()
        };
        let _ = self.app.emit(UPDATE_STATUS_EVENT, status);
    }
}

fn should_retry_install_error(error: &tauri_plugin_updater::Error) -> bool {
    matches!(
        error,
        tauri_plugin_updater::Error::Reqwest(_) | tauri_plugin_updater::Error::Network(_)
    )
}

fn should_retry_check_error(error: &tauri_plugin_updater::Error) -> bool {
    matches!(
        error,
        tauri_plugin_updater::Error::Reqwest(_)
            | tauri_plugin_updater::Error::Io(_)
            | tauri_plugin_updater::Error::Network(_)
            | tauri_plugin_updater::Error::Tauri(_)
    )
}

enum CheckResult {
    Completed,
    RetryLater,
    Deferred,
}

struct UpdateInstallGuard<'a> {
    runtime: &'a NodeRuntime,
}

impl Drop for UpdateInstallGuard<'_> {
    fn drop(&mut self) {
        self.runtime.end_update_install();
    }
}

fn is_due(
    frequency: UpdateFrequency,
    last_checked_at_ms: Option<i64>,
    pending: bool,
    retry_at_ms: Option<i64>,
    now_ms: i64,
    manual: bool,
) -> bool {
    if manual || frequency == UpdateFrequency::Disabled {
        return manual;
    }
    if let Some(retry_at_ms) = retry_at_ms {
        return now_ms >= retry_at_ms;
    }
    pending
        || last_checked_at_ms.is_none_or(|at| {
            frequency
                .period_ms()
                .is_some_and(|period| now_ms.saturating_sub(at) >= period)
        })
}

#[cfg(test)]
mod tests {
    use super::{is_due, should_retry_check_error, should_retry_install_error};
    use crate::runtime::UpdateFrequency;

    #[test]
    fn update_schedule_uses_daily_and_weekly_boundaries() {
        let day = 24 * 60 * 60 * 1000;
        assert!(is_due(
            UpdateFrequency::Daily,
            Some(0),
            false,
            None,
            day,
            false
        ));
        assert!(!is_due(
            UpdateFrequency::Daily,
            Some(0),
            false,
            None,
            day - 1,
            false
        ));
        assert!(is_due(
            UpdateFrequency::Weekly,
            Some(0),
            false,
            None,
            7 * day,
            false
        ));
    }

    #[test]
    fn disabled_schedule_only_runs_when_requested_manually() {
        assert!(!is_due(
            UpdateFrequency::Disabled,
            None,
            false,
            None,
            0,
            false
        ));
        assert!(is_due(
            UpdateFrequency::Disabled,
            None,
            false,
            None,
            0,
            true
        ));
    }

    #[test]
    fn pending_updates_are_due_again_after_a_deferred_install() {
        assert!(is_due(
            UpdateFrequency::Weekly,
            Some(10),
            true,
            None,
            11,
            false
        ));
        assert!(is_due(
            UpdateFrequency::Weekly,
            Some(10),
            false,
            Some(20),
            20,
            false
        ));
    }

    #[test]
    fn retry_backoff_prevents_a_failed_check_from_looping() {
        assert!(!is_due(
            UpdateFrequency::Weekly,
            None,
            false,
            Some(100),
            99,
            false
        ));
        assert!(is_due(
            UpdateFrequency::Weekly,
            None,
            false,
            Some(100),
            100,
            false
        ));
    }

    #[test]
    fn permanent_update_errors_wait_for_manual_or_normal_schedule() {
        assert!(!should_retry_install_error(
            &tauri_plugin_updater::Error::AuthenticationFailed
        ));
        assert!(!should_retry_install_error(
            &tauri_plugin_updater::Error::ReleaseNotFound
        ));
        assert!(should_retry_install_error(
            &tauri_plugin_updater::Error::Network("temporary download failure".to_string())
        ));
    }

    #[test]
    fn only_transport_errors_retry_update_checks() {
        assert!(should_retry_check_error(
            &tauri_plugin_updater::Error::Network("temporary request failure".to_string())
        ));
        assert!(!should_retry_check_error(
            &tauri_plugin_updater::Error::ReleaseNotFound
        ));
    }
}
