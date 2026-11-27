pub(crate) mod monitor_window;
pub(crate) mod tray;

use crate::{
    apply_monitor_locked, apply_monitor_visible, autostart, links, monitor, node_data_dir,
    start_node, terminal, NodeRuntime,
};
#[cfg(target_os = "linux")]
use crate::{linux, MonitorCapabilities};
use std::sync::Arc;
use tauri::{AppHandle, Manager, RunEvent, State, WindowEvent};
use tauri_plugin_dialog::DialogExt;
use tray::{set_main_window_visible, MainWindowVisibility};

/// 把节点 CA 写到用户选择的位置，返回实际路径；用户取消时返回 `None`。
#[tauri::command]
async fn save_ca_certificate(
    app: AppHandle,
    window: tauri::WebviewWindow,
    state: State<'_, Arc<NodeRuntime>>,
    url: String,
) -> Result<Option<String>, String> {
    let bytes = state.fetch_mobile_ca(&url).await?;
    tauri::async_runtime::spawn_blocking(move || -> Result<Option<String>, String> {
        let mut dialog = app
            .dialog()
            .file()
            .set_title("保存节点 CA 证书")
            .set_file_name("meshshare-ca.crt")
            .add_filter("证书文件", &["crt"])
            .set_parent(&window);
        if let Ok(directory) = app.path().download_dir() {
            dialog = dialog.set_directory(directory);
        }
        let Some(target) = dialog.blocking_save_file() else {
            return Ok(None);
        };
        let path = target
            .into_path()
            .map_err(|error| format!("保存 CA 证书失败：{error}"))?;
        std::fs::write(&path, bytes).map_err(|error| format!("保存 CA 证书失败：{error}"))?;
        Ok(Some(path.display().to_string()))
    })
    .await
    .map_err(|_| "无法打开保存对话框".to_string())?
}

fn hide_monitor_persisted(app: &AppHandle) {
    if let Some(state) = app.try_state::<Arc<NodeRuntime>>() {
        if let Err(error) = apply_monitor_visible(app, state.as_ref(), false) {
            eprintln!("保存悬浮监控条可见性失败：{error}");
        }
    }
}

pub(crate) fn create_window(app: &AppHandle, label: &str) -> anyhow::Result<tauri::WebviewWindow> {
    use anyhow::Context;

    let config = app
        .config()
        .app
        .windows
        .iter()
        .find(|window| window.label == label)
        .with_context(|| format!("缺少窗口配置：{label}"))?;
    Ok(tauri::WebviewWindowBuilder::from_config(app, config)?.build()?)
}

pub fn run() {
    let context = tauri::generate_context!();
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            let handle = app.clone();
            if let Err(error) =
                app.run_on_main_thread(move || set_main_window_visible(&handle, true))
            {
                eprintln!("无法重新打开主窗口：{error}");
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .on_page_load(crate::terminal_page_load)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "monitor-lock-now" => {
                if let Some(state) = app.try_state::<Arc<NodeRuntime>>() {
                    if let Err(error) = apply_monitor_locked(app, state.as_ref(), true) {
                        eprintln!("{error}");
                    }
                }
            }
            "monitor-hide-now" => hide_monitor_persisted(app),
            _ => {}
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                match window.label() {
                    "main" => {
                        api.prevent_close();
                        set_main_window_visible(window.app_handle(), false);
                    }
                    "monitor" => {
                        api.prevent_close();
                        hide_monitor_persisted(window.app_handle());
                    }
                    _ => {}
                }
            }
        })
        .setup(|app| {
            let autostart_launch = autostart::launched_from_args(std::env::args());
            app.manage(monitor::Controller::default());
            app.manage(MainWindowVisibility::default());
            #[cfg(target_os = "linux")]
            app.manage(MonitorCapabilities::for_backend(
                linux::fullscreen::backend(),
            ));
            let Some(runtime) = tauri::async_runtime::block_on(start_node(
                app.handle(),
                node_data_dir(app.handle()),
            ))?
            else {
                return Ok(());
            };
            if !cfg!(debug_assertions) {
                if let Err(error) = autostart::set_enabled(runtime.autostart_enabled()) {
                    eprintln!("同步开机自启动失败：{error}");
                }
            }
            tray::setup_tray_menu(app.handle(), runtime.clone())?;
            crate::clipboard::start_monitor(runtime.clone());
            monitor::start(app.handle().clone(), runtime.clone())?;
            if !autostart_launch {
                set_main_window_visible(app.handle(), true);
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            crate::node_base_url,
            save_ca_certificate,
            crate::remote_file_access,
            crate::set_remote_file_access,
            monitor_window::monitor_view,
            monitor_window::popup_monitor_menu,
            monitor_window::set_monitor_position,
            links::open_project_page,
            links::open_transfer_location,
            terminal::commands::terminal_accept_migration,
            terminal::commands::terminal_open,
            terminal::commands::terminal_close,
            terminal::commands::terminal_resize,
            terminal::commands::terminal_write,
            terminal::commands::terminal_ack
        ])
        .build(context)
        .expect("error while building MeshShare")
        .run(|app, event| match event {
            // Wry 在最后一个窗口释放时请求退出，托盘和节点仍需常驻。
            RunEvent::ExitRequested {
                code: None, api, ..
            } => api.prevent_exit(),
            RunEvent::ExitRequested { .. } | RunEvent::Exit => {
                #[cfg(target_os = "windows")]
                monitor::stop_unlock(app);
                crate::stop_node(app, matches!(event, RunEvent::Exit));
            }
            #[cfg(target_os = "macos")]
            RunEvent::Reopen { .. } => set_main_window_visible(app, true),
            _ => {}
        });
}
