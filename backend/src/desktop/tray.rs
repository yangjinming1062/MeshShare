use crate::{apply_monitor_locked, apply_monitor_visible, effective_monitor_locked, NodeRuntime};
use anyhow::Context;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, TrayIconEvent},
    AppHandle, Manager,
};

struct TrayHandles {
    window_toggle: MenuItem<tauri::Wry>,
    monitor_toggle: MenuItem<tauri::Wry>,
    lock_toggle: MenuItem<tauri::Wry>,
    sync_toggle: MenuItem<tauri::Wry>,
}

// Linux 下 show/hide 由 GTK 主循环异步执行，is_visible() 会读到变更前的状态，因此托盘文字以命令意图为准。
pub(super) struct MainWindowVisibility(AtomicBool);

impl MainWindowVisibility {
    pub(super) fn new(visible: bool) -> Self {
        Self(AtomicBool::new(visible))
    }

    fn get(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    fn set(&self, visible: bool) {
        self.0.store(visible, Ordering::SeqCst);
    }
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

fn clipboard_sync_toggle_label(enabled: bool) -> &'static str {
    if enabled {
        "暂停剪贴板同步"
    } else {
        "恢复剪贴板同步"
    }
}

fn main_window_visible(app: &AppHandle) -> bool {
    app.try_state::<MainWindowVisibility>()
        .is_some_and(|state| state.get())
}

pub(crate) fn sync_tray_labels(app: &AppHandle) {
    let Some(tray) = app.try_state::<TrayHandles>() else {
        return;
    };
    let node = app.state::<Arc<NodeRuntime>>();
    let _ = tray
        .window_toggle
        .set_text(window_toggle_label(main_window_visible(app)));
    let _ = tray
        .monitor_toggle
        .set_text(monitor_toggle_label(node.monitor_visible()));
    let _ = tray
        .lock_toggle
        .set_text(monitor_lock_toggle_label(effective_monitor_locked(
            app, &node,
        )));
    let _ = tray.sync_toggle.set_text(clipboard_sync_toggle_label(
        node.sync_enabled.load(Ordering::SeqCst),
    ));
}

pub(super) fn set_main_window_visible(app: &AppHandle, visible: bool) {
    if let Some(state) = app.try_state::<MainWindowVisibility>() {
        state.set(visible);
    }
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

fn handle_tray_icon_event(app: &AppHandle, event: TrayIconEvent) {
    if event.id().as_ref() != "meshshare"
        || !matches!(
            event,
            TrayIconEvent::DoubleClick {
                button: MouseButton::Left,
                ..
            }
        )
    {
        return;
    }

    set_main_window_visible(app, !main_window_visible(app));
}

pub(super) fn setup_tray_menu(app: &AppHandle, node: Arc<NodeRuntime>) -> anyhow::Result<()> {
    let tray = app
        .tray_by_id("meshshare")
        .context("MeshShare tray icon is missing")?;
    let item = |id: &str, label: &str| MenuItem::with_id(app, id, label, true, None::<&str>);
    let window_toggle = item(
        "toggle-window",
        window_toggle_label(main_window_visible(app)),
    )?;
    let monitor_toggle = item(
        "toggle-monitor",
        monitor_toggle_label(node.monitor_visible()),
    )?;
    let lock_toggle = item(
        "monitor-lock-toggle",
        monitor_lock_toggle_label(effective_monitor_locked(app, &node)),
    )?;
    let sync_toggle = item(
        "toggle-sync",
        clipboard_sync_toggle_label(node.sync_enabled.load(Ordering::SeqCst)),
    )?;
    let exit = item("exit", "退出程序")?;
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
    app.on_tray_icon_event(handle_tray_icon_event);
    tray.on_menu_event(move |app, event| match event.id().as_ref() {
        "toggle-window" => set_main_window_visible(app, !main_window_visible(app)),
        "toggle-monitor" => {
            if let Err(error) = apply_monitor_visible(app, &node, !node.monitor_visible()) {
                eprintln!("保存悬浮监控条可见性失败：{error}");
            }
        }
        "monitor-lock-toggle" => {
            if let Err(error) =
                apply_monitor_locked(app, &node, !effective_monitor_locked(app, &node))
            {
                eprintln!("{error}");
            }
        }
        "toggle-sync" => {
            node.toggle_sync_enabled();
            sync_tray_labels(app);
        }
        "exit" => {
            node.stopping.store(true, Ordering::SeqCst);
            app.exit(0);
        }
        _ => {}
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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

    #[test]
    fn main_window_visibility_keeps_last_command() {
        let state = MainWindowVisibility::new(false);
        assert!(!state.get());
        state.set(true);
        assert!(state.get());
        state.set(false);
        assert!(!state.get());
    }
}
