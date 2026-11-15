mod autostart;
mod clipboard;
mod clock;
mod discovery;
mod listeners;
mod metrics;
mod mobile_tls;
mod model;
mod runtime;
mod server;
mod updater;

use anyhow::Context;
use model::{
    ClipboardHistoryMode, ClipboardHistorySettings, ClipboardStatus, MonitorPosition, ReceiveMode,
    ReceiveSettings, TransferProgress, WireMessage,
};
#[cfg(target_os = "linux")]
use raw_window_handle::{HasDisplayHandle, RawDisplayHandle};
use runtime::{validate_receive_directory, NodeRuntime, UpdateFrequency};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tauri::menu::{Menu, MenuItem};
#[cfg(target_os = "macos")]
use tauri::RunEvent;
use tauri::{AppHandle, Manager, PhysicalPosition, State, WindowEvent};
use updater::{UpdateCoordinator, UpdateStatus};

#[derive(Clone)]
struct ManagedNode {
    runtime: Arc<NodeRuntime>,
}

struct TrayHandles {
    window_toggle: MenuItem<tauri::Wry>,
    monitor_toggle: MenuItem<tauri::Wry>,
    lock_toggle: MenuItem<tauri::Wry>,
    sync_toggle: MenuItem<tauri::Wry>,
}

#[derive(Clone)]
struct ManagedUpdater {
    coordinator: Arc<UpdateCoordinator>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
enum MonitorBackend {
    Wayland,
    X11,
    Other,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize)]
struct MonitorCapabilities {
    backend: MonitorBackend,
    can_persist_position: bool,
    can_click_through: bool,
    can_keep_above: bool,
}

fn monitor_capabilities_for_backend(backend: MonitorBackend) -> MonitorCapabilities {
    match backend {
        MonitorBackend::Wayland => MonitorCapabilities {
            backend,
            can_persist_position: false,
            can_click_through: false,
            can_keep_above: false,
        },
        MonitorBackend::X11 | MonitorBackend::Other => MonitorCapabilities {
            backend,
            can_persist_position: true,
            can_click_through: true,
            can_keep_above: true,
        },
        MonitorBackend::Unknown => MonitorCapabilities {
            backend,
            can_persist_position: false,
            can_click_through: false,
            can_keep_above: false,
        },
    }
}

