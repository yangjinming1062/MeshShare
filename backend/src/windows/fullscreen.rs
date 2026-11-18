use crate::monitor::{visible_fullscreen, Rect, Target, Window};
use anyhow::Result;
use std::mem::size_of;
use windows_sys::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows_sys::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows_sys::Win32::Graphics::Gdi::{
    ClientToScreen, GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetClientRect, GetLayeredWindowAttributes, GetWindowLongW,
    GetWindowRect, IsIconic, IsWindowVisible, SetWindowPos, GWL_EXSTYLE, LWA_ALPHA, SWP_HIDEWINDOW,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SWP_SHOWWINDOW, WS_EX_LAYERED,
    WS_EX_TOOLWINDOW,
};

fn rect(value: RECT) -> Rect {
    Rect {
        left: value.left.into(),
        top: value.top.into(),
        right: value.right.into(),
        bottom: value.bottom.into(),
    }
}

pub(crate) fn set_visible(window: &tauri::WebviewWindow, visible: bool) -> Result<()> {
    let hwnd = window.hwnd()?.0 as HWND;
    let visibility = if visible {
        SWP_SHOWWINDOW
    } else {
        SWP_HIDEWINDOW
    };
    let flags = visibility | SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER;
    // 悬浮条由主线程显隐。强制保留前台焦点，尤其是从全屏恢复时。
    let applied = unsafe { SetWindowPos(hwnd, std::ptr::null_mut(), 0, 0, 0, 0, flags) };
    anyhow::ensure!(applied != 0, std::io::Error::last_os_error());
    Ok(())
}

pub(crate) fn capture(window: &tauri::WebviewWindow) -> Result<Option<Target>> {
    let hwnd = window.hwnd()?.0 as HWND;
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // HWND 来自仍存活的 Tauri 窗口；输出缓冲区按 Win32 类型和大小初始化。
    unsafe {
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
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
        let mut cloaked = 0u32;
        if DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED as u32,
            (&mut cloaked as *mut u32).cast(),
            size_of::<u32>() as u32,
        ) >= 0
            && cloaked != 0
        {
            return None;
        }
        let mut bounds = RECT::default();
        if DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS as u32,
            (&mut bounds as *mut RECT).cast(),
            size_of::<RECT>() as u32,
        ) < 0
            && GetWindowRect(hwnd, &mut bounds) == 0
        {
            return None;
        }
        let bounds = rect(bounds);
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
