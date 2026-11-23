#[cfg(all(feature = "gui", any(target_os = "android", test)))]
mod android;
#[cfg(feature = "gui")]
mod autostart;
mod browse;
mod clipboard;
mod clock;
mod control;
mod discovery;
mod fsutil;
#[cfg(all(feature = "headless", target_os = "linux"))]
mod headless;
#[cfg(all(feature = "headless", target_os = "linux"))]
mod headless_service;
#[cfg(all(feature = "gui", not(target_os = "android")))]
mod links;
#[cfg(all(feature = "gui", target_os = "linux"))]
mod linux;
#[cfg(all(feature = "headless", target_os = "linux"))]
pub use headless::run_headless;
mod listeners;
#[cfg(target_os = "macos")]
mod macos;
mod metrics;
mod mobile_tls;
mod model;
#[cfg(all(feature = "gui", not(target_os = "android")))]
mod monitor;
mod runtime;
mod server;
mod terminal;
mod updater;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(all(feature = "gui", not(target_os = "android")))]
mod desktop;
#[cfg(all(feature = "gui", not(target_os = "android")))]
pub use desktop::run;
#[cfg(all(feature = "gui", not(target_os = "android")))]
use desktop::tray::sync_tray_labels;

use runtime::NodeRuntime;
use serde::Serialize;
#[cfg(feature = "gui")]
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
#[cfg(feature = "gui")]
use tauri::{AppHandle, Manager, State};

const DEFAULT_NODE_PORT: u16 = 37880;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(not(all(feature = "gui", target_os = "linux")), allow(dead_code))]
enum MonitorBackend {
    Wayland,
    X11,
    /// Linux 之外的桌面（Windows、macOS）统一归入此值。
    #[cfg(not(target_os = "linux"))]
    Other,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) struct MonitorCapabilities {
    pub(crate) backend: MonitorBackend,
    pub(crate) can_persist_position: bool,
    pub(crate) can_click_through: bool,
    pub(crate) can_keep_above: bool,
}

impl MonitorCapabilities {
    const fn for_backend(backend: MonitorBackend) -> Self {
        let global = !matches!(backend, MonitorBackend::Unknown | MonitorBackend::Wayland);
        Self {
            backend,
            can_persist_position: global,
            can_click_through: global,
            can_keep_above: global,
        }
    }

    pub(crate) const fn unknown() -> Self {
        Self::for_backend(MonitorBackend::Unknown)
    }
}

#[cfg(all(feature = "gui", not(target_os = "android")))]
pub(crate) fn current_monitor_capabilities(app: &AppHandle) -> MonitorCapabilities {
    #[cfg(target_os = "linux")]
    {
        app.try_state::<MonitorCapabilities>()
            .map_or_else(MonitorCapabilities::unknown, |caps| *caps.inner())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = app;
        MonitorCapabilities::for_backend(MonitorBackend::Other)
    }
}

#[cfg(all(feature = "gui", not(target_os = "android")))]
pub(crate) fn effective_monitor_locked(app: &AppHandle, runtime: &NodeRuntime) -> bool {
    current_monitor_capabilities(app).can_click_through && runtime.monitor_locked()
}

#[cfg(all(feature = "gui", target_os = "android"))]
pub(crate) fn current_monitor_capabilities(_app: &AppHandle) -> MonitorCapabilities {
    MonitorCapabilities::unknown()
}

#[cfg(all(feature = "gui", target_os = "android"))]
pub(crate) fn effective_monitor_locked(_app: &AppHandle, runtime: &NodeRuntime) -> bool {
    runtime.monitor_locked()
}

#[cfg(feature = "gui")]
#[tauri::command]
fn node_base_url(state: State<'_, Arc<NodeRuntime>>) -> String {
    format!("http://127.0.0.1:{}", state.node_info().port)
}

/// 远程管理页只能在本机界面开关这项能力；远程设置接口不暴露它。
#[cfg(feature = "gui")]
#[tauri::command]
fn remote_file_access(state: State<'_, Arc<NodeRuntime>>) -> bool {
    state.remote_file_access()
}

