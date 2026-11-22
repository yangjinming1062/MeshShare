use crate::runtime::NodeRuntime;
use serde::{Deserialize, Serialize};

#[cfg(not(target_os = "android"))]
mod desktop;
#[cfg(not(target_os = "android"))]
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
