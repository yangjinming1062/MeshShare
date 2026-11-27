use super::window::point_is_exposed;
use anyhow::{Context, Result};
use std::cell::RefCell;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr::{null, null_mut};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager, WebviewWindow};
use windows_sys::Win32::Foundation::{
    ERROR_HOOK_NOT_INSTALLED, ERROR_INVALID_HOOK_HANDLE, HWND, LPARAM, LRESULT, POINT,
    WAIT_OBJECT_0, WAIT_TIMEOUT, WPARAM,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::{CreateEventW, SetEvent, WaitForSingleObject};
use windows_sys::Win32::UI::HiDpi::{
    GetDpiForWindow, GetSystemMetricsForDpi, SetThreadDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, MsgWaitForMultipleObjectsEx, PeekMessageW, SetWindowsHookExW,
    UnhookWindowsHookEx, HC_ACTION, HHOOK, MSG, MSLLHOOKSTRUCT, MWMO_INPUTAVAILABLE, PM_REMOVE,
    QS_ALLINPUT, SM_CXDOUBLECLK, SM_CYDOUBLECLK, WH_MOUSE_LL, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MBUTTONDOWN, WM_MOUSEMOVE, WM_QUIT, WM_RBUTTONDOWN, WM_XBUTTONDOWN,
};

const HOOK_RENEWAL_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Clone, Copy)]
struct Click {
    point: POINT,
    time: u32,
}

struct Limits {
    time: u32,
    radius_x: u32,
    radius_y: u32,
}

impl Limits {
    fn for_window(window: HWND) -> Self {
        // 低级鼠标事件使用按显示器 DPI 感知的屏幕坐标。
        unsafe {
            let dpi = GetDpiForWindow(window);
            Self {
                time: GetDoubleClickTime(),
                radius_x: (GetSystemMetricsForDpi(SM_CXDOUBLECLK, dpi) / 2).max(1) as u32,
                radius_y: (GetSystemMetricsForDpi(SM_CYDOUBLECLK, dpi) / 2).max(1) as u32,
            }
        }
    }

    fn nearby(&self, first: Click, second: Click) -> bool {
        first.point.x.abs_diff(second.point.x) <= self.radius_x
            && first.point.y.abs_diff(second.point.y) <= self.radius_y
    }

    fn matches(&self, first: Click, second: Click) -> bool {
        second.time.wrapping_sub(first.time) <= self.time && self.nearby(first, second)
    }
}

#[derive(Default)]
struct DoubleClick {
    down: Option<Click>,
    first: Option<Click>,
}

impl DoubleClick {
    fn observe(&mut self, message: u32, click: Click, inside: bool, limits: &Limits) -> bool {
        match message {
            WM_LBUTTONDOWN if inside => self.down = Some(click),
            WM_LBUTTONUP if inside => {
                if let Some(down) = self.down.take().filter(|down| limits.matches(*down, click)) {
                    if self.first.is_some_and(|first| limits.matches(first, down)) {
                        self.first = None;
                        return true;
                    }
                    self.first = Some(down);
                } else {
                    self.first = None;
                }
            }
            WM_MOUSEMOVE => {
                if self.down.is_some_and(|down| !limits.nearby(down, click)) {
                    *self = Self::default();
                }
            }
            _ => *self = Self::default(),
        }
        false
    }
}

struct Session {
    stop: OwnedHandle,
    unlock_queued: AtomicBool,
}

impl Session {
    fn new() -> Result<Self> {
        let event = unsafe { CreateEventW(null(), 1, 0, null()) };
        anyhow::ensure!(!event.is_null(), std::io::Error::last_os_error());
        Ok(Self {
            stop: unsafe { OwnedHandle::from_raw_handle(event) },
            unlock_queued: AtomicBool::new(false),
        })
    }

