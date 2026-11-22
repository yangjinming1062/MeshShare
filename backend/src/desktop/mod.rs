pub(crate) mod monitor_window;
pub(crate) mod tray;

use crate::{
    apply_monitor_locked, apply_monitor_visible, autostart, links, monitor, node_data_dir,
    start_node, terminal, updater::UpdateCoordinator, NodeRuntime,
};
#[cfg(target_os = "linux")]
use crate::{linux, MonitorBackend, MonitorCapabilities};
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

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            set_main_window_visible(app, true);
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .on_page_load(|webview, payload| {
            if webview.label() == "main"
                && matches!(payload.event(), tauri::webview::PageLoadEvent::Started)
            {
                if let Some(state) = webview.try_state::<Arc<NodeRuntime>>() {
                    state.terminals.close_all();
                }
            }
        })
        .setup(|app| {
            let autostart_launch = autostart::launched_from_args(std::env::args());
            app.manage(monitor::Controller::default());
            app.manage(MainWindowVisibility::new(
                app.get_webview_window("main")
                    .and_then(|window| window.is_visible().ok())
                    .unwrap_or(false),
            ));
            let monitor_window = app.get_webview_window("monitor");
            #[cfg(target_os = "linux")]
            {
                let backend = monitor_window
                    .as_ref()
                    .map_or(MonitorBackend::Unknown, |window| {
                        linux::fullscreen::prepare(window).unwrap_or_else(|error| {
                            eprintln!("准备悬浮条原生窗口失败：{error:#}");
                            MonitorBackend::Unknown
                        })
                    });
                app.manage(MonitorCapabilities::for_backend(backend));
            }
            let (runtime, control) = tauri::async_runtime::block_on(start_node(
                app.handle(),
                node_data_dir(app.handle()),
            ))?;
            let updater = UpdateCoordinator::new(app.handle().clone(), runtime.clone());
            control.install_updater(updater.clone())?;
            updater.start();
            if !cfg!(debug_assertions) {
                if let Err(error) = autostart::set_enabled(runtime.autostart_enabled()) {
                    eprintln!("同步开机自启动失败：{error}");
                }
            }
            tray::setup_tray_menu(app.handle(), runtime.clone())?;
            if let Some(window) = monitor_window.as_ref() {
                #[cfg(not(target_os = "windows"))]
                monitor_window::normalize_monitor_size(window);
                if let Err(error) = monitor_window::position_monitor_window(window, &runtime) {
                    eprintln!("设置悬浮条初始位置失败：{error:#}");
                }
            }
            monitor::start(app.handle().clone(), runtime.clone())?;
            if let Some(window) = monitor_window {
                let app_for_menu = app.handle().clone();
                window.on_menu_event(move |_window, event| match event.id().as_ref() {
                    "monitor-lock-now" => {
                        if let Some(state) = app_for_menu.try_state::<Arc<NodeRuntime>>() {
                            if let Err(error) =
                                apply_monitor_locked(&app_for_menu, state.as_ref(), true)
                            {
                                eprintln!("{error}");
                            }
                        }
                    }
                    "monitor-hide-now" => hide_monitor_persisted(&app_for_menu),
                    _ => {}
                });
                let app_for_close = app.handle().clone();
                window.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        hide_monitor_persisted(&app_for_close);
                    }
                });
            }
            if let Some(window) = app.get_webview_window("main") {
                if !autostart_launch {
                    set_main_window_visible(app.handle(), true);
                }
                let app_for_close = app.handle().clone();
                window.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        set_main_window_visible(&app_for_close, false);
                    }
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            crate::node_base_url,
            save_ca_certificate,
            crate::remote_file_access,
            crate::set_remote_file_access,
            monitor_window::popup_monitor_menu,
            monitor_window::set_monitor_position,
            links::open_project_page,
            links::open_transfer_location,
            terminal::commands::terminal_open,
            terminal::commands::terminal_close,
            terminal::commands::terminal_resize,
            terminal::commands::terminal_write,
            terminal::commands::terminal_ack,
            terminal::commands::terminal_settings,
            terminal::commands::terminal_set_enabled,
            terminal::commands::terminal_pair,
            terminal::commands::terminal_decide,
            terminal::commands::terminal_revoke
        ])
        .build(tauri::generate_context!())
        .expect("error while building MeshShare")
        .run(|app, event| match event {
            RunEvent::ExitRequested { .. } | RunEvent::Exit => {
                if let Some(state) = app.try_state::<Arc<NodeRuntime>>() {
                    state
                        .stopping
                        .store(true, std::sync::atomic::Ordering::SeqCst);
                    state.terminals.close_all();
                    state.ssh.shutdown();
                    if matches!(event, RunEvent::Exit) {
                        state.wait_for_terminal_shutdown();
                    }
                }
            }
            #[cfg(target_os = "macos")]
            RunEvent::Reopen { .. } => set_main_window_visible(app, true),
            _ => {}
        });
}