fn monitor_backend<R: tauri::Runtime>(window: &tauri::WebviewWindow<R>) -> MonitorBackend {
    #[cfg(target_os = "linux")]
    {
        match window.display_handle().ok().map(|handle| handle.as_raw()) {
            Some(RawDisplayHandle::Wayland(_)) => MonitorBackend::Wayland,
            Some(RawDisplayHandle::Xlib(_)) | Some(RawDisplayHandle::Xcb(_)) => MonitorBackend::X11,
            Some(_) => MonitorBackend::Other,
            None => MonitorBackend::Unknown,
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = window;
        MonitorBackend::Other
    }
}

fn current_monitor_capabilities(app: &AppHandle) -> MonitorCapabilities {
    app.get_webview_window("monitor")
        .map(|window| monitor_capabilities_for_backend(monitor_backend(&window)))
        .unwrap_or_else(|| monitor_capabilities_for_backend(MonitorBackend::Unknown))
}

fn effective_monitor_locked(app: &AppHandle, runtime: &NodeRuntime) -> bool {
    current_monitor_capabilities(app).can_click_through && runtime.monitor_locked()
}

#[tauri::command]
fn node_base_url(state: State<'_, ManagedNode>) -> String {
    format!("http://127.0.0.1:{}", state.runtime.node_info().port)
}

#[tauri::command]
fn toggle_clipboard_sync(app: AppHandle, state: State<'_, ManagedNode>) -> bool {
    let enabled = !state
        .runtime
        .sync_enabled
        .load(std::sync::atomic::Ordering::SeqCst);
    state
        .runtime
        .sync_enabled
        .store(enabled, std::sync::atomic::Ordering::SeqCst);
    sync_tray_labels(&app);
    enabled
}

#[tauri::command]
fn clipboard_sync_enabled(state: State<'_, ManagedNode>) -> bool {
    state
        .runtime
        .sync_enabled
        .load(std::sync::atomic::Ordering::SeqCst)
}

#[tauri::command]
fn clipboard_status(state: State<'_, ManagedNode>) -> ClipboardStatus {
    state.runtime.clipboard_status()
}

#[tauri::command]
fn clipboard_history_settings(state: State<'_, ManagedNode>) -> ClipboardHistorySettings {
    state.runtime.clipboard_history_settings()
}

#[tauri::command]
fn set_clipboard_history_settings(
    state: State<'_, ManagedNode>,
    mode: String,
    value: u64,
) -> Result<ClipboardHistorySettings, String> {
    let mode =
        ClipboardHistoryMode::parse(&mode).ok_or_else(|| "剪贴板历史模式无效".to_string())?;
    state
        .runtime
        .set_clipboard_history_settings(mode, value)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn update_frequency(state: State<'_, ManagedUpdater>) -> String {
    state.coordinator.frequency().as_str().to_string()
}

#[tauri::command]
fn set_update_frequency(
    state: State<'_, ManagedUpdater>,
    frequency: String,
) -> Result<String, String> {
    let frequency =
        UpdateFrequency::parse(&frequency).ok_or_else(|| "自动更新频率无效".to_string())?;
    state.coordinator.set_frequency(frequency)
}

#[tauri::command]
fn check_for_updates(state: State<'_, ManagedUpdater>) -> Result<(), String> {
    state.coordinator.request_manual_check()
}

#[tauri::command]
fn update_status(state: State<'_, ManagedUpdater>) -> UpdateStatus {
    state.coordinator.status()
}

#[tauri::command]
fn autostart_enabled(state: State<'_, ManagedNode>) -> Result<bool, String> {
    if cfg!(debug_assertions) {
        return Err("开发构建不注册开机自启动".to_string());
    }
    let enabled = autostart::Registration::current()
        .and_then(|registration| registration.is_enabled())
        .map_err(|error| format!("无法读取开机自启动状态：{error}"))?;
    if state.runtime.autostart_enabled() && !enabled {
        return Err("开机自启动尚未注册，请检查系统权限或重新安装正式版本".to_string());
    }
    Ok(enabled)
}

#[tauri::command]
fn set_autostart_enabled(state: State<'_, ManagedNode>, enabled: bool) -> Result<bool, String> {
    if cfg!(debug_assertions) {
        return Err("开发构建不注册开机自启动".to_string());
    }
    let registration = autostart::Registration::current()
        .map_err(|error| format!("无法准备开机自启动设置：{error}"))?;
    let previous_registration = registration
        .is_enabled()
        .map_err(|error| format!("无法读取当前开机自启动状态：{error}"))?;
    let previous_preference = state.runtime.autostart_enabled();
    state
        .runtime
        .set_autostart_enabled(enabled)
        .map_err(|error| format!("保存开机自启动设置失败：{error}"))?;
    if let Err(error) = apply_autostart_registration(&registration, enabled) {
        let restore_registration =
            apply_autostart_registration(&registration, previous_registration);
        let restore_preference = state.runtime.set_autostart_enabled(previous_preference);
        return Err(format_registration_failure(
            error,
            restore_registration,
            restore_preference,
        ));
    }
    match registration.is_enabled() {
        Ok(actual) if actual == enabled => Ok(enabled),
        Ok(actual) => {
            let error =
                anyhow::anyhow!("系统报告的开机自启动状态为 {actual}，期望状态为 {enabled}");
            let restore_registration =
                apply_autostart_registration(&registration, previous_registration);
            let restore_preference = state.runtime.set_autostart_enabled(previous_preference);
            Err(format_registration_failure(
                error,
                restore_registration,
                restore_preference,
            ))
        }
        Err(error) => {
            let restore_registration =
                apply_autostart_registration(&registration, previous_registration);
            let restore_preference = state.runtime.set_autostart_enabled(previous_preference);
            Err(format_registration_failure(
                error,
                restore_registration,
                restore_preference,
            ))
        }
    }
}

#[tauri::command]
fn receive_settings(state: State<'_, ManagedNode>) -> ReceiveSettings {
    state.runtime.receive_settings()
}

#[tauri::command]
fn set_receive_settings(
    state: State<'_, ManagedNode>,
    mode: String,
    default_path: Option<String>,
) -> Result<ReceiveSettings, String> {
    let mode = ReceiveMode::parse(&mode).ok_or_else(|| "接收模式无效".to_string())?;
    let directory = default_path.map(PathBuf::from);
    state
        .runtime
        .set_receive_settings(mode, directory)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn accept_transfer(
    state: State<'_, ManagedNode>,
    transfer_id: String,
    destination: String,
) -> Result<TransferProgress, String> {
    let destination = validate_receive_directory(PathBuf::from(destination).as_path())
        .map_err(|error| error.to_string())?;
    let progress = state
        .runtime
        .set_transfer_acceptance(&transfer_id, destination)
        .ok_or_else(|| "传输不存在".to_string())?;
    state.runtime.publish_event(WireMessage::TransferProgress {
        progress: progress.clone(),
    });
    if let Some(source_node_id) = state.runtime.transfer_source_id(&transfer_id) {
        let _ = state.runtime.send_to_peer(
            &source_node_id,
            WireMessage::TransferAction {
                transfer_id,
                action: "accept".to_string(),
                reason: None,
            },
        );
    }
    Ok(progress)
}

#[tauri::command]
fn register_outgoing_transfer(
    state: State<'_, ManagedNode>,
    offer: model::TransferOffer,
) -> TransferProgress {
    state.runtime.add_outgoing_transfer(offer)
}

#[tauri::command]
fn update_transfer_progress(state: State<'_, ManagedNode>, progress: TransferProgress) {
    let transfer_id = progress.transfer_id.clone();
    state.runtime.update_transfer(&transfer_id, progress);
}

#[tauri::command]
fn monitor_capabilities(app: AppHandle) -> MonitorCapabilities {
    current_monitor_capabilities(&app)
}

#[tauri::command]
fn monitor_locked(app: AppHandle, state: State<'_, ManagedNode>) -> bool {
    effective_monitor_locked(&app, state.runtime.as_ref())
}

fn apply_monitor_locked(
    app: &AppHandle,
    runtime: &NodeRuntime,
    locked: bool,
) -> Result<bool, String> {
    let capabilities = current_monitor_capabilities(app);
    if locked && !capabilities.can_click_through {
        return Err("当前 Wayland 会话不支持悬浮条点击穿透，请从托盘隐藏悬浮条".to_string());
    }
    let previous = runtime.monitor_locked();
    if let Some(window) = app.get_webview_window("monitor") {
        window
            .set_ignore_cursor_events(locked)
            .map_err(|error| format!("无法更新监控条鼠标状态：{error}"))?;
    }
    if let Err(error) = runtime.set_monitor_locked(locked) {
        if let Some(window) = app.get_webview_window("monitor") {
            let _ = window.set_ignore_cursor_events(previous);
        }
        return Err(error.to_string());
    }
    sync_tray_labels(app);
    Ok(locked)
}

#[tauri::command]
fn set_monitor_locked(
    app: AppHandle,
    state: State<'_, ManagedNode>,
    locked: bool,
) -> Result<bool, String> {
    apply_monitor_locked(&app, state.runtime.as_ref(), locked)
}

#[tauri::command]
fn popup_monitor_menu(app: AppHandle) -> Result<(), String> {
    let Some(window) = app.get_webview_window("monitor") else {
        return Err("悬浮监控条尚未创建".to_string());
    };
    let lock = MenuItem::with_id(
        &app,
        "monitor-lock-now",
        "锁定悬浮条（忽略鼠标）",
        true,
        None::<&str>,
    )
    .map_err(|error| error.to_string())?;
    let hide = MenuItem::with_id(&app, "monitor-hide-now", "隐藏悬浮条", true, None::<&str>)
        .map_err(|error| error.to_string())?;
    let menu = Menu::with_items(&app, &[&lock, &hide]).map_err(|error| error.to_string())?;
    window
        .popup_menu(&menu)
        .map_err(|error| format!("无法打开悬浮条菜单：{error}"))
}

#[tauri::command]
fn set_monitor_position(
    app: AppHandle,
    state: State<'_, ManagedNode>,
    x: i32,
    y: i32,
) -> Result<(), String> {
    if !current_monitor_capabilities(&app).can_persist_position {
        return Err("当前 Wayland 会话不支持保存悬浮条全局坐标".to_string());
    }
    state
        .runtime
        .set_monitor_position(MonitorPosition { x, y })
        .map_err(|error| error.to_string())
}

fn window_toggle_label(visible: bool) -> &'static str {
    if visible {
        "隐藏窗口"
    } else {
        "显示窗口"
    }
}

fn monitor_toggle_label(visible: bool) -> &'static str {
    if visible {
        "隐藏悬浮监控条"
    } else {
        "显示悬浮监控条"
    }
}

fn monitor_lock_toggle_label(locked: bool) -> &'static str {
    if locked {
        "解锁悬浮条"
    } else {
        "锁定悬浮条"
    }
}

