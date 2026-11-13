#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod clipboard;
mod clock;
mod discovery;
mod model;
mod runtime;
mod server;

use runtime::NodeRuntime;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
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

async fn start_node(data_dir: PathBuf, static_root: PathBuf) -> anyhow::Result<Arc<NodeRuntime>> {
    let preferred_port = std::env::var("MESHSHARE_PORT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(37880u16);
    let listener = match tokio::net::TcpListener::bind(("0.0.0.0", preferred_port)).await {
        Ok(listener) => listener,
        Err(_) => tokio::net::TcpListener::bind(("0.0.0.0", 0)).await?,
    };
    let actual_port = listener.local_addr()?.port();
    let runtime = NodeRuntime::new(data_dir, actual_port)?;
    let app = server::router(runtime.clone(), static_root);
    tauri::async_runtime::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    clipboard::start_monitor(runtime.clone());
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

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "显示窗口", true, None::<&str>)?;
    let hide = MenuItem::with_id(app, "hide", "隐藏窗口", true, None::<&str>)?;
    let toggle = MenuItem::with_id(
        app,
        "toggle-sync",
        "暂停/恢复剪贴板同步",
        true,
        None::<&str>,
    )?;
    let exit = MenuItem::with_id(app, "exit", "退出程序", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &hide, &toggle, &exit])?;
    TrayIconBuilder::with_id("meshshare")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| {
            let Some(window) = app.get_webview_window("main") else {
                return;
            };
            match event.id().as_ref() {
                "show" => {
                    let _ = window.show();
                    let _ = window.unminimize();
                    let _ = window.set_focus();
                }
                "hide" => {
                    let _ = window.hide();
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
            }
        })
        .build(app)?;
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
            let runtime =
                tauri::async_runtime::block_on(start_node(data_dir, static_root(app.handle())))?;
            app.manage(ManagedNode { runtime });
            build_tray(app.handle())?;
            if let Some(window) = app.get_webview_window("main") {
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
            clipboard_sync_enabled
        ])
        .run(tauri::generate_context!())
        .expect("error while running MeshShare");
}
