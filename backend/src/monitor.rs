//! 桌面悬浮条的临时显示状态；用户偏好仍由节点配置持有。

#[cfg(target_os = "linux")]
use crate::linux::fullscreen as platform;
#[cfg(target_os = "macos")]
use crate::macos::fullscreen as platform;
use crate::runtime::NodeRuntime;
#[cfg(target_os = "windows")]
use crate::windows::fullscreen as platform;
use anyhow::{Context, Result};
use parking_lot::Mutex;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

const SAMPLE_INTERVAL: Duration = Duration::from_millis(500);
const FAILURE_GRACE: Duration = Duration::from_secs(2);

#[path = "monitor/geometry.rs"]
mod geometry;
pub(crate) use geometry::{visible_fullscreen, Rect, Target, Window};

struct FullscreenState {
    initialized: bool,
    suppressed: bool,
    clear_samples: u8,
    error_since: Option<Instant>,
}

impl Default for FullscreenState {
    fn default() -> Self {
        Self {
            initialized: false,
            suppressed: true,
            clear_samples: 0,
            error_since: None,
        }
    }
}

impl FullscreenState {
    fn observe(&mut self, observation: Option<bool>, now: Instant) {
        match observation {
            Some(fullscreen) => {
                self.error_since = None;
                if fullscreen {
                    self.suppressed = true;
                    self.clear_samples = 0;
                } else if !self.initialized {
                    self.suppressed = false;
                } else {
                    self.clear_samples = self.clear_samples.saturating_add(1);
                    if self.clear_samples >= 2 {
                        self.suppressed = false;
                    }
                }
                self.initialized = true;
            }
            None => {
                self.clear_samples = 0;
                let since = *self.error_since.get_or_insert(now);
                if now.saturating_duration_since(since) >= FAILURE_GRACE {
                    self.initialized = true;
                    self.suppressed = false;
                }
            }
        }
    }

    fn presentation(&self, visible: bool, locked: bool) -> (bool, bool) {
        let visible = visible && !self.suppressed;
        (visible, visible && locked)
    }
}

#[derive(Default)]
struct Presentation {
    fullscreen: FullscreenState,
    target: Option<Target>,
    applied: Option<(bool, bool)>,
    detection_error: Option<String>,
    window_error: Option<String>,
}

#[derive(Default)]
pub(crate) struct Controller(Mutex<Presentation>);

impl Presentation {
    fn observe_sample(
        &mut self,
        expected: Option<Target>,
        current: Option<Target>,
        result: Result<bool, String>,
        now: Instant,
    ) -> bool {
        if expected.is_some() && expected != current {
            return false;
        }
        if self.target != expected {
            self.target = expected;
            self.fullscreen.clear_samples = 0;
        }
        self.fullscreen.observe(result.as_ref().ok().copied(), now);
        log_change(&mut self.detection_error, result.err(), "全屏窗口检测");
        true
    }
}

fn log_change(previous: &mut Option<String>, error: Option<String>, context: &str) {
    if *previous != error {
        match &error {
            Some(error) => eprintln!("{context}失败：{error}"),
            None if previous.is_some() => eprintln!("{context}已恢复"),
            None => {}
        }
        *previous = error;
    }
}

/// 仅在主线程调用；不要用 GTK 的 is_visible() 判断异步 show/hide 是否完成。
fn reconcile(app: &AppHandle, reapply: bool) {
    let (Some(controller), Some(node), Some(window)) = (
        app.try_state::<Controller>(),
        app.try_state::<crate::ManagedNode>(),
        app.get_webview_window("monitor"),
    ) else {
        return;
    };
    if node
        .runtime
        .stopping
        .load(std::sync::atomic::Ordering::SeqCst)
    {
        return;
    }
    let mut state = controller.0.lock();
    let desired = state.fullscreen.presentation(
        node.runtime.monitor_visible(),
        crate::effective_monitor_locked(app, &node.runtime),
    );
    if reapply {
        state.applied = None;
    }
    if state.applied == Some(desired) {
        return;
    }
    let result = (|| -> Result<()> {
        if crate::current_monitor_capabilities(app).can_click_through {
            window.set_ignore_cursor_events(desired.1)?;
        }
        // Win32 的穿透样式变更也可能按 Tao 的初始隐藏标记隐藏窗口，因此每次都提交显隐。
        #[cfg(target_os = "windows")]
        platform::set_visible(&window, desired.0)?;
        #[cfg(not(target_os = "windows"))]
        if state.applied.map(|value| value.0) != Some(desired.0) {
            #[cfg(target_os = "linux")]
            platform::set_visible(&window, desired.0)?;
            #[cfg(not(target_os = "linux"))]
            if desired.0 {
                window.show()?;
            } else {
                window.hide()?;
            }
        }
        Ok(())
    })();
    if result.is_ok() {
        state.applied = Some(desired);
    }
    log_change(
        &mut state.window_error,
        result.err().map(|error| error.to_string()),
        "悬浮条窗口更新",
    );
}