fn tray_monitor_locked(app: &AppHandle) -> bool {
    app.try_state::<ManagedNode>()
        .is_some_and(|state| effective_monitor_locked(app, state.runtime.as_ref()))
}

fn clipboard_sync_toggle_label(enabled: bool) -> &'static str {
    if enabled {
        "暂停剪贴板同步"
    } else {
        "恢复剪贴板同步"
    }
}

fn tray_sync_enabled(app: &AppHandle) -> bool {
    app.try_state::<ManagedNode>().is_some_and(|state| {
        state
            .runtime
            .sync_enabled
            .load(std::sync::atomic::Ordering::SeqCst)
    })
}

fn main_window_visible(app: &AppHandle) -> bool {
    app.get_webview_window("main")
        .and_then(|window| window.is_visible().ok())
        .unwrap_or(false)
}

fn monitor_window_visible(app: &AppHandle) -> bool {
    app.get_webview_window("monitor")
        .and_then(|window| window.is_visible().ok())
        .unwrap_or(false)
}

fn sync_tray_labels(app: &AppHandle) {
    let Some(tray) = app.try_state::<TrayHandles>() else {
        return;
    };
    let _ = tray
        .window_toggle
        .set_text(window_toggle_label(main_window_visible(app)));
    let _ = tray
        .monitor_toggle
        .set_text(monitor_toggle_label(monitor_window_visible(app)));
    let _ = tray
        .lock_toggle
        .set_text(monitor_lock_toggle_label(tray_monitor_locked(app)));
    let _ = tray
        .sync_toggle
        .set_text(clipboard_sync_toggle_label(tray_sync_enabled(app)));
}