    fn stopping(&self) -> bool {
        // 手动复位事件一旦发出就保持取消，等待中的主线程回调也会看到它。
        unsafe { WaitForSingleObject(self.stop.as_raw_handle(), 0) != WAIT_TIMEOUT }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Target {
    window: usize,
    revision: u64,
    corner_radius: u32,
}

type UnlockCallback = fn(&AppHandle, u64) -> Result<()>;

struct Worker {
    target: Target,
    session: Arc<Session>,
    status: mpsc::Receiver<Result<()>>,
    installed: bool,
}

impl Worker {
    fn spawn(target: Target, app: AppHandle, unlock: UnlockCallback) -> Result<Self> {
        let session = Arc::new(Session::new().context("创建鼠标监听停止事件")?);
        let worker_session = session.clone();
        let (sender, status) = mpsc::channel();
        std::thread::Builder::new()
            .name("meshshare-monitor-mouse".into())
            .spawn(move || {
                if let Err(error) = run_worker(target, &app, &worker_session, &sender, unlock) {
                    let _ = sender.send(Err(error));
                }
            })
            .context("启动鼠标消息线程")?;
        Ok(Self {
            target,
            session,
            status,
            installed: false,
        })
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // 工作线程持有事件直至卸载钩子。停止时不在窗口主线程等待它退出。
        if unsafe { SetEvent(self.session.stop.as_raw_handle()) } == 0 {
            eprintln!(
                "停止悬浮条鼠标监听失败：{}",
                std::io::Error::last_os_error()
            );
        }
    }
}

#[derive(Default)]
pub(crate) struct Listener(Option<Worker>);

impl Listener {
    pub(crate) fn poll(
        &mut self,
        window: &WebviewWindow,
        revision: u64,
        corner_radius: u32,
        unlock: UnlockCallback,
    ) -> Result<bool> {
        let hwnd = match window.hwnd() {
            Ok(handle) => handle.0 as usize,
            Err(error) => {
                self.stop();
                return Err(error.into());
            }
        };
        let target = Target {
            window: hwnd,
            revision,
            corner_radius,
        };
        self.poll_worker(target, || {
            Worker::spawn(target, window.app_handle().clone(), unlock)
        })
    }

    fn poll_worker(
        &mut self,
        target: Target,
        spawn: impl FnOnce() -> Result<Worker>,
    ) -> Result<bool> {
        if self
            .0
            .as_ref()
            .is_some_and(|worker| worker.target != target)
        {
            self.stop();
        }
        let worker = match self.0 {
            Some(ref mut worker) => worker,
            None => self.0.insert(spawn()?),
        };
        let error = match worker.status.try_recv() {
            Ok(Ok(())) => {
                worker.installed = true;
                return Ok(true);
            }
            Err(mpsc::TryRecvError::Empty) => return Ok(worker.installed),
            Ok(Err(error)) => error,
            Err(mpsc::TryRecvError::Disconnected) => anyhow::anyhow!("鼠标消息线程已退出"),
        };
        self.stop();
        Err(error)
    }

    pub(crate) fn stop(&mut self) {
        self.0 = None;
    }
}

struct Hook(HHOOK);

impl Hook {
    fn install() -> Result<Self> {
        let hook = unsafe {
            let module = GetModuleHandleW(null());
            SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_event), module, 0)
        };
        anyhow::ensure!(!hook.is_null(), std::io::Error::last_os_error());
        Ok(Self(hook))
    }

    fn remove(&mut self) -> Result<bool> {
        if self.0.is_null() {
            return Ok(false);
        }
        let removed = unsafe { UnhookWindowsHookEx(self.0) } != 0;
        if !removed {
            let error = std::io::Error::last_os_error();
            if !matches!(
                error.raw_os_error().map(|code| code as u32),
                Some(ERROR_INVALID_HOOK_HANDLE | ERROR_HOOK_NOT_INSTALLED)
            ) {
                return Err(error).context("卸载低级鼠标钩子");
            }
        }
        self.0 = null_mut();
        Ok(removed)
    }
}

impl Drop for Hook {
    fn drop(&mut self) {
        let _ = self.remove();
    }
}

struct Input {
    window: HWND,
    corner_radius: u32,
    limits: Limits,
    clicks: DoubleClick,
    unlock: bool,
}

thread_local! {
    // WH_MOUSE_LL 只在安装钩子的鼠标消息线程回调。
    static INPUT: RefCell<Option<Input>> = const { RefCell::new(None) };
}

