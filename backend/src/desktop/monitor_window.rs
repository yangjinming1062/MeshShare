#[cfg(target_os = "windows")]
use crate::windows;
use crate::{
    current_monitor_capabilities, effective_monitor_locked,
    model::{HardwareMetrics, MonitorPosition},
    MonitorCapabilities, NodeRuntime,
};
#[cfg(not(target_os = "linux"))]
use anyhow::Context;
use std::sync::Arc;
use tauri::{
    menu::{Menu, MenuItem},
    AppHandle, Manager, PhysicalRect, PhysicalSize, State,
};
#[cfg(target_os = "macos")]
use tauri::{LogicalPosition, PhysicalPosition};
#[cfg(not(target_os = "linux"))]
use tauri::{LogicalSize, WebviewWindow};

#[derive(serde::Serialize)]
pub(super) struct MonitorView {
    metrics: Option<HardwareMetrics>,
    capabilities: MonitorCapabilities,
    locked: bool,
}

#[tauri::command]
pub(super) fn monitor_view(app: AppHandle, state: State<'_, Arc<NodeRuntime>>) -> MonitorView {
    MonitorView {
        metrics: state.local_metrics(),
        capabilities: current_monitor_capabilities(&app),
        locked: effective_monitor_locked(&app, &state),
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn get(app: &AppHandle) -> Option<WebviewWindow> {
    app.get_webview_window("monitor")
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn initialize(window: &WebviewWindow, runtime: &NodeRuntime) {
    #[cfg(not(target_os = "windows"))]
    if let Err(error) = window.set_size(LogicalSize::new(MONITOR_WIDTH, MONITOR_HEIGHT)) {
        eprintln!("设置监控条尺寸失败：{error}");
    }
    if let Err(error) = position_monitor_window(window, runtime) {
        eprintln!("设置悬浮条初始位置失败：{error:#}");
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn release_window(window: &WebviewWindow, runtime: &NodeRuntime) -> anyhow::Result<()> {
    if current_monitor_capabilities(window.app_handle()).can_persist_position
        && window.is_visible().unwrap_or(false)
    {
        if let Err(error) = save_visible_position(window, runtime) {
            eprintln!("保存悬浮条位置失败：{error:#}");
        }
    }
    Ok(window.destroy()?)
}

#[cfg(not(target_os = "linux"))]
fn save_visible_position(window: &WebviewWindow, runtime: &NodeRuntime) -> anyhow::Result<()> {
    if let Some(position) = current_position(window)? {
        if runtime.monitor_position().as_ref() != Some(&position) {
            runtime.set_monitor_position(position)?;
        }
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn current_position(window: &WebviewWindow) -> anyhow::Result<Option<MonitorPosition>> {
    #[cfg(target_os = "windows")]
    return windows::fullscreen::on_screen_position(window);

    #[cfg(not(target_os = "windows"))]
    {
        let position = window.outer_position()?;
        let position = MonitorPosition::new(position.x, position.y);
        #[cfg(target_os = "macos")]
        let position = MonitorPosition {
            scale_factor: Some(window.scale_factor()?),
            ..position
        };
        Ok(Some(position))
    }
}

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
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let position = {
        let window = app
            .get_webview_window("monitor")
            .ok_or_else(|| "悬浮条窗口不存在".to_string())?;
        let current = current_position(&window).map_err(|error| error.to_string())?;
        // WebView 会延迟提交移动事件。只保存当前仍在屏幕上的实际位置。
        let Some(position) = current.filter(|point| (point.x, point.y) == (x, y)) else {
            return Ok(());
        };
        position
    };
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let position = MonitorPosition::new(x, y);
    if state.monitor_position().as_ref() == Some(&position) {
        return Ok(());
    }
    state
        .set_monitor_position(position)
        .map_err(|error| error.to_string())
}

const MONITOR_MARGIN_PX: i32 = 16;

pub(crate) const MONITOR_WIDTH: i32 = 220;

pub(crate) const MONITOR_HEIGHT: i32 = 50;

pub(crate) fn point_is_in_work_area(
    position: &MonitorPosition,
    area: &PhysicalRect<i32, u32>,
) -> bool {
    let left = i64::from(area.position.x);
    let top = i64::from(area.position.y);
    let right = left + i64::from(area.size.width);
    let bottom = top + i64::from(area.size.height);
    let x = i64::from(position.x);
    let y = i64::from(position.y);
    x >= left && x < right && y >= top && y < bottom
}

pub(crate) fn clamp_position_to_work_area(
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
    MonitorPosition::new(x.clamp(left, right) as i32, y.clamp(top, bottom) as i32)
}

#[cfg(target_os = "macos")]
fn macos_monitor_overlap(
    point: LogicalPosition<f64>,
    bounds: PhysicalRect<i32, u32>,
    scale: f64,
) -> f64 {
    let origin = bounds.position.to_logical::<f64>(scale);
    let size = bounds.size.to_logical::<f64>(scale);
    let width =
        (point.x + f64::from(MONITOR_WIDTH)).min(origin.x + size.width) - point.x.max(origin.x);
    let height =
        (point.y + f64::from(MONITOR_HEIGHT)).min(origin.y + size.height) - point.y.max(origin.y);
    width.max(0.0) * height.max(0.0)
}

#[cfg(not(target_os = "linux"))]
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
        #[cfg(target_os = "macos")]
        if let Some(scale) = position
            .scale_factor
            .filter(|scale| scale.is_finite() && *scale > 0.0)
        {
            let point = PhysicalPosition::new(position.x, position.y).to_logical::<f64>(scale);
            // AppKit 按窗口与屏幕的交集确定归属。只看左上角会选错跨屏窗口。
            let (monitor, _) = monitors
                .iter()
                .filter_map(|monitor| {
                    let bounds = PhysicalRect {
                        position: *monitor.position(),
                        size: *monitor.size(),
                    };
                    let overlap = macos_monitor_overlap(point, bounds, monitor.scale_factor());
                    (overlap > 0.0).then_some((monitor, overlap))
                })
                .max_by(|(left, left_area), (right, right_area)| {
                    left_area.total_cmp(right_area).then_with(|| {
                        (left.scale_factor() == scale).cmp(&(right.scale_factor() == scale))
                    })
                })?;
            let physical = point.to_physical(monitor.scale_factor());
            return Some((
                Some(MonitorPosition::new(physical.x, physical.y)),
                monitor.clone(),
            ));
        }
        // 缺少比例的旧配置沿用所在工作区判断。
        monitors
            .iter()
            .find(|monitor| point_is_in_work_area(&position, monitor.work_area()))
            .map(|monitor| (Some(position), monitor.clone()))
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
    let size = LogicalSize::new(MONITOR_WIDTH, MONITOR_HEIGHT).to_physical(monitor.scale_factor());
    let position = clamp_position_to_work_area(position, area, size);
    #[cfg(target_os = "windows")]
    let position = windows::fullscreen::place_hidden(window, position, size)?;
    #[cfg(target_os = "macos")]
    let position = {
        let scale = monitor.scale_factor();
        // 物理坐标会被 Tao 按新窗口当前屏幕的比例换算。
        let logical = PhysicalPosition::new(position.x, position.y).to_logical::<f64>(scale);
        window.set_position(logical)?;
        MonitorPosition {
            scale_factor: Some(scale),
            ..position
        }
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let physical = tauri::PhysicalPosition::new(position.x, position.y);
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
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
    use tauri::{LogicalSize, PhysicalPosition};
    #[test]
    fn monitor_position_is_clamped_inside_work_area() {
        let work_area = PhysicalRect {
            position: PhysicalPosition::new(0, 0),
            size: PhysicalSize::new(1920, 1080),
        };
        let size = PhysicalSize::new(440, 42);
        assert_eq!(
            clamp_position_to_work_area(Some(MonitorPosition::new(-500, -500)), &work_area, size,),
            MonitorPosition::new(16, 16)
        );
        assert_eq!(
            clamp_position_to_work_area(Some(MonitorPosition::new(4000, 4000)), &work_area, size,),
            MonitorPosition::new(1464, 1022)
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
        let saved = MonitorPosition::new(-800, -50);
        let parked = MonitorPosition::new(-32000, -32000);
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
            MonitorPosition::new(4829, 2021),
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_restores_logical_position_across_mixed_scale_displays() {
        let legacy: MonitorPosition = serde_json::from_str(r#"{"x":-1000,"y":80}"#).unwrap();
        assert!(legacy.scale_factor.is_none());
        let saved = MonitorPosition {
            scale_factor: Some(1.0),
            ..legacy
        };
        let left = PhysicalRect {
            position: PhysicalPosition::new(-1920, 0),
            size: PhysicalSize::new(1920, 1040),
        };

        // 右侧 1× 副屏和 2× 主屏的物理坐标重叠，桌面的逻辑坐标可确定目标屏。
        let primary = PhysicalRect {
            position: PhysicalPosition::new(0, 0),
            size: PhysicalSize::new(2880, 1800),
        };
        let right = PhysicalRect {
            position: PhysicalPosition::new(1440, 0),
            size: PhysicalSize::new(1920, 1080),
        };
        assert!(point_is_in_work_area(&saved, &left));
        let point = PhysicalPosition::new(saved.x, saved.y).to_logical(saved.scale_factor.unwrap());
        assert_eq!(macos_monitor_overlap(point, left, 1.0), 11_000.0);
        assert_eq!(macos_monitor_overlap(point, primary, 2.0), 0.0);
        let point = LogicalPosition::new(1700.0, 100.0);
        assert_eq!(macos_monitor_overlap(point, primary, 2.0), 0.0);
        assert_eq!(macos_monitor_overlap(point, right, 1.0), 11_000.0);
        // 左上角在主屏，窗口主体仍在右侧副屏。
        let point = LogicalPosition::new(1380.0, 100.0);
        assert_eq!(macos_monitor_overlap(point, primary, 2.0), 3_000.0);
        assert_eq!(macos_monitor_overlap(point, right, 1.0), 8_000.0);
        let stored = serde_json::to_vec(&saved).unwrap();
        assert_eq!(
            serde_json::from_slice::<MonitorPosition>(&stored).unwrap(),
            saved
        );
    }
}