fn set_main_window_visible(app: &AppHandle, visible: bool) {
    if let Some(window) = app.get_webview_window("main") {
        if visible {
            let _ = window.show();
            let _ = window.unminimize();
            let _ = window.set_focus();
        } else {
            let _ = window.hide();
        }
    }
    sync_tray_labels(app);
}

fn set_monitor_window_visible(app: &AppHandle, visible: bool, locked: bool) {
    let Some(window) = app.get_webview_window("monitor") else {
        return;
    };
    let locked = visible && locked && current_monitor_capabilities(app).can_click_through;
    let _ = window.set_ignore_cursor_events(locked);
    if visible {
        let _ = window.show();
    } else {
        let _ = window.hide();
    }
    sync_tray_labels(app);
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

fn sync_autostart_registration(runtime: &NodeRuntime) {
    if cfg!(debug_assertions) {
        return;
    }
    let registration = match autostart::Registration::current() {
        Ok(registration) => registration,
        Err(error) => {
            eprintln!("准备开机自启动失败：{error}");
            return;
        }
    };
    if let Err(error) = apply_autostart_registration(&registration, runtime.autostart_enabled()) {
        eprintln!("{error}");
    }
}

fn is_autostart_launch() -> bool {
    autostart::launched_from_args(std::env::args())
}

fn show_main_window(app: &AppHandle) {
    set_main_window_visible(app, true);
}

const MONITOR_MARGIN_PX: i32 = 16;

fn point_is_in_work_area(position: &MonitorPosition, monitor: &tauri::Monitor) -> bool {
    let area = monitor.work_area();
    let left = i64::from(area.position.x);
    let top = i64::from(area.position.y);
    let right = left + i64::from(area.size.width);
    let bottom = top + i64::from(area.size.height);
    let x = i64::from(position.x);
    let y = i64::from(position.y);
    x >= left && x < right && y >= top && y < bottom
}

fn clamp_monitor_position(
    position: MonitorPosition,
    monitor: &tauri::Monitor,
    window_width: i32,
    window_height: i32,
) -> MonitorPosition {
    let area = monitor.work_area();
    clamp_position_to_work_area(
        position,
        area.position.x,
        area.position.y,
        area.size.width,
        area.size.height,
        window_width,
        window_height,
    )
}

fn clamp_position_to_work_area(
    position: MonitorPosition,
    area_x: i32,
    area_y: i32,
    area_width: u32,
    area_height: u32,
    window_width: i32,
    window_height: i32,
) -> MonitorPosition {
    let left = i64::from(area_x) + i64::from(MONITOR_MARGIN_PX);
    let top = i64::from(area_y) + i64::from(MONITOR_MARGIN_PX);
    let right = i64::from(area_x) + i64::from(area_width)
        - i64::from(window_width)
        - i64::from(MONITOR_MARGIN_PX);
    let bottom = i64::from(area_y) + i64::from(area_height)
        - i64::from(window_height)
        - i64::from(MONITOR_MARGIN_PX);
    MonitorPosition {
        x: position.x.clamp(left as i32, right.max(left) as i32),
        y: position.y.clamp(top as i32, bottom.max(top) as i32),
    }
}

fn position_monitor_window<R: tauri::Runtime>(
    window: &tauri::WebviewWindow<R>,
    runtime: &NodeRuntime,
) {
    if !monitor_capabilities_for_backend(monitor_backend(window)).can_persist_position {
        return;
    }
    let Ok(outer_size) = window.outer_size() else {
        return;
    };
    let window_width = i32::try_from(outer_size.width).unwrap_or(i32::MAX);
    let window_height = i32::try_from(outer_size.height).unwrap_or(i32::MAX);
    let Ok(monitors) = window.available_monitors() else {
        return;
    };
    if monitors.is_empty() {
        return;
    }
    let saved = runtime.monitor_position();
    let monitor = saved
        .filter(|position| {
            monitors
                .iter()
                .any(|item| point_is_in_work_area(position, item))
        })
        .and_then(|position| {
            monitors
                .iter()
                .find(|item| point_is_in_work_area(&position, item))
                .map(|item| (position, item.clone()))
        });
    let (position, monitor) = if let Some((position, monitor)) = monitor {
        (position, monitor)
    } else {
        let monitor = window
            .primary_monitor()
            .ok()
            .flatten()
            .or_else(|| monitors.first().cloned())
            .unwrap_or_else(|| monitors[0].clone());
        let area = monitor.work_area();
        let position = MonitorPosition {
            x: (i64::from(area.position.x) + i64::from(area.size.width)
                - i64::from(window_width)
                - i64::from(MONITOR_MARGIN_PX)) as i32,
            y: (i64::from(area.position.y) + i64::from(area.size.height)
                - i64::from(window_height)
                - i64::from(MONITOR_MARGIN_PX)) as i32,
        };
        (position, monitor)
    };
    let position = clamp_monitor_position(position, &monitor, window_width, window_height);
    if window
        .set_position(PhysicalPosition::new(position.x, position.y))
        .is_ok()
    {
        if let Err(error) = runtime.set_monitor_position(position) {
            eprintln!("保存监控条位置失败：{error}");
        }
    }
}

async fn start_node(
    data_dir: PathBuf,
    static_root: PathBuf,
    version: String,
) -> anyhow::Result<Arc<NodeRuntime>> {
    let preferred_port = std::env::var("MESHSHARE_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(37880u16);
    let runtime = listeners::start(data_dir, static_root, version, preferred_port).await?;
    let cleanup_runtime = runtime.clone();
    tauri::async_runtime::spawn(async move {
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
    clipboard::start_monitor(runtime.clone());
    metrics::start(runtime.clone());
    discovery::start(runtime.clone());
    Ok(runtime)
}

fn static_root(app: &AppHandle) -> PathBuf {
    if let Ok(resource_dir) = app.path().resource_dir() {
        for candidate in [
            resource_dir.join("web"),
            resource_dir.join("web/dist"),
            resource_dir.join("dist"),
        ] {
            if candidate.join("index.html").exists() {
                return candidate;
            }
        }
    }
    let bundled = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join("web")))
        .filter(|path| path.exists());
    bundled.unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../frontend/dist"))
}

fn setup_tray_menu(app: &AppHandle) -> anyhow::Result<()> {
    let tray = app
        .tray_by_id("meshshare")
        .context("MeshShare tray icon is missing")?;
    let window_toggle = MenuItem::with_id(
        app,
        "toggle-window",
        window_toggle_label(main_window_visible(app)),
        true,
        None::<&str>,
    )?;
    let monitor_toggle = MenuItem::with_id(
        app,
        "toggle-monitor",
        monitor_toggle_label(monitor_window_visible(app)),
        true,
        None::<&str>,
    )?;
    let lock_toggle = MenuItem::with_id(
        app,
        "monitor-lock-toggle",
        monitor_lock_toggle_label(tray_monitor_locked(app)),
        true,
        None::<&str>,
    )?;
    let sync_toggle = MenuItem::with_id(
        app,
        "toggle-sync",
        clipboard_sync_toggle_label(tray_sync_enabled(app)),
        true,
        None::<&str>,
    )?;
    let exit = MenuItem::with_id(app, "exit", "退出程序", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &monitor_toggle,
            &lock_toggle,
            &window_toggle,
            &sync_toggle,
            &exit,
        ],
    )?;
    tray.set_menu(Some(menu))?;
    app.manage(TrayHandles {
        window_toggle,
        monitor_toggle,
        lock_toggle,
        sync_toggle,
    });
    sync_tray_labels(app);
    tray.on_menu_event(|app, event| match event.id().as_ref() {
        "toggle-window" => {
            set_main_window_visible(app, !main_window_visible(app));
        }
        "toggle-monitor" => {
            if let Some(state) = app.try_state::<ManagedNode>() {
                let visible = !monitor_window_visible(app);
                match state.runtime.set_monitor_visible(visible) {
                    Ok(()) => set_monitor_window_visible(
                        app,
                        visible,
                        effective_monitor_locked(app, state.runtime.as_ref()),
                    ),
                    Err(error) => eprintln!("保存悬浮监控条可见性失败：{error}"),
                }
            }
        }
        "monitor-lock-toggle" => {
            if let Some(state) = app.try_state::<ManagedNode>() {
                let locked = !effective_monitor_locked(app, state.runtime.as_ref());
                if let Err(error) = apply_monitor_locked(app, state.runtime.as_ref(), locked) {
                    eprintln!("{error}");
                }
            }
        }
        "toggle-sync" => {
            if let Some(state) = app.try_state::<ManagedNode>() {
                let enabled = !state
                    .runtime
                    .sync_enabled
                    .load(std::sync::atomic::Ordering::SeqCst);
                state
                    .runtime
                    .sync_enabled
                    .store(enabled, std::sync::atomic::Ordering::SeqCst);
            }
            sync_tray_labels(app);
        }
        "exit" => {
            if let Some(state) = app.try_state::<ManagedNode>() {
                state
                    .runtime
                    .stopping
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
            app.exit(0);
        }
        _ => {}
    });
    Ok(())
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main_window(app);
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            let autostart_launch = is_autostart_launch();
            let data_dir = app
                .path()
                .app_data_dir()
                .unwrap_or_else(|_| PathBuf::from(".meshshare"));
            let runtime = tauri::async_runtime::block_on(start_node(
                data_dir,
                static_root(app.handle()),
                app.package_info().version.to_string(),
            ))?;
            app.manage(ManagedNode {
                runtime: runtime.clone(),
            });
            let updater = UpdateCoordinator::new(app.handle().clone(), runtime.clone());
            app.manage(ManagedUpdater {
                coordinator: updater.clone(),
            });
            updater.start();
            sync_autostart_registration(runtime.as_ref());
            setup_tray_menu(app.handle())?;
            if let Some(window) = app.get_webview_window("monitor") {
                position_monitor_window(&window, runtime.as_ref());
            }
            set_monitor_window_visible(
                app.handle(),
                runtime.monitor_visible(),
                effective_monitor_locked(app.handle(), runtime.as_ref()),
            );
            if let Some(window) = app.get_webview_window("monitor") {
                let app_for_menu = app.handle().clone();
                window.on_menu_event(move |_window, event| match event.id().as_ref() {
                    "monitor-lock-now" => {
                        if let Some(state) = app_for_menu.try_state::<ManagedNode>() {
                            if let Err(error) =
                                apply_monitor_locked(&app_for_menu, state.runtime.as_ref(), true)
                            {
                                eprintln!("{error}");
                            }
                        }
                    }
                    "monitor-hide-now" => {
                        set_monitor_window_visible(&app_for_menu, false, false);
                    }
                    _ => {}
                });
            }
            if let Some(window) = app.get_webview_window("main") {
                if !autostart_launch {
                    show_main_window(app.handle());
                }
                let app_for_close = app.handle().clone();
                window.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        set_main_window_visible(&app_for_close, false);
                    }
                });
            }
            if let Some(window) = app.get_webview_window("monitor") {
                let app_for_close = app.handle().clone();
                window.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        set_monitor_window_visible(&app_for_close, false, false);
                    }
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            node_base_url,
            toggle_clipboard_sync,
            clipboard_sync_enabled,
            clipboard_status,
            clipboard_history_settings,
            set_clipboard_history_settings,
            update_frequency,
            set_update_frequency,
            check_for_updates,
            update_status,
            autostart_enabled,
            set_autostart_enabled,
            receive_settings,
            set_receive_settings,
            accept_transfer,
            register_outgoing_transfer,
            update_transfer_progress,
            monitor_capabilities,
            monitor_locked,
            set_monitor_locked,
            popup_monitor_menu,
            set_monitor_position
        ])
        .build(tauri::generate_context!())
        .expect("error while building MeshShare")
        .run(|app, event| {
            #[cfg(target_os = "macos")]
            if let RunEvent::Reopen { .. } = event {
                show_main_window(app);
            }
            #[cfg(not(target_os = "macos"))]
            let _ = (app, event);
        });
}

