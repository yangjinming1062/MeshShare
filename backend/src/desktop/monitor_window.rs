#[cfg(target_os = "linux")]
use crate::linux;
#[cfg(target_os = "windows")]
use crate::windows;
use crate::{current_monitor_capabilities, model::MonitorPosition, NodeRuntime};
use anyhow::Context;
use std::sync::Arc;
use tauri::{
    menu::{Menu, MenuItem},
    AppHandle, LogicalSize, Manager, PhysicalRect, PhysicalSize, State,
};

#[tauri::command]
pub(super) fn popup_monitor_menu(app: AppHandle) -> Result<(), String> {
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
pub(super) fn set_monitor_position(
    app: AppHandle,
    state: State<'_, Arc<NodeRuntime>>,
    x: i32,
    y: i32,
) -> Result<(), String> {
    if !current_monitor_capabilities(&app).can_persist_position {
        return Err("当前桌面环境不支持保存悬浮条全局坐标".to_string());
    }
    let position = MonitorPosition { x, y };
    #[cfg(target_os = "windows")]
    {
        let window = app
            .get_webview_window("monitor")
            .ok_or_else(|| "悬浮条窗口不存在".to_string())?;
        let current =
            windows::fullscreen::on_screen_position(&window).map_err(|error| error.to_string())?;
        // WebView 会延迟提交移动事件。只保存当前仍在屏幕上的实际位置。
        if current.as_ref() != Some(&position)
            || state.monitor_position().as_ref() == Some(&position)
        {
            return Ok(());
        }
    }
    state
        .set_monitor_position(position)
        .map_err(|error| error.to_string())
}

const MONITOR_MARGIN_PX: i32 = 16;

const MONITOR_WIDTH: f64 = 220.0;

const MONITOR_HEIGHT: f64 = 50.0;

#[cfg(not(target_os = "windows"))]
pub(super) fn normalize_monitor_size<R: tauri::Runtime>(window: &tauri::WebviewWindow<R>) {
    if let Err(error) = window.set_size(LogicalSize::new(MONITOR_WIDTH, MONITOR_HEIGHT)) {
        eprintln!("设置监控条尺寸失败：{error}");
    }
}

fn point_is_in_work_area(position: &MonitorPosition, area: &PhysicalRect<i32, u32>) -> bool {
    let left = i64::from(area.position.x);
    let top = i64::from(area.position.y);
    let right = left + i64::from(area.size.width);
    let bottom = top + i64::from(area.size.height);
    let x = i64::from(position.x);
    let y = i64::from(position.y);
    x >= left && x < right && y >= top && y < bottom
}

fn clamp_position_to_work_area(
    position: Option<MonitorPosition>,
    area: &PhysicalRect<i32, u32>,
    size: PhysicalSize<u32>,
) -> MonitorPosition {
    let margin = i64::from(MONITOR_MARGIN_PX);
    let (x, y) = (i64::from(area.position.x), i64::from(area.position.y));
    let left = x + margin;
    let top = y + margin;
    let right = (x + i64::from(area.size.width) - i64::from(size.width) - margin).max(left);
    let bottom = (y + i64::from(area.size.height) - i64::from(size.height) - margin).max(top);
    let (x, y) = position
        .map(|position| (i64::from(position.x), i64::from(position.y)))
        .unwrap_or((right, bottom));
    MonitorPosition {
        x: x.clamp(left, right) as i32,
        y: y.clamp(top, bottom) as i32,
    }
}

pub(crate) fn position_monitor_window(
    window: &tauri::WebviewWindow,
    runtime: &NodeRuntime,
) -> anyhow::Result<()> {
    if !current_monitor_capabilities(window.app_handle()).can_persist_position {
        return Ok(());
    }
    let monitors = window.available_monitors()?;
    let first = monitors.first().context("未找到活动显示器")?;
    let saved = runtime.monitor_position().and_then(|position| {
        monitors
            .iter()
            .find(|item| point_is_in_work_area(&position, item.work_area()))
            .map(|item| (Some(position), item.clone()))
    });
    let (position, monitor) = saved.unwrap_or_else(|| {
        let monitor = window
            .primary_monitor()
            .ok()
            .flatten()
            .unwrap_or_else(|| first.clone());
        (None, monitor)
    });
    let area = monitor.work_area();
    anyhow::ensure!(
        area.size.width > 0 && area.size.height > 0,
        "显示器工作区尚未就绪"
    );
    #[cfg(target_os = "windows")]
    let size = LogicalSize::new(MONITOR_WIDTH, MONITOR_HEIGHT).to_physical(monitor.scale_factor());
    #[cfg(not(target_os = "windows"))]
    let size = window.outer_size()?;
    let position = clamp_position_to_work_area(position, area, size);
    #[cfg(target_os = "windows")]
    let position = windows::fullscreen::place_hidden(window, position, size)?;
    #[cfg(not(target_os = "windows"))]
    let physical = tauri::PhysicalPosition::new(position.x, position.y);
    #[cfg(target_os = "linux")]
    linux::fullscreen::set_initial_position(window, physical)?;
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    window.set_position(physical)?;
    if runtime.monitor_position().as_ref() != Some(&position) {
        if let Err(error) = runtime.set_monitor_position(position) {
            eprintln!("保存悬浮条位置失败：{error}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tauri::PhysicalPosition;
    #[test]
    fn monitor_position_is_clamped_inside_work_area() {
        let work_area = PhysicalRect {
            position: PhysicalPosition::new(0, 0),
            size: PhysicalSize::new(1920, 1080),
        };
        let size = PhysicalSize::new(440, 42);
        assert_eq!(
            clamp_position_to_work_area(
                Some(MonitorPosition { x: -500, y: -500 }),
                &work_area,
                size,
            ),
            MonitorPosition { x: 16, y: 16 }
        );
        assert_eq!(
            clamp_position_to_work_area(
                Some(MonitorPosition { x: 4000, y: 4000 }),
                &work_area,
                size,
            ),
            MonitorPosition { x: 1464, y: 1022 }
        );
    }

    #[test]
    fn monitor_position_preserves_a_negative_display_and_falls_back_at_destination_scale() {
        let secondary = PhysicalRect {
            position: PhysicalPosition::new(-1920, -160),
            size: PhysicalSize::new(1920, 1040),
        };
        let primary = PhysicalRect {
            position: PhysicalPosition::new(0, 0),
            size: PhysicalSize::new(5120, 2100),
        };
        let saved = MonitorPosition { x: -800, y: -50 };
        let parked = MonitorPosition {
            x: -32000,
            y: -32000,
        };
        let logical_size = LogicalSize::new(MONITOR_WIDTH, MONITOR_HEIGHT);
        assert!(point_is_in_work_area(&saved, &secondary));
        assert!(!point_is_in_work_area(&saved, &primary));
        assert!(!point_is_in_work_area(&parked, &secondary));
        assert!(!point_is_in_work_area(&parked, &primary));
        assert_eq!(
            clamp_position_to_work_area(
                Some(saved.clone()),
                &secondary,
                logical_size.to_physical(1.5),
            ),
            saved,
        );
        assert_eq!(
            clamp_position_to_work_area(None, &primary, logical_size.to_physical(1.25)),
            MonitorPosition { x: 4829, y: 2021 },
        );
    }
}
