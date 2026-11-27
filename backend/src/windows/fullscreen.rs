use super::window::{frame_bounds, is_cloaked};
use crate::model::MonitorPosition;
use crate::monitor::{visible_fullscreen, Rect, Target, Window};
use anyhow::{Context, Result};
use std::mem::size_of;
use tauri::{PhysicalSize, WebviewWindow};
use windows_sys::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    ClientToScreen, GetMonitorInfoW, MonitorFromRect, HMONITOR, MONITORINFO, MONITOR_DEFAULTTONULL,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetClientRect, GetLayeredWindowAttributes, GetWindowLongW,
    GetWindowRect, IsIconic, IsWindowVisible, SetWindowPos, GWL_EXSTYLE, HWND_TOPMOST, LWA_ALPHA,
    SWP_HIDEWINDOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW,
    WS_EX_LAYERED, WS_EX_TOOLWINDOW,
};

fn rect(value: RECT) -> Rect {
    Rect {
        left: value.left.into(),
        top: value.top.into(),
        right: value.right.into(),
        bottom: value.bottom.into(),
    }
}

struct Placement {
    bounds: RECT,
    monitor: HMONITOR,
}

impl Placement {
    fn is_on_screen(&self) -> bool {
        !self.monitor.is_null()
            && self.bounds.left < self.bounds.right
            && self.bounds.top < self.bounds.bottom
    }

    fn position(&self) -> MonitorPosition {
        MonitorPosition {
            x: self.bounds.left,
            y: self.bounds.top,
        }
    }
}

fn placement(hwnd: HWND) -> Result<Option<Placement>> {
    let mut bounds = RECT::default();
    // 最小化时的实际矩形是系统临时坐标，等窗口恢复后再判断位置。
    unsafe {
        if IsIconic(hwnd) != 0 {
            return Ok(None);
        }
        anyhow::ensure!(
            GetWindowRect(hwnd, &mut bounds) != 0,
            std::io::Error::last_os_error()
        );
        let monitor = MonitorFromRect(&bounds, MONITOR_DEFAULTTONULL);
        Ok(Some(Placement { bounds, monitor }))
    }
}

fn on_screen(hwnd: HWND) -> Result<Placement> {
    placement(hwnd)?
        .filter(Placement::is_on_screen)
        .context("悬浮条尚未位于活动显示器上")
}

pub(crate) fn needs_repositioning(window: &WebviewWindow) -> Result<bool> {
    Ok(placement(window.hwnd()?.0 as HWND)?.is_some_and(|placement| !placement.is_on_screen()))
}

pub(crate) fn on_screen_position(window: &WebviewWindow) -> Result<Option<MonitorPosition>> {
    Ok(placement(window.hwnd()?.0 as HWND)?
        .filter(Placement::is_on_screen)
        .map(|placement| placement.position()))
}

pub(crate) fn place_hidden(
    window: &WebviewWindow,
    position: MonitorPosition,
    size: PhysicalSize<u32>,
) -> Result<MonitorPosition> {
    let hwnd = window.hwnd()?.0 as HWND;
    let (width, height) = (i32::try_from(size.width)?, i32::try_from(size.height)?);
    // 切屏后系统可能同时改变窗口尺寸。恢复时保持隐藏，等待新位置的全屏采样。
    let placed = unsafe {
        SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            position.x,
            position.y,
            width,
            height,
            SWP_HIDEWINDOW | SWP_NOACTIVATE | SWP_NOZORDER,
        )
    };
    anyhow::ensure!(placed != 0, std::io::Error::last_os_error());
    Ok(on_screen(hwnd)?.position())
}

pub(crate) fn set_visible(window: &WebviewWindow, visible: bool) -> Result<()> {
    let hwnd = window.hwnd()?.0 as HWND;
    let (insert_after, visibility) = if visible {
        on_screen(hwnd)?;
        (HWND_TOPMOST, SWP_SHOWWINDOW)
    } else {
        (std::ptr::null_mut(), SWP_HIDEWINDOW | SWP_NOZORDER)
    };
    let flags = visibility | SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE;
    // 可见窗口仍可能被压在普通窗口后面，显示时显式恢复置顶。
    let applied = unsafe { SetWindowPos(hwnd, insert_after, 0, 0, 0, 0, flags) };
    anyhow::ensure!(applied != 0, std::io::Error::last_os_error());
    Ok(())
}

