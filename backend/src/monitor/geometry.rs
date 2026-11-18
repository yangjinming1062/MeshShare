#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Rect {
    pub left: i64,
    pub top: i64,
    pub right: i64,
    pub bottom: i64,
}

impl Rect {
    pub fn new(left: i64, top: i64, width: i64, height: i64) -> Self {
        Self {
            left,
            top,
            right: left + width,
            bottom: top + height,
        }
    }

    pub fn intersection(self, other: Self) -> Option<Self> {
        let rect = Self {
            left: self.left.max(other.left),
            top: self.top.max(other.top),
            right: self.right.min(other.right),
            bottom: self.bottom.min(other.bottom),
        };
        (rect.right > rect.left && rect.bottom > rect.top).then_some(rect)
    }

    pub fn covers(self, other: Self) -> bool {
        self.left <= other.left + 2
            && self.top <= other.top + 2
            && self.right >= other.right - 2
            && self.bottom >= other.bottom - 2
    }

    fn subtract(self, cover: Self, remaining: &mut Vec<Self>) {
        let Some(overlap) = self.intersection(cover) else {
            remaining.push(self);
            return;
        };
        for rect in [
            Self {
                bottom: overlap.top,
                ..self
            },
            Self {
                top: overlap.bottom,
                ..self
            },
            Self {
                top: overlap.top,
                bottom: overlap.bottom,
                right: overlap.left,
                ..self
            },
            Self {
                top: overlap.top,
                bottom: overlap.bottom,
                left: overlap.right,
                ..self
            },
        ] {
            if rect.right > rect.left && rect.bottom > rect.top {
                remaining.push(rect);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Target {
    pub window_id: u64,
    pub screen: Rect,
    // macOS 的窗口可能只覆盖刘海屏安全区域；其余平台与 screen 相同。
    pub fullscreen_area: Rect,
    // 普通窗口可覆盖的工作区，用于遮挡判断；macOS 也用它排除最大化歧义。
    pub maximized_area: Option<Rect>,
}

impl Target {
    pub fn visibility_area(self) -> Rect {
        self.maximized_area
            .and_then(|area| area.intersection(self.screen))
            .unwrap_or(self.screen)
    }
}

pub(crate) struct Window {
    pub bounds: Rect,
    pub fullscreen: bool,
    pub opaque: bool,
}

/// 窗口按前到后排列，只扣除不透明区域；系统面板只能参与遮挡，不能成为全屏候选。
pub(crate) fn visible_fullscreen(screen: Rect, windows: &[Window]) -> bool {
    let mut remaining = vec![screen];
    for window in windows {
        if window.fullscreen
            && remaining
                .iter()
                .any(|rect| rect.intersection(window.bounds).is_some())
        {
            return true;
        }
        if window.opaque {
            let mut next = Vec::new();
            for rect in remaining {
                rect.subtract(window.bounds, &mut next);
            }
            remaining = next;
            if remaining.is_empty() {
                break;
            }
        }
    }
    false
}