#[cfg(feature = "gui")]
#[tauri::command]
fn set_remote_file_access(
    state: State<'_, Arc<NodeRuntime>>,
    enabled: bool,
) -> Result<bool, String> {
    state
        .set_remote_file_access(enabled)
        .map(|_| enabled)
        .map_err(|error| error.to_string())
}

#[cfg(all(feature = "gui", target_os = "android"))]
#[tauri::command]
fn android_network_changed(state: State<'_, Arc<NodeRuntime>>) {
    state.request_network_rebind();
}

#[cfg(all(feature = "gui", target_os = "android"))]
pub(crate) fn sync_tray_labels(_app: &AppHandle) {}

#[cfg(feature = "gui")]
pub(crate) fn apply_monitor_locked(
    app: &AppHandle,
    runtime: &NodeRuntime,
    locked: bool,
) -> Result<(), String> {
    if !cfg!(target_os = "android")
        && locked
        && !current_monitor_capabilities(app).can_click_through
    {
        return Err("当前桌面环境不支持悬浮条点击穿透，请从托盘隐藏悬浮条".to_string());
    }
    runtime
        .set_monitor_locked(locked)
        .map_err(|error| error.to_string())?;
    refresh_monitor(app)
}

#[cfg(feature = "gui")]
pub(crate) fn apply_monitor_visible(
    app: &AppHandle,
    runtime: &NodeRuntime,
    visible: bool,
) -> Result<(), String> {
    runtime
        .set_monitor_visible(visible)
        .map_err(|error| error.to_string())?;
    refresh_monitor(app)
}

#[cfg(all(feature = "gui", target_os = "android"))]
fn refresh_monitor(_app: &AppHandle) -> Result<(), String> {
    Ok(())
}

#[cfg(all(feature = "gui", not(target_os = "android")))]
use monitor::refresh as refresh_monitor;

#[cfg(feature = "gui")]
fn terminal_page_load(webview: &tauri::Webview, payload: &tauri::webview::PageLoadPayload<'_>) {
    if webview.label() == "main"
        && matches!(payload.event(), tauri::webview::PageLoadEvent::Started)
    {
        if let Some(runtime) = webview.try_state::<Arc<NodeRuntime>>() {
            runtime.terminals.close_all();
        }
    }
}

#[cfg(feature = "gui")]
fn stop_node(app: &AppHandle, wait: bool) {
    if let Some(runtime) = app.try_state::<Arc<NodeRuntime>>() {
        runtime
            .stopping
            .store(true, std::sync::atomic::Ordering::SeqCst);
        runtime.terminals.close_all();
        runtime.ssh.shutdown();
        if wait {
            runtime.wait_for_terminal_shutdown();
        }
    }
}

#[cfg(feature = "gui")]
async fn start_node(
    app: &AppHandle,
    data_dir: PathBuf,
) -> anyhow::Result<(Arc<NodeRuntime>, Arc<control::ControlPlane>)> {
    let preferred_port = std::env::var("MESHSHARE_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_NODE_PORT);
    let (runtime, control, _listener_task) = listeners::start(
        data_dir,
        static_root(app),
        app.package_info().version.to_string(),
        preferred_port,
        runtime::NodeProfile::current(),
        |runtime| control::ControlPlane::with_app(runtime, app.clone()),
    )
    .await?;
    #[cfg(not(target_os = "android"))]
    if runtime.ssh.host_fingerprint().is_none() {
        let owner = runtime.clone();
        tauri::async_runtime::spawn_blocking(move || {
            if let Err(error) = owner.restore_legacy_terminal_grant() {
                eprintln!("自动核验旧版终端授权失败：{error:#}");
            }
        });
    }
    start_workers(&runtime);
    clipboard::start_monitor(runtime.clone());
    app.manage(runtime.clone());
    Ok((runtime, control))
}

fn start_workers(runtime: &Arc<NodeRuntime>) {
    let cleanup_runtime = runtime.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(15));
        loop {
            interval.tick().await;
            if cleanup_runtime
                .stopping
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                break;
            }
            cleanup_runtime.mobile_sessions_snapshot();
            for directory in cleanup_runtime.prune_mobile_transfers() {
                if let Err(error) = tokio::fs::remove_dir_all(directory).await {
                    if error.kind() != std::io::ErrorKind::NotFound {
                        eprintln!("清理手机临时目录失败：{error}");
                    }
                }
            }
            cleanup_runtime.prune_transfers();
            cleanup_runtime.prune_clipboard_history();
        }
    });

    metrics::start(runtime.clone());
    discovery::start(runtime.clone());
}