pub fn cleanup_autostart() {
    if let Err(error) =
        autostart::Registration::current().and_then(|registration| registration.disable())
    {
        eprintln!("清理开机自启动失败：{error}");
    }
}

#[cfg(test)]
mod tests {
    use super::{
        clamp_position_to_work_area, clipboard_sync_toggle_label, monitor_capabilities_for_backend,
        monitor_lock_toggle_label, monitor_toggle_label, window_toggle_label, MonitorBackend,
    };
    use crate::autostart::launched_from_args;
    use crate::model::MonitorPosition;

    #[test]
    fn monitor_position_is_clamped_inside_work_area() {
        assert_eq!(
            clamp_position_to_work_area(
                MonitorPosition { x: -500, y: -500 },
                0,
                0,
                1920,
                1080,
                440,
                42
            ),
            MonitorPosition { x: 16, y: 16 }
        );
        assert_eq!(
            clamp_position_to_work_area(
                MonitorPosition { x: 4000, y: 4000 },
                0,
                0,
                1920,
                1080,
                440,
                42
            ),
            MonitorPosition { x: 1464, y: 1022 }
        );
    }

    #[test]
    fn wayland_monitor_capabilities_do_not_claim_global_controls() {
        let capabilities = monitor_capabilities_for_backend(MonitorBackend::Wayland);
        assert!(!capabilities.can_persist_position);
        assert!(!capabilities.can_click_through);
        assert!(!capabilities.can_keep_above);
    }

    #[test]
    fn autostart_argument_is_detected_without_matching_similar_flags() {
        assert!(launched_from_args(["meshshare", "--meshshare-autostart"]));
        assert!(!launched_from_args(["meshshare", "--autostart"]));
    }
    #[test]
    fn tray_toggle_labels_follow_state() {
        assert_eq!(window_toggle_label(true), "隐藏窗口");
        assert_eq!(window_toggle_label(false), "显示窗口");
        assert_eq!(monitor_toggle_label(true), "隐藏悬浮监控条");
        assert_eq!(monitor_toggle_label(false), "显示悬浮监控条");
        assert_eq!(clipboard_sync_toggle_label(true), "暂停剪贴板同步");
        assert_eq!(clipboard_sync_toggle_label(false), "恢复剪贴板同步");
        assert_eq!(monitor_lock_toggle_label(true), "解锁悬浮条");
        assert_eq!(monitor_lock_toggle_label(false), "锁定悬浮条");
    }
}