fn run_worker(
    target: Target,
    app: &AppHandle,
    session: &Arc<Session>,
    status: &mpsc::Sender<Result<()>>,
    unlock: UnlockCallback,
) -> Result<()> {
    if session.stopping() {
        return Ok(());
    }
    // 令窗口矩形与 MSLLHOOKSTRUCT.pt 使用相同的屏幕坐标。
    let dpi = unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE) };
    anyhow::ensure!(
        !dpi.is_null(),
        "设置鼠标消息线程的 DPI 感知失败：{}",
        std::io::Error::last_os_error()
    );
    let mut hook = Hook::install().context("安装低级鼠标钩子")?;
    let window = target.window as HWND;
    INPUT.set(Some(Input {
        window,
        corner_radius: target.corner_radius,
        limits: Limits::for_window(window),
        clicks: DoubleClick::default(),
        unlock: false,
    }));
    if status.send(Ok(())).is_err() {
        return Ok(());
    }

    let handle = session.stop.as_raw_handle();
    let mut message = MSG::default();
    let mut renew_at = Instant::now() + HOOK_RENEWAL_INTERVAL;
    loop {
        if Instant::now() >= renew_at {
            if session.stopping() {
                return Ok(());
            }
            // Windows 超时移除钩子时没有通知。定期重建，也避免同时安装两个钩子。
            if !hook.remove()? {
                INPUT.with_borrow_mut(|slot| {
                    if let Some(input) = slot {
                        input.clicks = DoubleClick::default();
                    }
                });
            }
            hook = Hook::install().context("重建低级鼠标钩子")?;
            renew_at = Instant::now() + HOOK_RENEWAL_INTERVAL;
        }
        let timeout = renew_at
            .saturating_duration_since(Instant::now())
            .as_millis()
            .max(1) as u32;
        let wait = unsafe {
            MsgWaitForMultipleObjectsEx(1, &handle, timeout, QS_ALLINPUT, MWMO_INPUTAVAILABLE)
        };
        if wait == WAIT_OBJECT_0 {
            return Ok(());
        }
        if wait == WAIT_TIMEOUT {
            continue;
        }
        anyhow::ensure!(
            wait == WAIT_OBJECT_0 + 1,
            "等待鼠标消息失败：{}",
            std::io::Error::last_os_error()
        );
        // PeekMessageW 会先交付发给本线程的钩子回调。本线程没有窗口。
        let received = unsafe { PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) } != 0;
        anyhow::ensure!(
            !received || message.message != WM_QUIT,
            "鼠标消息循环已退出"
        );
        if INPUT.with_borrow_mut(|slot| {
            slot.as_mut()
                .is_some_and(|input| std::mem::take(&mut input.unlock))
        }) {
            queue_unlock(app, session, target.revision, unlock)?;
        }
    }
}

unsafe extern "system" fn mouse_event(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let message = wparam as u32;
    if code == HC_ACTION as i32
        && matches!(
            message,
            WM_LBUTTONDOWN
                | WM_LBUTTONUP
                | WM_MOUSEMOVE
                | WM_RBUTTONDOWN
                | WM_MBUTTONDOWN
                | WM_XBUTTONDOWN
        )
    {
        // HC_ACTION 提供仅在本次回调期间有效的鼠标事件。
        let event = unsafe { &*(lparam as *const MSLLHOOKSTRUCT) };
        let _ = INPUT.try_with(|slot| {
            let Ok(mut slot) = slot.try_borrow_mut() else {
                return;
            };
            let Some(input) = slot.as_mut() else {
                return;
            };
            let check_point = message == WM_LBUTTONDOWN
                || (message == WM_LBUTTONUP && input.clicks.down.is_some());
            let inside =
                check_point && point_is_exposed(input.window, event.pt, input.corner_radius);
            if inside && message == WM_LBUTTONDOWN {
                input.limits = Limits::for_window(input.window);
            }
            let click = Click {
                point: event.pt,
                time: event.time,
            };
            input.unlock |= input.clicks.observe(message, click, inside, &input.limits);
        });
    }
    // 监听期间保持底层窗口的原生点击行为。
    unsafe { CallNextHookEx(null_mut(), code, wparam, lparam) }
}