pub(crate) fn refresh(app: &AppHandle) -> Result<(), String> {
    let handle = app.clone();
    // Windows 的实际窗口层级可能与上次应用的状态不同，手动操作应重新提交。
    let reapply = cfg!(target_os = "windows");
    app.run_on_main_thread(move || reconcile(&handle, reapply))
        .map_err(|error| error.to_string())?;
    crate::sync_tray_labels(app);
    Ok(())
}

/// 每轮只允许一个主线程回调在途，主线程忙时不积累过期采样或显隐命令。
fn on_main<T: Send + 'static>(
    app: &AppHandle,
    runtime: &NodeRuntime,
    operation: impl FnOnce(&AppHandle) -> T + Send + 'static,
) -> Result<T> {
    let (sender, receiver) = mpsc::sync_channel(1);
    let handle = app.clone();
    app.run_on_main_thread(move || {
        let _ = sender.send(operation(&handle));
    })?;
    loop {
        match receiver.recv_timeout(SAMPLE_INTERVAL) {
            Ok(value) => return Ok(value),
            Err(mpsc::RecvTimeoutError::Disconnected) => anyhow::bail!("窗口主循环已关闭"),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if runtime.stopping.load(std::sync::atomic::Ordering::SeqCst) {
                    anyhow::bail!("节点正在停止");
                }
            }
        }
    }
}

fn capture(app: &AppHandle) -> Result<Option<Target>> {
    let window = app
        .get_webview_window("monitor")
        .context("悬浮条窗口不存在")?;
    platform::capture(&window)
}

