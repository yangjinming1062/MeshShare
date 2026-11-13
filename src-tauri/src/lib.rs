#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod clipboard;
mod clock;
mod discovery;
mod metrics;
mod model;
mod runtime;
mod server;

use anyhow::Context;
use model::{HardwareMetrics, MonitorDisplayMode, WireMessage};
use runtime::NodeRuntime;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::menu::{Menu, MenuItem};
use tauri::{AppHandle, Manager, State, WindowEvent};

#[derive(Clone)]
struct ManagedNode {
    runtime: Arc<NodeRuntime>,
}

#[tauri::command]
fn node_base_url(state: State<'_, ManagedNode>) -> String {
    format!("http://127.0.0.1:{}", state.runtime.node_info().port)
}

#[tauri::command]
fn toggle_clipboard_sync(state: State<'_, ManagedNode>) -> bool {
    let enabled = !state
        .runtime
        .sync_enabled
        .load(std::sync::atomic::Ordering::SeqCst);
    state
        .runtime
        .sync_enabled
        .store(enabled, std::sync::atomic::Ordering::SeqCst);
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
fn monitor_display_mode(state: State<'_, ManagedNode>) -> String {
    state.runtime.monitor_display_mode().as_str().to_string()
}

#[tauri::command]
fn set_monitor_display_mode(
    app: AppHandle,
    state: State<'_, ManagedNode>,
    mode: String,
) -> Result<String, String> {
    let mode = parse_monitor_mode(&mode).ok_or_else(|| "监控显示模式无效".to_string())?;
    state
        .runtime
        .set_monitor_display_mode(mode.clone())
        .map_err(|error| error.to_string())?;
    apply_monitor_mode(&app, &mode);
    Ok(mode.as_str().to_string())
}

fn parse_monitor_mode(value: &str) -> Option<MonitorDisplayMode> {
    match value {
        "floating" => Some(MonitorDisplayMode::Floating),
        "tray" => Some(MonitorDisplayMode::Tray),
        "disabled" => Some(MonitorDisplayMode::Disabled),
        _ => None,
    }
}

fn apply_monitor_mode(app: &AppHandle, mode: &MonitorDisplayMode) {
    let Some(window) = app.get_webview_window("monitor") else {
        return;
    };
    match mode {
        MonitorDisplayMode::Floating => {
            let _ = window.show();
        }
        MonitorDisplayMode::Tray | MonitorDisplayMode::Disabled => {
            let _ = window.hide();
        }
    }
}

fn format_tray_metrics(metrics: &HardwareMetrics) -> String {
    let cpu = metrics
        .cpu_usage_percent
        .map(|value| format!("CPU {:.0}%", value))
        .unwrap_or_else(|| "CPU —".to_string());
    let memory = match (metrics.memory_used_bytes, metrics.memory_total_bytes) {
        (Some(used), Some(total)) if total > 0 => {
            format!("内存 {:.0}%", used as f64 / total as f64 * 100.0)
        }
        _ => "内存 —".to_string(),
    };
    let gpu = metrics
        .gpus
        .iter()
        .filter_map(|gpu| gpu.utilization_percent)
        .reduce(f32::max)
        .map(|value| format!("GPU {:.0}%", value))
        .unwrap_or_else(|| "GPU —".to_string());
    let (vram_used, vram_total) = metrics
        .gpus
        .iter()
        .fold((0u64, 0u64), |(used, total), gpu| {
            (
                used.saturating_add(gpu.memory_used_bytes.unwrap_or_default()),
                total.saturating_add(gpu.memory_total_bytes.unwrap_or_default()),
            )
        });
    let vram = if vram_total > 0 {
        format!(
            "显存 {} / {}",
            format_bytes(vram_used),
            format_bytes(vram_total)
        )
    } else {
        "显存 —".to_string()
    };
    format!(
        "{} · {} · {} · {} · ↓{} ↑{}",
        cpu,
        gpu,
        memory,
        vram,
        format_rate(metrics.network_down_bytes_per_sec),
        format_rate(metrics.network_up_bytes_per_sec)
    )
}

fn format_bytes(value: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    if value as f64 >= MIB {
        format!("{:.1} MB", value as f64 / MIB)
    } else if value as f64 >= KIB {
        format!("{:.1} KB", value as f64 / KIB)
    } else {
        format!("{} B", value)
    }
}

fn format_rate(value: Option<u64>) -> String {
    let Some(value) = value else {
        return "—".to_string();
    };
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    if value as f64 >= MIB {
        format!("{:.1} MB/s", value as f64 / MIB)
    } else if value as f64 >= KIB {
        format!("{:.1} KB/s", value as f64 / KIB)
    } else {
        format!("{} B/s", value)
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
    let listener = match tokio::net::TcpListener::bind(("0.0.0.0", preferred_port)).await {
        Ok(listener) => listener,
        Err(_) => tokio::net::TcpListener::bind(("0.0.0.0", 0)).await?,
    };
    let actual_port = listener.local_addr()?.port();
    let runtime = NodeRuntime::new(data_dir, actual_port, version)?;
    let app = server::router(runtime.clone(), static_root);
    tauri::async_runtime::spawn(async move {
        let _ = axum::serve(listener, app).await;
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
    bundled.unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../web/dist"))
}

fn setup_tray_menu(app: &AppHandle, runtime: Arc<NodeRuntime>) -> anyhow::Result<()> {
    let tray = app
        .tray_by_id("meshshare")
        .context("MeshShare tray icon is missing")?;
    let show = MenuItem::with_id(app, "show", "显示窗口", true, None::<&str>)?;
    let hide = MenuItem::with_id(app, "hide", "隐藏窗口", true, None::<&str>)?;
    let metrics_summary = MenuItem::with_id(
        app,
        "metrics-summary",
        "硬件监控：采集中…",
        false,
        None::<&str>,
    )?;
    let monitor_show =
        MenuItem::with_id(app, "monitor-show", "显示悬浮监控条", true, None::<&str>)?;
    let monitor_hide =
        MenuItem::with_id(app, "monitor-hide", "隐藏悬浮监控条", true, None::<&str>)?;
    let monitor_floating = MenuItem::with_id(
        app,
        "monitor-floating",
        "监控模式：悬浮条",
        true,
        None::<&str>,
    )?;
    let monitor_tray =
        MenuItem::with_id(app, "monitor-tray", "监控模式：仅托盘", true, None::<&str>)?;
    let monitor_disabled =
        MenuItem::with_id(app, "monitor-disabled", "关闭硬件监控", true, None::<&str>)?;
    let toggle = MenuItem::with_id(
        app,
        "toggle-sync",
        "暂停/恢复剪贴板同步",
        true,
        None::<&str>,
    )?;
    let exit = MenuItem::with_id(app, "exit", "退出程序", true, None::<&str>)?;
    let menu = Menu::with_items(
        app,
        &[
            &metrics_summary,
            &monitor_show,
            &monitor_hide,
            &monitor_floating,
            &monitor_tray,
            &monitor_disabled,
            &show,
            &hide,
            &toggle,
            &exit,
        ],
    )?;
    tray.set_menu(Some(menu))?;
    let mut events = runtime.subscribe();
    let metrics_summary_for_task = metrics_summary.clone();
    tauri::async_runtime::spawn(async move {
        while let Ok(message) = events.recv().await {
            if let WireMessage::MetricsUpdate { metrics } = message {
                let _ = metrics_summary_for_task.set_text(format_tray_metrics(&metrics));
            }
        }
    });
    tray.on_menu_event(|app, event| match event.id().as_ref() {
        "show" => {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }
        "hide" => {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.hide();
            }
        }
        "monitor-show" => {
            if let Some(window) = app.get_webview_window("monitor") {
                let _ = window.show();
            }
        }
        "monitor-hide" => {
            if let Some(window) = app.get_webview_window("monitor") {
                let _ = window.hide();
            }
        }
        "monitor-floating" | "monitor-tray" | "monitor-disabled" => {
            let mode = match event.id().as_ref() {
                "monitor-floating" => MonitorDisplayMode::Floating,
                "monitor-tray" => MonitorDisplayMode::Tray,
                _ => MonitorDisplayMode::Disabled,
            };
            if let Some(state) = app.try_state::<ManagedNode>() {
                let _ = state.runtime.set_monitor_display_mode(mode.clone());
                apply_monitor_mode(app, &mode);
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
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
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
            setup_tray_menu(app.handle(), runtime.clone())?;
            apply_monitor_mode(app.handle(), &runtime.monitor_display_mode());
            if let Some(window) = app.get_webview_window("main") {
                let window_for_close = window.clone();
                window.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = window_for_close.hide();
                    }
                });
            }
            if let Some(window) = app.get_webview_window("monitor") {
                let window_for_close = window.clone();
                window.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = window_for_close.hide();
                    }
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            node_base_url,
            toggle_clipboard_sync,
            clipboard_sync_enabled,
            monitor_display_mode,
            set_monitor_display_mode
        ])
        .run(tauri::generate_context!())
        .expect("error while running MeshShare");
}