fn queue_unlock(
    app: &AppHandle,
    session: &Arc<Session>,
    revision: u64,
    unlock: UnlockCallback,
) -> Result<()> {
    if session.stopping() || session.unlock_queued.swap(true, Ordering::AcqRel) {
        return Ok(());
    }
    let handle = app.clone();
    let session = session.clone();
    app.run_on_main_thread(move || {
        if !session.stopping() {
            if let Err(error) = unlock(&handle, revision) {
                eprintln!("双击解锁悬浮条失败：{error}");
            }
        }
        session.unlock_queued.store(false, Ordering::Release);
    })
    .context("将悬浮条双击交给窗口主线程")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_installation_can_retry_for_the_same_window() {
        let target = Target {
            window: 1,
            revision: 0,
            corner_radius: 10,
        };
        let mut listener = Listener::default();
        let failed = Arc::new(Session::new().unwrap());
        let (send_error, status) = mpsc::channel();
        send_error
            .send(Err(anyhow::anyhow!("模拟安装失败")))
            .unwrap();
        let first = listener.poll_worker(target, || {
            Ok(Worker {
                target,
                session: failed.clone(),
                status,
                installed: false,
            })
        });
        assert_eq!(first.unwrap_err().to_string(), "模拟安装失败");
        assert!(failed.stopping());

        let active = Arc::new(Session::new().unwrap());
        let (send_ready, status) = mpsc::channel();
        let pending = listener.poll_worker(target, || {
            Ok(Worker {
                target,
                session: active.clone(),
                status,
                installed: false,
            })
        });
        assert!(!pending.unwrap());
        send_ready.send(Ok(())).unwrap();
        assert!(listener
            .poll_worker(target, || panic!("监听已安装"))
            .unwrap());
        listener.stop();
        assert!(active.stopping());
    }

    #[test]
    fn unlock_requires_two_completed_clicks_over_the_bar() {
        let limits = Limits {
            time: 500,
            radius_x: 3,
            radius_y: 3,
        };
        let mut clicks = DoubleClick::default();
        let mut send = |message, time, x, inside| {
            clicks.observe(
                message,
                Click {
                    point: POINT { x, y: 10 },
                    time,
                },
                inside,
                &limits,
            )
        };
        assert!(!send(WM_LBUTTONDOWN, 10, 10, false));
        assert!(!send(WM_LBUTTONUP, 20, 10, false));
        assert!(!send(WM_LBUTTONDOWN, 50, 10, true));
        assert!(!send(WM_LBUTTONUP, 60, 10, true));
        assert!(!send(WM_LBUTTONDOWN, 100, 11, true));
        assert!(send(WM_LBUTTONUP, 110, 11, true));
        assert!(!send(WM_LBUTTONDOWN, 150, 11, true));
        assert!(!send(WM_LBUTTONUP, 160, 11, true));
    }

    #[test]
    fn stale_distant_and_dragged_clicks_do_not_complete_the_gesture() {
        let limits = Limits {
            time: 500,
            radius_x: 3,
            radius_y: 3,
        };
        let mut clicks = DoubleClick::default();
        let mut send = |message, time, x| {
            clicks.observe(
                message,
                Click {
                    point: POINT { x, y: 10 },
                    time,
                },
                true,
                &limits,
            )
        };
        assert!(!send(WM_LBUTTONDOWN, 10, 10));
        assert!(!send(WM_LBUTTONUP, 20, 10));
        assert!(!send(WM_LBUTTONDOWN, 550, 10));
        assert!(!send(WM_LBUTTONUP, 560, 10));
        assert!(!send(WM_LBUTTONDOWN, 570, 30));
        assert!(!send(WM_LBUTTONUP, 580, 30));
        assert!(!send(WM_LBUTTONDOWN, 590, 30));
        assert!(!send(WM_MOUSEMOVE, 600, 50));
        assert!(!send(WM_LBUTTONUP, 610, 30));
    }
}
