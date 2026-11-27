use std::mem::size_of;
use std::ptr::null_mut;
use windows_sys::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows_sys::Win32::Graphics::Dwm::{
    DwmGetWindowAttribute, DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS,
};
use windows_sys::Win32::Graphics::Gdi::{
    ClientToScreen, CreateRectRgn, DeleteObject, GetWindowRgn, PtInRect, PtInRegion, RGN_ERROR,
};
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClientRect, GetLayeredWindowAttributes, GetWindowLongW, GetWindowRect,
    IsIconic, IsWindowVisible, GWL_EXSTYLE, LWA_ALPHA, USER_DEFAULT_SCREEN_DPI, WS_EX_LAYERED,
    WS_EX_LAYOUTRTL,
};

pub(super) fn is_cloaked(window: HWND) -> bool {
    let mut cloaked = 0u32;
    let result = unsafe {
        DwmGetWindowAttribute(
            window,
            DWMWA_CLOAKED as u32,
            (&mut cloaked as *mut u32).cast(),
            size_of::<u32>() as u32,
        )
    };
    result >= 0 && cloaked != 0
}

pub(super) fn frame_bounds(window: HWND) -> Option<RECT> {
    let mut bounds = RECT::default();
    let found = unsafe {
        DwmGetWindowAttribute(
            window,
            DWMWA_EXTENDED_FRAME_BOUNDS as u32,
            (&mut bounds as *mut RECT).cast(),
            size_of::<RECT>() as u32,
        ) >= 0
            || GetWindowRect(window, &mut bounds) != 0
    };
    found.then_some(bounds)
}

fn window_at_point(window: HWND, point: POINT) -> Option<RECT> {
    let mut bounds = RECT::default();
    let inside = unsafe {
        IsWindowVisible(window) != 0
            && IsIconic(window) == 0
            && GetWindowRect(window, &mut bounds) != 0
            && PtInRect(&bounds, point) != 0
    };
    (inside && !is_cloaked(window)).then_some(bounds)
}

fn covers_point(window: HWND, point: POINT) -> bool {
    let Some(bounds) = window_at_point(window, point) else {
        return false;
    };
    if !frame_bounds(window).is_some_and(|frame| unsafe { PtInRect(&frame, point) != 0 }) {
        return false;
    }
    unsafe {
        let style = GetWindowLongW(window, GWL_EXSTYLE) as u32;
        if style & WS_EX_LAYERED != 0 {
            let (mut alpha, mut flags) = (0, 0);
            let uniform =
                GetLayeredWindowAttributes(window, null_mut(), &mut alpha, &mut flags) != 0;
            if uniform && flags & LWA_ALPHA != 0 && alpha == 0 {
                return false;
            }
        }
        // 分层窗口可以穿透鼠标并遮挡画面。未知透明度按窗口区域判断。
        let region = CreateRectRgn(0, 0, 0, 0);
        if region.is_null() {
            return true;
        }
        let x = if style & WS_EX_LAYOUTRTL != 0 {
            bounds.right - 1 - point.x
        } else {
            point.x - bounds.left
        };
        let inside = GetWindowRgn(window, region) == RGN_ERROR
            || PtInRegion(region, x, point.y - bounds.top) != 0;
        DeleteObject(region);
        inside
    }
}

struct HitTest {
    window: HWND,
    point: POINT,
    exposed: bool,
}

unsafe extern "system" fn test_in_z_order(window: HWND, data: LPARAM) -> i32 {
    // EnumWindows 同步调用回调，data 只在该调用内指向栈上的 HitTest。
    let hit = unsafe { &mut *(data as *mut HitTest) };
    if window == hit.window {
        hit.exposed = true;
        return 0;
    }
    i32::from(!covers_point(window, hit.point))
}

fn rounded_rect_contains(bounds: RECT, point: POINT, radius: f64) -> bool {
    if unsafe { PtInRect(&bounds, point) } == 0 {
        return false;
    }
    let width = f64::from(bounds.right) - f64::from(bounds.left);
    let height = f64::from(bounds.bottom) - f64::from(bounds.top);
    let radius = radius.min(width / 2.0).min(height / 2.0);
    // CSS 在矩形过窄或过矮时缩小圆角。用像素中心判断是否落在圆内。
    let x = f64::from(point.x) - f64::from(bounds.left) + 0.5;
    let y = f64::from(point.y) - f64::from(bounds.top) + 0.5;
    let dx = x - x.clamp(radius, width - radius);
    let dy = y - y.clamp(radius, height - radius);
    dx * dx + dy * dy <= radius * radius
}

fn bar_contains(window: HWND, point: POINT, corner_radius: u32) -> bool {
    let mut client = RECT::default();
    let mut origin = POINT::default();
    if unsafe {
        GetClientRect(window, &mut client) == 0 || ClientToScreen(window, &mut origin) == 0
    } {
        return false;
    }
    let dpi = unsafe { GetDpiForWindow(window) };
    if dpi == 0 {
        return false;
    }
    let radius = f64::from(corner_radius) * f64::from(dpi) / f64::from(USER_DEFAULT_SCREEN_DPI);
    let local = POINT {
        x: point.x - origin.x,
        y: point.y - origin.y,
    };
    rounded_rect_contains(client, local, radius)
}

pub(super) fn point_is_exposed(window: HWND, point: POINT, corner_radius: u32) -> bool {
    if window_at_point(window, point).is_none() || !bar_contains(window, point, corner_radius) {
        return false;
    }
    let mut hit = HitTest {
        window,
        point,
        exposed: false,
    };
    // 穿透的悬浮条不会成为点击目标，按 Z 序检查它前面的可见窗口。
    unsafe { EnumWindows(Some(test_in_z_order), (&mut hit as *mut HitTest) as LPARAM) };
    hit.exposed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounded_hit_area_excludes_transparent_pixels_at_different_scales() {
        let bounds = RECT {
            left: 0,
            top: 0,
            right: 275,
            bottom: 63,
        };
        assert!(!rounded_rect_contains(bounds, POINT { x: 0, y: 0 }, 12.5));
        assert!(!rounded_rect_contains(
            bounds,
            POINT { x: 274, y: 62 },
            12.5
        ));
        assert!(rounded_rect_contains(bounds, POINT { x: 0, y: 31 }, 12.5));
        assert!(rounded_rect_contains(bounds, POINT { x: 5, y: 5 }, 12.5));
        assert!(!rounded_rect_contains(
            bounds,
            POINT { x: 275, y: 31 },
            12.5
        ));
        assert!(!rounded_rect_contains(bounds, POINT { x: 3, y: 3 }, 12.5));
        assert!(rounded_rect_contains(bounds, POINT { x: 3, y: 3 }, 10.0));
        assert!(rounded_rect_contains(bounds, POINT { x: 0, y: 0 }, 0.0));
    }
}
