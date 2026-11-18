use crate::monitor::{visible_fullscreen, Rect, Target, Window};
use anyhow::{Context, Result};
use core_foundation::{
    base::{CFType, TCFType},
    dictionary::CFDictionary,
    number::CFNumber,
    string::CFString,
};
use core_graphics::{
    display::CGDisplay,
    window::{
        copy_window_info, kCGNullWindowID, kCGWindowListExcludeDesktopElements,
        kCGWindowListOptionOnScreenOnly,
    },
};
use objc2::{runtime::NSObjectProtocol, sel, MainThreadMarker};
use objc2_app_kit::NSWindow;

pub(crate) fn capture(window: &tauri::WebviewWindow) -> Result<Option<Target>> {
    let _main = MainThreadMarker::new().context("显示器信息必须在主线程读取")?;
    let pointer = window.ns_window()?;
    // Tauri 窗口在整个主线程回调期间存活；不把 AppKit 对象传到检测线程。
    let native =
        unsafe { (pointer as *const NSWindow).as_ref() }.context("原生悬浮条窗口不存在")?;
    let screen = native.screen().context("悬浮条尚未关联显示器")?;
    let frame = screen.frame();
    let primary_height = CGDisplay::main().bounds().size.height;
    let convert = |x: f64, y: f64, width: f64, height: f64| {
        Rect::new(
            x.round() as i64,
            (primary_height - y - height).round() as i64,
            width.round() as i64,
            height.round() as i64,
        )
    };
    let bounds = convert(
        frame.origin.x,
        frame.origin.y,
        frame.size.width,
        frame.size.height,
    );
    let visible = screen.visibleFrame();
    let maximized_area = Some(convert(
        visible.origin.x,
        visible.origin.y,
        visible.size.width,
        visible.size.height,
    ));
    let mut fullscreen_area = bounds;
    // safeAreaInsets 从 macOS 12 开始提供，旧系统仍按完整显示器检测。
    if screen.respondsToSelector(sel!(safeAreaInsets)) {
        let insets = screen.safeAreaInsets();
        fullscreen_area = Rect {
            left: bounds.left + insets.left.round() as i64,
            top: bounds.top + insets.top.round() as i64,
            right: bounds.right - insets.right.round() as i64,
            bottom: bounds.bottom - insets.bottom.round() as i64,
        };
    }
    Ok(Some(Target {
        window_id: native.windowNumber() as u64,
        screen: bounds,
        fullscreen_area,
        maximized_area,
    }))
}

fn value(dictionary: &CFDictionary, key: &str) -> Option<CFType> {
    let key = CFString::new(key);
    let pointer = dictionary.find(key.as_CFTypeRef())?;
    // CFDictionary 的值仍由 dictionary 保活；wrap_under_get_rule 增加引用计数。
    Some(unsafe { CFType::wrap_under_get_rule(*pointer) })
}

fn number(dictionary: &CFDictionary, key: &str) -> Option<f64> {
    value(dictionary, key)?.downcast::<CFNumber>()?.to_f64()
}

fn describe(dictionary: &CFDictionary, target: Target) -> Option<Window> {
    if number(dictionary, "kCGWindowNumber")? as u64 == target.window_id
        || number(dictionary, "kCGWindowLayer")? != 0.0
    {
        return None;
    }
    let alpha = number(dictionary, "kCGWindowAlpha")?;
    if alpha <= 0.0 {
        return None;
    }
    let bounds = value(dictionary, "kCGWindowBounds")?.downcast::<CFDictionary>()?;
    let (x, y, width, height) = (
        number(&bounds, "X")?,
        number(&bounds, "Y")?,
        number(&bounds, "Width")?,
        number(&bounds, "Height")?,
    );
    if ![x, y, width, height].iter().all(|value| value.is_finite()) || width <= 0.0 || height <= 0.0
    {
        return None;
    }
    let bounds = Rect::new(
        x.round() as i64,
        y.round() as i64,
        width.round() as i64,
        height.round() as i64,
    );
    bounds.intersection(target.screen)?;
    // 完整屏幕与安全区都可能等于普通最大化区域，此时不猜测为全屏。
    let ambiguous = target
        .maximized_area
        .is_some_and(|area| area.covers(bounds) && bounds.covers(area));
    let fullscreen =
        !ambiguous && (bounds.covers(target.screen) || bounds.covers(target.fullscreen_area));
    let opaque = alpha >= 1.0;
    Some(Window {
        bounds,
        fullscreen: fullscreen && opaque,
        opaque,
    })
}

#[derive(Default)]
pub(crate) struct Probe {}

