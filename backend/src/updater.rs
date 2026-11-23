use crate::runtime::NodeRuntime;
use serde::{Deserialize, Serialize};

#[cfg(all(feature = "gui", not(target_os = "android")))]
mod desktop;
#[cfg(all(feature = "gui", not(target_os = "android")))]
pub(crate) use desktop::UpdateCoordinator;

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
    NeedsAuthorization,
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

#[cfg(any(
    all(feature = "gui", not(target_os = "android")),
    all(feature = "headless", target_os = "linux")
))]
pub(super) fn is_due(
    frequency: crate::runtime::UpdateFrequency,
    last_checked_at_ms: Option<i64>,
    pending: bool,
    retry_at_ms: Option<i64>,
    now_ms: i64,
    manual: bool,
) -> bool {
    if manual || frequency == crate::runtime::UpdateFrequency::Disabled {
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

pub(crate) struct UpdateInstallGuard<'a> {
    runtime: &'a NodeRuntime,
}

impl<'a> UpdateInstallGuard<'a> {
    pub(crate) fn acquire(runtime: &'a NodeRuntime) -> Option<Self> {
        runtime.begin_update_install().then(|| Self { runtime })
    }
}

impl Drop for UpdateInstallGuard<'_> {
    fn drop(&mut self) {
        self.runtime.end_update_install();
    }
}