pub(crate) fn capture(window: &WebviewWindow) -> Result<Option<Target>> {
    let hwnd = window.hwnd()?.0 as HWND;
    let monitor = on_screen(hwnd)?.monitor;
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // HWND 来自仍存活的 Tauri 窗口；输出缓冲区按 Win32 类型和大小初始化。
    unsafe {
        anyhow::ensure!(
            GetMonitorInfoW(monitor, &mut info) != 0,
            std::io::Error::last_os_error()
        );
    }
    let screen = rect(info.rcMonitor);
    Ok(Some(Target {
        window_id: hwnd as u64,
        screen,
        fullscreen_area: screen,
        maximized_area: Some(rect(info.rcWork)),
    }))
}

#[derive(Default)]
pub(crate) struct Probe {}

struct Enumeration {
    target: Target,
    windows: Vec<Window>,
}

unsafe extern "system" fn collect(hwnd: HWND, data: LPARAM) -> i32 {
    // EnumWindows 同步调用回调，data 只在该调用内指向栈上的 Enumeration。
    let enumeration = unsafe { &mut *(data as *mut Enumeration) };
    if let Some(window) = unsafe { describe(hwnd, enumeration.target) } {
        enumeration.windows.push(window);
    }
    1
}

unsafe fn describe(hwnd: HWND, target: Target) -> Option<Window> {
    // 窗口在枚举期间可能关闭；查询失败按已消失跳过，不发送跨进程窗口消息。
    unsafe {
        if hwnd as u64 == target.window_id || IsWindowVisible(hwnd) == 0 || IsIconic(hwnd) != 0 {
            return None;
        }
        let mut class = [0u16; 128];
        let length = GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32);
        let class = String::from_utf16_lossy(&class[..length.max(0) as usize]);
        if matches!(class.as_str(), "Progman" | "WorkerW" | "#32768") {
            return None;
        }
        let taskbar = matches!(class.as_str(), "Shell_TrayWnd" | "Shell_SecondaryTrayWnd");
        let style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        if style & WS_EX_TOOLWINDOW != 0 && !taskbar {
            return None;
        }
        if is_cloaked(hwnd) {
            return None;
        }
        let bounds = rect(frame_bounds(hwnd)?);
        bounds.intersection(target.screen)?;
        // 普通最大化窗口与前方任务栏可共同完全遮住后台全屏窗口。
        // 任务栏只参与遮挡，不能被自身的屏幕边缘几何判为全屏。
        if taskbar {
            return Some(Window {
                bounds,
                fullscreen: false,
                opaque: true,
            });
        }
        let mut client = RECT::default();
        let mut origin = POINT::default();
        let fullscreen = GetClientRect(hwnd, &mut client) != 0
            && ClientToScreen(hwnd, &mut origin) != 0
            && Rect::new(
                origin.x.into(),
                origin.y.into(),
                (client.right - client.left).into(),
                (client.bottom - client.top).into(),
            )
            .covers(target.screen);
        let opaque = if style & WS_EX_LAYERED == 0 {
            true
        } else {
            let (mut color, mut alpha, mut flags) = (0, 0, 0);
            GetLayeredWindowAttributes(hwnd, &mut color, &mut alpha, &mut flags) != 0
                && flags == LWA_ALPHA
                && alpha == 255
        };
        Some(Window {
            bounds,
            fullscreen: fullscreen && opaque,
            opaque,
        })
    }
}

impl Probe {
    pub fn fullscreen(&mut self, target: Target) -> Result<bool> {
        let mut enumeration = Enumeration {
            target,
            windows: Vec::new(),
        };
        // EnumWindows 按当前 Z 序枚举顶层窗口；本进程悬浮条不参与遮挡。
        let success = unsafe {
            EnumWindows(
                Some(collect),
                (&mut enumeration as *mut Enumeration) as LPARAM,
            )
        };
        anyhow::ensure!(success != 0, std::io::Error::last_os_error());
        Ok(visible_fullscreen(
            target.visibility_area(),
            &enumeration.windows,
        ))
    }
}