impl Probe {
    pub fn fullscreen(&mut self, target: Target) -> Result<bool> {
        let list = copy_window_info(
            kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements,
            kCGNullWindowID,
        )
        .context("无法读取当前空间的窗口列表")?;
        let mut windows = Vec::new();
        for pointer in list.iter() {
            // Quartz 返回保活的 CFDictionary 数组；先检查类型再读取各字段。
            let value = unsafe { CFType::wrap_under_get_rule(*pointer) };
            if let Some(dictionary) = value.downcast::<CFDictionary>() {
                if let Some(window) = describe(&dictionary, target) {
                    windows.push(window);
                }
            }
        }
        Ok(visible_fullscreen(target.visibility_area(), &windows))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(bounds: Rect, id: i64, layer: i64, alpha: f64) -> CFDictionary {
        let bounds = CFDictionary::from_CFType_pairs(&[
            (CFString::new("X"), CFNumber::from(bounds.left).as_CFType()),
            (CFString::new("Y"), CFNumber::from(bounds.top).as_CFType()),
            (
                CFString::new("Width"),
                CFNumber::from(bounds.right - bounds.left).as_CFType(),
            ),
            (
                CFString::new("Height"),
                CFNumber::from(bounds.bottom - bounds.top).as_CFType(),
            ),
        ]);
        CFDictionary::from_CFType_pairs(&[
            (
                CFString::new("kCGWindowNumber"),
                CFNumber::from(id).as_CFType(),
            ),
            (
                CFString::new("kCGWindowLayer"),
                CFNumber::from(layer).as_CFType(),
            ),
            (
                CFString::new("kCGWindowAlpha"),
                CFNumber::from(alpha).as_CFType(),
            ),
            (CFString::new("kCGWindowBounds"), bounds.as_CFType()),
        ])
        .into_untyped()
    }

    fn target() -> Target {
        let screen = Rect::new(-1512, 0, 1512, 982);
        Target {
            window_id: 1,
            screen,
            fullscreen_area: Rect::new(-1512, 32, 1512, 950),
            maximized_area: Some(Rect::new(-1512, 32, 1512, 900)),
        }
    }

    #[test]
    fn quartz_metadata_distinguishes_maximized_fullscreen_and_notch_safe_area() {
        let target = target();
        assert!(
            describe(&fixture(target.screen, 2, 0, 1.0), target)
                .unwrap()
                .fullscreen
        );
        assert!(
            describe(&fixture(target.fullscreen_area, 2, 0, 1.0), target)
                .unwrap()
                .fullscreen
        );
        assert!(
            !describe(&fixture(target.maximized_area.unwrap(), 2, 0, 1.0), target)
                .unwrap()
                .fullscreen
        );
        let ambiguous = Target {
            maximized_area: Some(target.fullscreen_area),
            ..target
        };
        assert!(
            !describe(&fixture(target.fullscreen_area, 2, 0, 1.0), ambiguous)
                .unwrap()
                .fullscreen
        );
        let rounded = Target {
            maximized_area: Some(Rect::new(-1512, 33, 1512, 949)),
            ..target
        };
        assert!(
            !describe(
                &fixture(rounded.maximized_area.unwrap(), 2, 0, 1.0),
                rounded
            )
            .unwrap()
            .fullscreen
        );
    }

    #[test]
    fn own_overlay_and_system_layers_are_excluded_and_transparency_does_not_occlude() {
        let target = target();
        assert!(describe(&fixture(target.screen, 1, 0, 1.0), target).is_none());
        assert!(describe(&fixture(target.screen, 2, 24, 1.0), target).is_none());
        let transparent = describe(&fixture(target.screen, 2, 0, 0.5), target).unwrap();
        assert!(!transparent.fullscreen);
        assert!(!transparent.opaque);
    }

    #[test]
    fn full_screen_work_area_does_not_turn_maximized_windows_into_fullscreen() {
        let screen = Rect::new(-1920, 0, 1920, 1080);
        let target = Target {
            window_id: 1,
            screen,
            fullscreen_area: screen,
            maximized_area: Some(screen),
        };
        for bounds in [screen, Rect::new(-1919, 1, 1918, 1078)] {
            assert!(
                !describe(&fixture(bounds, 2, 0, 1.0), target)
                    .unwrap()
                    .fullscreen
            );
        }
        let reserved = Target {
            maximized_area: Some(Rect::new(-1920, 24, 1920, 1000)),
            ..target
        };
        assert!(
            describe(&fixture(screen, 2, 0, 1.0), reserved)
                .unwrap()
                .fullscreen
        );
    }
}
