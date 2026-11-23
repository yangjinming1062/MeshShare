use super::{is_due, UpdateInstallGuard, UpdateState, UpdateStatus};
use crate::clock::now_millis;
use crate::runtime::{NodeRuntime, UpdateFrequency};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Emitter};
use tauri_plugin_updater::UpdaterExt;
use tokio::sync::Notify;

const UPDATE_STATUS_EVENT: &str = "update-status";
const CHECK_RETRY_MS: i64 = 60 * 60 * 1000;
const ACTIVE_TRANSFER_RETRY_MS: i64 = 15 * 60 * 1000;
#[cfg(any(target_os = "linux", test))]
const AUTHORIZATION_RETRY_MS: i64 = 24 * 60 * 60 * 1000;
const CHECK_TIMEOUT: Duration = Duration::from_secs(20);

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
        tauri::async_runtime::spawn(self.clone().run());
    }

    pub fn status(&self) -> UpdateStatus {
        self.status.lock().clone()
    }

    fn frequency(&self) -> UpdateFrequency {
        self.runtime.update_settings().0
    }

    pub fn set_frequency(&self, frequency: UpdateFrequency) -> Result<(), String> {
        self.runtime
            .set_update_frequency(frequency)
            .map_err(|error| format!("保存自动更新设置失败：{error}"))?;
        self.notify.notify_one();
        if frequency == UpdateFrequency::Disabled && !self.busy.load(Ordering::SeqCst) {
            self.set_status(UpdateState::Disabled, None, None);
        }
        Ok(())
    }

    pub fn request_manual_check(&self) -> Result<(), String> {
        if cfg!(debug_assertions) {
            return Err("开发构建不执行自动更新检查".to_string());
        }
        if self.busy.load(Ordering::SeqCst) {
            return Err("更新检查正在进行中".to_string());
        }
        self.manual_requested.store(true, Ordering::SeqCst);
        self.set_status(UpdateState::Checking, None, None);
        self.notify.notify_one();
        Ok(())
    }

    async fn run(self: Arc<Self>) {
        let mut retry_at_ms = None;
        while !self.runtime.stopping.load(Ordering::SeqCst) {
            let (frequency, last_checked_at_ms, pending_version) = self.runtime.update_settings();
            let manual = self.manual_requested.load(Ordering::SeqCst);
            if is_due(
                frequency,
                last_checked_at_ms,
                pending_version.is_some(),
                retry_at_ms,
                now_millis(),
                manual,
            ) {
                self.busy.store(true, Ordering::SeqCst);
                let result = self.check_and_maybe_install().await;
                self.busy.store(false, Ordering::SeqCst);
                let delay = match result {
                    CheckResult::RetryLater => Some(CHECK_RETRY_MS),
                    CheckResult::Deferred => Some(ACTIVE_TRANSFER_RETRY_MS),
                    #[cfg(target_os = "linux")]
                    CheckResult::NeedsAuthorization => Some(AUTHORIZATION_RETRY_MS),
                    CheckResult::Completed => None,
                };
                retry_at_ms = delay.map(|ms| now_millis().saturating_add(ms));
                continue;
            }

            let state = self.status.lock().state.clone();
            if frequency == UpdateFrequency::Disabled && should_apply_disabled_placeholder(&state) {
                self.set_status(UpdateState::Disabled, None, None);
            }
            tokio::select! {
                _ = self.notify.notified() => {}
                _ = tokio::time::sleep(Duration::from_secs(1)) => {}
            }
        }
    }

    async fn check_and_maybe_install(&self) -> CheckResult {
        let manual = self.manual_requested.swap(false, Ordering::SeqCst);
        if self.frequency() == UpdateFrequency::Disabled && !manual {
            self.set_status(UpdateState::Disabled, None, None);
            return CheckResult::Completed;
        }

        self.set_status(UpdateState::Checking, None, None);
        let builder = self.app.updater_builder().timeout(CHECK_TIMEOUT);
        let updater = match builder.build() {
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

        let Some(mut update) = update else {
            if let Err(error) = self
                .runtime
                .set_update_check_state(Some(checked_at_ms), None)
            {
                self.set_error(format!("保存更新检查状态失败：{error}"));
                return CheckResult::RetryLater;
            }
            self.set_status(UpdateState::UpToDate, None, None);
            return CheckResult::Completed;
        };

        // 清单使用短超时，大安装包下载保留普通流式请求的时限。
        update.timeout = None;
        // 检查过程中用户可能关闭了自动更新。
        if self.frequency() == UpdateFrequency::Disabled && !manual {
            self.set_status(UpdateState::Disabled, None, None);
            return CheckResult::Completed;
        }

        let version = &update.version;
        if let Err(error) = self
            .runtime
            .set_update_check_state(Some(checked_at_ms), Some(version.clone()))
        {
            self.set_error(format!("保存待更新状态失败：{error}"));
            return CheckResult::RetryLater;
        }
        self.set_status(UpdateState::Available, Some(version.clone()), None);

        if self.runtime.update_has_active_work() {
            return self.defer_install(version);
        }

        if self.frequency() == UpdateFrequency::Disabled && !manual {
            self.set_status(UpdateState::Disabled, None, None);
            return CheckResult::Completed;
        }

        self.set_status(UpdateState::Downloading, Some(version.clone()), None);
        let bytes = match update
            .download(
                |chunk_length, content_length| {
                    self.update_status(|status| {
                        status.downloaded_bytes =
                            status.downloaded_bytes.saturating_add(chunk_length as u64);
                        status.content_length = content_length.or(status.content_length);
                    });
                },
                || {},
            )
            .await
        {
            Ok(bytes) => bytes,
            Err(error) => {
                return self.handle_install_error(error, checked_at_ms, version);
            }
        };

        if self.frequency() == UpdateFrequency::Disabled && !manual {
            self.set_status(UpdateState::Disabled, None, None);
            return CheckResult::Completed;
        }

        let Some(install_guard) = UpdateInstallGuard::acquire(&self.runtime) else {
            return self.defer_install(version);
        };

        self.set_status(UpdateState::Installing, Some(version.clone()), None);
        #[cfg(target_os = "linux")]
        let installed = if let Some(package) = crate::linux::update::system_package() {
            self.set_status(
                UpdateState::Installing,
                Some(version.clone()),
                Some("请在系统授权对话框中确认安装更新".to_string()),
            );
            match crate::linux::update::install(package, bytes).await {
                crate::linux::update::Outcome::Installed => Ok(()),
                crate::linux::update::Outcome::NeedsAuthorization(reason) => {
                    drop(install_guard);
                    self.set_status(
                        UpdateState::NeedsAuthorization,
                        Some(version.clone()),
                        Some(reason.to_owned()),
                    );
                    return CheckResult::NeedsAuthorization;
                }
                crate::linux::update::Outcome::Failed(message) => {
                    drop(install_guard);
                    return self.record_install_failure(message, checked_at_ms);
                }
            }
        } else {
            update.install(&bytes)
        };
        #[cfg(not(target_os = "linux"))]
        let installed = update.install(&bytes);

        if let Err(error) = installed {
            drop(install_guard);
            return self.handle_install_error(error, checked_at_ms, version);
        }
        let _ = self
            .runtime
            .set_update_check_state(Some(checked_at_ms), None);
        self.set_status(
            UpdateState::Installing,
            Some(version.clone()),
            Some("更新已安装，正在重启".to_string()),
        );
        self.app.restart()
    }

    fn defer_install(&self, version: &str) -> CheckResult {
        self.set_status(
            UpdateState::Deferred,
            Some(version.to_owned()),
            Some("等待活动文件传输和终端会话结束".to_string()),
        );
        CheckResult::Deferred
    }

    fn set_error(&self, error: String) {
        self.set_status(UpdateState::Error, None, Some(error));
    }

    fn check_error_result(&self, error: tauri_plugin_updater::Error) -> CheckResult {
        let retry = should_retry_check_error(&error);
        let error_message = describe_update_error(&error);
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
        let error_message = describe_update_error(&error);
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

    #[cfg(target_os = "linux")]
    fn record_install_failure(&self, message: String, checked_at_ms: i64) -> CheckResult {
        if let Err(state_error) = self
            .runtime
            .set_update_check_state(Some(checked_at_ms), None)
        {
            self.set_error(format!("{message}；保存失败状态失败：{state_error}"));
        } else {
            self.set_error(message);
        }
        CheckResult::Completed
    }

    fn set_status(
        &self,
        state: UpdateState,
        available_version: Option<String>,
        error: Option<String>,
    ) {
        self.update_status(|status| {
            status.state = state;
            status.available_version = available_version;
            status.downloaded_bytes = 0;
            status.content_length = None;
            status.error = error;
        });
    }

    fn update_status(&self, change: impl FnOnce(&mut UpdateStatus)) {
        let status = {
            let mut status = self.status.lock();
            change(&mut status);
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

fn describe_update_error(error: &tauri_plugin_updater::Error) -> String {
    let mut message = error.to_string();
    if let Some(cause) = deepest_source(error) {
        if !cause.is_empty() && !message.contains(&cause) {
            message.push('：');
            message.push_str(&cause);
        }
    }
    if let Some(hint) = update_error_hint(error) {
        message.push('（');
        message.push_str(hint);
        message.push('）');
    }
    message
}

fn deepest_source(mut error: &(dyn std::error::Error + 'static)) -> Option<String> {
    let mut deepest = None;
    while let Some(cause) = error.source() {
        deepest = Some(cause.to_string());
        error = cause;
    }
    deepest
}

fn update_error_hint(error: &tauri_plugin_updater::Error) -> Option<&'static str> {
    match error {
        tauri_plugin_updater::Error::Reqwest(inner) if inner.is_timeout() => {
            Some("连接 GitHub 超时，请检查网络或代理设置")
        }
        tauri_plugin_updater::Error::Reqwest(_) => {
            Some("无法访问 GitHub，请检查网络、DNS 或代理设置")
        }
        tauri_plugin_updater::Error::ReleaseNotFound => {
            Some("未获取到稳定版更新清单，请确认能访问 GitHub 后重试")
        }
        tauri_plugin_updater::Error::Network(_) => Some("更新包下载失败，请检查网络或稍后重试"),
        _ => None,
    }
}

enum CheckResult {
    Completed,
    RetryLater,
    Deferred,
    #[cfg(target_os = "linux")]
    NeedsAuthorization,
}

// 关闭自动更新时只替换尚未检查过的占位状态，
// 保留手动检查得到的已是最新、待授权和失败结果。
fn should_apply_disabled_placeholder(state: &UpdateState) -> bool {
    matches!(state, UpdateState::Idle)
}

#[cfg(test)]
mod tests {
    use super::{
        deepest_source, describe_update_error, is_due, should_apply_disabled_placeholder,
        should_retry_check_error, should_retry_install_error, update_error_hint, UpdateState,
        AUTHORIZATION_RETRY_MS,
    };
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
        assert!(!is_due(
            UpdateFrequency::Weekly,
            Some(10),
            true,
            Some(AUTHORIZATION_RETRY_MS),
            AUTHORIZATION_RETRY_MS - 1,
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

    #[derive(Debug)]
    struct TestCause(&'static str);

    impl std::fmt::Display for TestCause {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str(self.0)
        }
    }

    impl std::error::Error for TestCause {}

    #[derive(Debug)]
    struct TestError {
        message: &'static str,
        cause: Option<TestCause>,
    }

    impl std::fmt::Display for TestError {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str(self.message)
        }
    }

    impl std::error::Error for TestError {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.cause
                .as_ref()
                .map(|cause| cause as &(dyn std::error::Error + 'static))
        }
    }

    #[test]
    fn disabled_frequency_only_replaces_the_idle_placeholder() {
        assert!(should_apply_disabled_placeholder(&UpdateState::Idle));
        for state in [
            UpdateState::Unavailable,
            UpdateState::Disabled,
            UpdateState::Checking,
            UpdateState::Available,
            UpdateState::Deferred,
            UpdateState::Downloading,
            UpdateState::Installing,
            UpdateState::UpToDate,
            UpdateState::NeedsAuthorization,
            UpdateState::Error,
        ] {
            assert!(!should_apply_disabled_placeholder(&state), "{state:?}");
        }
    }

    #[test]
    fn deepest_source_returns_the_innermost_cause() {
        let chained = TestError {
            message: "top",
            cause: Some(TestCause("root cause")),
        };
        assert_eq!(deepest_source(&chained), Some("root cause".to_string()));
        let plain = TestError {
            message: "top",
            cause: None,
        };
        assert_eq!(deepest_source(&plain), None);
    }

    #[test]
    fn update_errors_keep_the_actionable_hint() {
        let message = describe_update_error(&tauri_plugin_updater::Error::Network(
            "Download request failed with status: 404".to_string(),
        ));
        assert!(message.contains("Download request failed with status: 404"));
        assert!(message.ends_with("（更新包下载失败，请检查网络或稍后重试）"));
        let message = describe_update_error(&tauri_plugin_updater::Error::ReleaseNotFound);
        assert!(message.ends_with("（未获取到稳定版更新清单，请确认能访问 GitHub 后重试）"));
        assert_eq!(
            update_error_hint(&tauri_plugin_updater::Error::UnsupportedOs),
            None
        );
    }
}