#[cfg(feature = "gui")]
fn static_root(app: &AppHandle) -> PathBuf {
    app.path()
        .resource_dir()
        .ok()
        .map(|resource_dir| resource_dir.join("web"))
        .filter(|web| web.join("index.html").is_file())
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../frontend/dist"))
}

#[cfg(feature = "gui")]
fn node_data_dir(app: &AppHandle) -> PathBuf {
    app.path()
        .app_data_dir()
        .unwrap_or_else(|_| PathBuf::from(".meshshare"))
}

#[cfg(all(feature = "gui", target_os = "android"))]
#[tauri::mobile_entry_point]
pub fn run() {
    tauri::Builder::default()
        .on_page_load(terminal_page_load)
        .plugin(android::network::plugin())
        .plugin(android::storage::plugin())
        .plugin(tauri_plugin_dialog::init())
        .plugin(android::update::app::plugin())
        .setup(|app| {
            let data_dir = node_data_dir(app.handle());
            let (runtime, _) =
                tauri::async_runtime::block_on(start_node(app.handle(), data_dir.clone()))?;
            if let Err(error) = android::update::app::start(app.handle(), runtime, &data_dir) {
                eprintln!("启动 Android 更新服务失败：{error:#}");
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            node_base_url,
            android_network_changed,
            terminal::commands::terminal_open,
            terminal::commands::terminal_close,
            terminal::commands::terminal_resize,
            terminal::commands::terminal_write,
            terminal::commands::terminal_ack,
            remote_file_access,
            set_remote_file_access,
            android::commands::android_receive_storage,
            android::commands::android_request_receive_storage,
            android::commands::android_reveal_path,
            android::update::app::android_update_status,
            android::update::app::android_set_update_frequency,
            android::update::app::android_check_for_updates,
            android::update::app::android_continue_update
        ])
        .build(tauri::generate_context!())
        .expect("error while building MeshShare")
        .run(|app, event| {
            if matches!(
                event,
                tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit
            ) {
                stop_node(app, matches!(event, tauri::RunEvent::Exit));
            }
        });
}

#[cfg(feature = "gui")]
pub fn cleanup_autostart() {
    if let Err(error) = autostart::disable() {
        eprintln!("清理开机自启动失败：{error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wayland_monitor_capabilities_do_not_claim_global_controls() {
        let capabilities = MonitorCapabilities::for_backend(MonitorBackend::Wayland);
        assert!(!capabilities.can_persist_position);
        assert!(!capabilities.can_click_through);
        assert!(!capabilities.can_keep_above);
    }

    #[test]
    fn non_wayland_monitor_capabilities_allow_global_controls() {
        #[cfg(not(target_os = "linux"))]
        let supported = [MonitorBackend::X11, MonitorBackend::Other];
        #[cfg(target_os = "linux")]
        let supported = [MonitorBackend::X11];
        for backend in supported {
            let capabilities = MonitorCapabilities::for_backend(backend);
            assert!(capabilities.can_persist_position);
            assert!(capabilities.can_click_through);
            assert!(capabilities.can_keep_above);
        }
        assert!(!MonitorCapabilities::for_backend(MonitorBackend::Unknown).can_click_through);
    }
}