pub(crate) fn start(app: AppHandle, runtime: Arc<NodeRuntime>) -> Result<()> {
    std::thread::Builder::new()
        .name("meshshare-monitor".into())
        .spawn(move || {
            let mut probe = platform::Probe::default();
            while !runtime.stopping.load(std::sync::atomic::Ordering::SeqCst) {
                let started = Instant::now();
                let captured = match on_main(&app, &runtime, capture) {
                    Ok(value) => value,
                    Err(error) => {
                        if !runtime.stopping.load(std::sync::atomic::Ordering::SeqCst) {
                            eprintln!("悬浮条检测任务停止：{error:#}");
                        }
                        break;
                    }
                };
                let target = captured.as_ref().ok().copied().flatten();
                let result = captured
                    .and_then(|target| match target {
                        Some(target) => probe.fullscreen(target),
                        None => Ok(false),
                    })
                    .map_err(|error| format!("{error:#}"));
                if let Err(error) = on_main(&app, &runtime, move |app| {
                    let Some(controller) = app.try_state::<Controller>() else {
                        return;
                    };
                    // 拖到另一屏或改变显示布局时，不能应用旧屏幕的检测结果。
                    let current = capture(app).ok().flatten();
                    let mut state = controller.0.lock();
                    if !state.observe_sample(target, current, result, Instant::now()) {
                        return;
                    }
                    drop(state);
                    reconcile(app, false);
                }) {
                    if !runtime.stopping.load(std::sync::atomic::Ordering::SeqCst) {
                        eprintln!("应用悬浮条检测结果失败，任务已停止：{error:#}");
                    }
                    break;
                }
                std::thread::sleep(SAMPLE_INTERVAL.saturating_sub(started.elapsed()));
            }
        })
        .context("无法启动悬浮条检测线程")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Rect = Rect {
        left: -1920,
        top: 0,
        right: 0,
        bottom: 1080,
    };
    fn window(bounds: Rect, fullscreen: bool, opaque: bool) -> Window {
        Window {
            bounds,
            fullscreen,
            opaque,
        }
    }

    #[test]
    fn fullscreen_on_other_display_does_not_hide_monitor() {
        assert!(!visible_fullscreen(
            SCREEN,
            &[window(Rect::new(0, 0, 2560, 1440), true, true)]
        ));
        assert!(visible_fullscreen(SCREEN, &[window(SCREEN, true, true)]));
        assert!(!visible_fullscreen(SCREEN, &[window(SCREEN, false, true)]));
    }

    #[test]
    fn only_complete_opaque_occlusion_removes_fullscreen() {
        let half = Rect::new(-1920, 0, 960, 1080);
        assert!(visible_fullscreen(
            SCREEN,
            &[window(half, false, true), window(SCREEN, true, true)]
        ));
        assert!(visible_fullscreen(
            SCREEN,
            &[window(SCREEN, false, false), window(SCREEN, true, true)]
        ));
        assert!(!visible_fullscreen(
            SCREEN,
            &[
                window(half, false, true),
                window(Rect::new(-960, 0, 960, 1080), false, true),
                window(SCREEN, true, true),
            ]
        ));
    }

    #[test]
    fn maximized_window_and_system_panel_together_cover_background_fullscreen() {
        let maximized = window(Rect::new(-1920, 0, 1920, 1040), false, true);
        let panel = window(Rect::new(-1920, 1040, 1920, 40), false, true);
        assert!(!visible_fullscreen(
            SCREEN,
            &[panel, maximized, window(SCREEN, true, true)]
        ));
        assert!(visible_fullscreen(
            SCREEN,
            &[
                window(SCREEN, true, true),
                window(Rect::new(-1920, 1040, 1920, 40), false, true),
            ]
        ));
        let target = Target {
            window_id: 1,
            screen: SCREEN,
            fullscreen_area: SCREEN,
            maximized_area: Some(Rect::new(-1920, 0, 1920, 1040)),
        };
        assert!(!visible_fullscreen(
            target.visibility_area(),
            &[
                window(target.maximized_area.unwrap(), false, true),
                window(SCREEN, true, true),
            ]
        ));
    }

    #[test]
    fn client_bounds_distinguish_maximized_and_fullscreen() {
        assert!(!Rect::new(-1920, 30, 1920, 1050).covers(SCREEN));
        assert!(Rect::new(-1919, 1, 1918, 1078).covers(SCREEN));
        assert!(Rect::new(-1920, 0, 4480, 1440).covers(SCREEN));
    }

    #[test]
    fn preferences_and_lock_survive_suppression_and_restoration() {
        let now = Instant::now();
        let mut state = FullscreenState::default();
        state.observe(Some(true), now);
        assert_eq!(state.presentation(true, true), (false, false));
        state.observe(Some(false), now + SAMPLE_INTERVAL);
        assert_eq!(state.presentation(true, true), (false, false));
        state.observe(Some(false), now + SAMPLE_INTERVAL * 2);
        assert_eq!(state.presentation(true, true), (true, true));
        assert_eq!(state.presentation(false, true), (false, false));
        assert_eq!(state.presentation(true, false), (true, false));
    }

    #[test]
    fn failures_break_clear_sequence_and_eventually_release_suppression() {
        let now = Instant::now();
        let mut state = FullscreenState::default();
        state.observe(Some(true), now);
        state.observe(Some(false), now);
        state.observe(None, now);
        state.observe(Some(false), now);
        assert!(state.suppressed);
        state.observe(None, now);
        state.observe(None, now + FAILURE_GRACE);
        assert!(!state.suppressed);
        state.observe(Some(true), now + FAILURE_GRACE);
        assert!(state.suppressed);
    }

    #[test]
    fn first_clear_sample_can_show_without_startup_flash() {
        let mut state = FullscreenState::default();
        assert_eq!(state.presentation(true, false), (false, false));
        state.observe(Some(false), Instant::now());
        assert_eq!(state.presentation(true, false), (true, false));
    }

    #[test]
    fn moving_between_displays_discards_inflight_results_and_restarts_confirmation() {
        let first = Target {
            window_id: 1,
            screen: SCREEN,
            fullscreen_area: SCREEN,
            maximized_area: None,
        };
        let second = Target {
            screen: Rect::new(0, 0, 2560, 1440),
            ..first
        };
        let now = Instant::now();
        let mut state = Presentation::default();
        assert!(state.observe_sample(Some(first), Some(first), Ok(true), now));
        assert!(state.observe_sample(Some(first), Some(first), Ok(false), now));
        assert!(!state.observe_sample(Some(first), Some(second), Ok(false), now));
        assert!(state.fullscreen.suppressed);
        assert!(state.observe_sample(Some(second), Some(second), Ok(false), now));
        assert!(state.fullscreen.suppressed);
        assert!(state.observe_sample(Some(second), Some(second), Ok(false), now));
        assert!(!state.fullscreen.suppressed);
    }

    #[test]
    fn manual_hiding_during_fullscreen_wins_over_later_samples() {
        let mut state = FullscreenState::default();
        let now = Instant::now();
        state.observe(Some(true), now);
        assert_eq!(state.presentation(false, true), (false, false));
        state.observe(Some(false), now);
        state.observe(Some(false), now);
        assert_eq!(state.presentation(false, true), (false, false));
        assert_eq!(state.presentation(true, true), (true, true));
        state.observe(Some(true), now);
        assert_eq!(state.presentation(true, true), (false, false));
    }
}
