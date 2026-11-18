use crate::monitor::{visible_fullscreen, Rect, Target, Window};
use crate::{current_monitor_capabilities, MonitorBackend};
use anyhow::{Context, Result};
use gtk::prelude::{DisplayExtManual, GtkWindowExt, MonitorExt, WidgetExt};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{
    cell::Cell,
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    os::unix::net::UnixStream,
    time::{Duration, Instant},
};
use tauri::{Manager, PhysicalPosition, WebviewWindow};
use x11rb::reexports::x11rb_protocol::{
    parse_display::{parse_display, ConnectAddress},
    xauth::get_auth,
};
use x11rb::{
    connection::Connection,
    errors::ReplyError,
    protocol::{
        xproto::{Atom, AtomEnum, ConnectionExt, MapState},
        ErrorKind,
    },
    rust_connection::{DefaultStream, PollMode, RustConnection, Stream},
    utils::RawFdContainer,
};

const PROBE_BUDGET: Duration = Duration::from_millis(400);

pub(crate) fn prepare(window: &WebviewWindow) -> tauri::Result<MonitorBackend> {
    let native = window.gtk_window()?;
    let backend = native.display().backend();
    if backend.is_x11() {
        // 初次检测需要 XID。仅创建 GDK 资源，此时悬浮条仍未映射。
        native.realize();
        Ok(MonitorBackend::X11)
    } else if backend.is_wayland() {
        Ok(MonitorBackend::Wayland)
    } else {
        Ok(MonitorBackend::Unknown)
    }
}

pub(crate) fn set_initial_position(
    window: &WebviewWindow,
    position: PhysicalPosition<i32>,
) -> tauri::Result<()> {
    let native = window.gtk_window()?;
    let (x, y) = position
        .to_logical::<i32>(f64::from(native.scale_factor()))
        .into();
    // GTK 的异步窗口请求可能在首个检测回调之后处理，初始位置在主线程直接设置。
    native.move_(x, y);
    Ok(())
}

pub(crate) fn set_visible(window: &WebviewWindow, visible: bool) -> tauri::Result<()> {
    if !matches!(
        current_monitor_capabilities(window.app_handle()).backend,
        MonitorBackend::X11
    ) {
        return if visible {
            window.show()
        } else {
            window.hide()
        };
    }

    let native = window.gtk_window()?;
    if visible {
        native.show_all();
    } else {
        // 拖动由窗口管理器完成。隐藏后显式保留实际位置，供未映射时的检测和恢复使用。
        let position = native.is_mapped().then(|| native.position());
        native.hide();
        if let Some((x, y)) = position {
            native.move_(x, y);
        }
    }
    Ok(())
}

pub(crate) fn capture(window: &WebviewWindow) -> Result<Option<Target>> {
    if !matches!(
        current_monitor_capabilities(window.app_handle()).backend,
        MonitorBackend::X11
    ) {
        return Ok(None);
    }
    let handle = window
        .window_handle()
        .map_err(|error| anyhow::anyhow!("无法读取 X11 窗口句柄：{error}"))?;
    let window_id = match handle.as_raw() {
        RawWindowHandle::Xlib(handle) => u64::from(u32::try_from(handle.window)?),
        RawWindowHandle::Xcb(handle) => u64::from(handle.window.get()),
        _ => anyhow::bail!("无法读取 X11 悬浮条窗口标识"),
    };
    let native = window.gtk_window()?;
    let display = native.display();
    let monitor = if native.is_mapped() {
        native
            .window()
            .and_then(|window| display.monitor_at_window(&window))
    } else {
        // 未映射时用 GTK 将应用的位置。GDK 窗口此时可能仍在默认坐标。
        let (x, y) = native.position();
        let (width, height) = native.size();
        display.monitor_at_point(x + width / 2, y + height / 2)
    }
    .or_else(|| display.primary_monitor())
    .context("悬浮条尚未关联显示器")?;
    let scale = i64::from(monitor.scale_factor());
    let physical_rect = |rect: gtk::gdk::Rectangle| {
        Rect::new(
            i64::from(rect.x()) * scale,
            i64::from(rect.y()) * scale,
            i64::from(rect.width()) * scale,
            i64::from(rect.height()) * scale,
        )
    };
    let screen = physical_rect(monitor.geometry());
    Ok(Some(Target {
        window_id,
        screen,
        fullscreen_area: screen,
        maximized_area: Some(physical_rect(monitor.workarea())),
    }))
}

/// x11rb 的默认等待无超时；为连接握手及整轮请求共享一个截止时间。
struct TimedStream {
    inner: DefaultStream,
    deadline: Cell<Instant>,
}

fn poll_fd(fd: i32, events: i16, deadline: Instant) -> io::Result<()> {
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "X11 检测超时"));
        }
        let mut descriptor = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        // fd 由当前连接保活；poll 只借用一个正确大小的 pollfd。
        let result = unsafe { libc::poll(&mut descriptor, 1, left.as_millis().max(1) as i32) };
        if result > 0 {
            return Ok(());
        }
        if result == 0 {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "X11 检测超时"));
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

impl TimedStream {
    fn check(&self) -> io::Result<()> {
        if Instant::now() >= self.deadline.get() {
            Err(io::Error::new(io::ErrorKind::TimedOut, "X11 检测超时"))
        } else {
            Ok(())
        }
    }
}

impl Stream for TimedStream {
    fn poll(&self, mode: PollMode) -> io::Result<()> {
        let events = (if mode.readable() { libc::POLLIN } else { 0 })
            | (if mode.writable() { libc::POLLOUT } else { 0 });
        poll_fd(self.inner.as_raw_fd(), events, self.deadline.get())
    }
    fn read(&self, buffer: &mut [u8], fds: &mut Vec<RawFdContainer>) -> io::Result<usize> {
        self.check()?;
        self.inner.read(buffer, fds)
    }
    fn write(&self, buffer: &[u8], fds: &mut Vec<RawFdContainer>) -> io::Result<usize> {
        self.check()?;
        self.inner.write(buffer, fds)
    }
}

fn local_socket(path: &str, abstract_socket: bool, deadline: Instant) -> io::Result<UnixStream> {
    // 先创建非阻塞 socket，避免服务端失去响应时连 connect 也无限等待。
    let raw = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // 新建 fd 的唯一所有者，之后所有错误路径均自动关闭它。
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let bytes = path.as_bytes();
    if bytes.len() + 1 > address.sun_path.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "X11 socket 路径过长",
        ));
    }
    let offset = usize::from(abstract_socket);
    for (output, input) in address.sun_path[offset..].iter_mut().zip(bytes) {
        *output = *input as libc::c_char;
    }
    let length = std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1;
    let result = unsafe {
        libc::connect(
            fd.as_raw_fd(),
            (&address as *const libc::sockaddr_un).cast(),
            length as libc::socklen_t,
        )
    };
    if result != 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EINPROGRESS) {
            return Err(error);
        }
        poll_fd(fd.as_raw_fd(), libc::POLLOUT, deadline)?;
        let mut error = 0i32;
        let mut size = std::mem::size_of_val(&error) as libc::socklen_t;
        let result = unsafe {
            libc::getsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_ERROR,
                (&mut error as *mut i32).cast(),
                &mut size,
            )
        };
        if result != 0 {
            return Err(io::Error::last_os_error());
        }
        if error != 0 {
            return Err(io::Error::from_raw_os_error(error));
        }
    }
    Ok(UnixStream::from(fd))
}

struct Session {
    connection: RustConnection<TimedStream>,
    root: u32,
    atoms: Atoms,
}

struct Atoms {
    clients: Atom,
    desktop: Atom,
    window_desktop: Atom,
    state: Atom,
    fullscreen: Atom,
    maximized_horiz: Atom,
    maximized_vert: Atom,
    hidden: Atom,
    kind: Atom,
    normal: Atom,
    dialog: Atom,
    dock: Atom,
    opacity: Atom,
    frame: Atom,
}

impl Session {
    fn connect() -> Result<Self> {
        let display = parse_display(None)?;
        let deadline = Instant::now() + PROBE_BUDGET;
        let path = display
            .connect_instruction()
            .find_map(|address| match address {
                ConnectAddress::Socket(path) => Some(path),
                _ => None,
            })
            .context("自动隐藏只支持本地 X11 socket")?;
        let socket = local_socket(&path, true, deadline)
            .or_else(|_| local_socket(&path, false, deadline))?;
        let (stream, (family, address)) = DefaultStream::from_unix_stream(socket)?;
        let (name, data) = match get_auth(family, &address, display.display) {
            Ok(auth) => auth.unwrap_or_default(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => (Vec::new(), Vec::new()),
            Err(error) => return Err(error).context("无法读取 X11 授权信息"),
        };
        let connection = RustConnection::connect_to_stream_with_auth_info(
            TimedStream {
                inner: stream,
                deadline: Cell::new(deadline),
            },
            display.screen.into(),
            name,
            data,
        )?;
        let root = connection.setup().roots[usize::from(display.screen)].root;
        let atom = |name: &[u8]| -> Result<Atom> {
            Ok(connection.intern_atom(false, name)?.reply()?.atom)
        };
        let atoms = Atoms {
            clients: atom(b"_NET_CLIENT_LIST_STACKING")?,
            desktop: atom(b"_NET_CURRENT_DESKTOP")?,
            window_desktop: atom(b"_NET_WM_DESKTOP")?,
            state: atom(b"_NET_WM_STATE")?,
            fullscreen: atom(b"_NET_WM_STATE_FULLSCREEN")?,
            maximized_horiz: atom(b"_NET_WM_STATE_MAXIMIZED_HORZ")?,
            maximized_vert: atom(b"_NET_WM_STATE_MAXIMIZED_VERT")?,
            hidden: atom(b"_NET_WM_STATE_HIDDEN")?,
            kind: atom(b"_NET_WM_WINDOW_TYPE")?,
            normal: atom(b"_NET_WM_WINDOW_TYPE_NORMAL")?,
            dialog: atom(b"_NET_WM_WINDOW_TYPE_DIALOG")?,
            dock: atom(b"_NET_WM_WINDOW_TYPE_DOCK")?,
            opacity: atom(b"_NET_WM_WINDOW_OPACITY")?,
            frame: atom(b"_NET_FRAME_EXTENTS")?,
        };
        Ok(Self {
            connection,
            root,
            atoms,
        })
    }

    fn property(&self, window: u32, atom: Atom, kind: AtomEnum) -> Result<Option<Vec<u32>>> {
        let reply = self
            .connection
            .get_property(false, window, atom, kind, 0, 4096)?
            .reply()?;
        if reply.type_ == u32::from(AtomEnum::NONE) {
            return Ok(None);
        }
        anyhow::ensure!(reply.bytes_after == 0, "X11 窗口属性超过检测预算");
        let values = reply.value32().context("X11 窗口属性格式无效")?;
        Ok(Some(values.collect()))
    }

    fn describe(&self, id: u32, target: Target, desktop: Option<u32>) -> Result<Option<Window>> {
        let a = &self.atoms;
        if u64::from(id) == target.window_id
            || self
                .connection
                .get_window_attributes(id)?
                .reply()?
                .map_state
                != MapState::VIEWABLE
        {
            return Ok(None);
        }
        let assigned = self
            .property(id, a.window_desktop, AtomEnum::CARDINAL)?
            .and_then(|values| values.into_iter().next());
        if assigned.is_some_and(|value| {
            value != u32::MAX && desktop.is_some_and(|desktop| value != desktop)
        }) {
            return Ok(None);
        }
        let states = self
            .property(id, a.state, AtomEnum::ATOM)?
            .unwrap_or_default();
        if states.contains(&a.hidden) {
            return Ok(None);
        }
        let kinds = self
            .property(id, a.kind, AtomEnum::ATOM)?
            .unwrap_or_default();
        let dock = kinds.contains(&a.dock);
        if !kinds.is_empty() && !kinds.contains(&a.normal) && !kinds.contains(&a.dialog) && !dock {
            return Ok(None);
        }
        let geometry = self.connection.get_geometry(id)?.reply()?;
        let origin = self
            .connection
            .translate_coordinates(id, self.root, 0, 0)?
            .reply()?;
        let client = Rect::new(
            origin.dst_x.into(),
            origin.dst_y.into(),
            geometry.width.into(),
            geometry.height.into(),
        );
        let extents = self
            .property(id, a.frame, AtomEnum::CARDINAL)?
            .unwrap_or_default();
        let mut bounds = client;
        if let [left, right, top, bottom] = extents.as_slice() {
            bounds.left -= i64::from(*left);
            bounds.right += i64::from(*right);
            bounds.top -= i64::from(*top);
            bounds.bottom += i64::from(*bottom);
        }
        if bounds.intersection(target.screen).is_none() {
            return Ok(None);
        }
        let opaque = self
            .property(id, a.opacity, AtomEnum::CARDINAL)?
            .and_then(|values| values.into_iter().next())
            .is_none_or(|alpha| alpha == u32::MAX);
        let maximized = states.contains(&a.maximized_horiz) || states.contains(&a.maximized_vert);
        let fullscreen = !dock
            && opaque
            && client.covers(target.screen)
            && (states.contains(&a.fullscreen)
                || (!maximized && extents.iter().all(|value| *value == 0)));
        Ok(Some(Window {
            bounds,
            fullscreen,
            opaque,
        }))
    }

    fn fullscreen(&self, target: Target) -> Result<bool> {
        self.connection
            .stream()
            .deadline
            .set(Instant::now() + PROBE_BUDGET);
        let clients = self
            .property(self.root, self.atoms.clients, AtomEnum::WINDOW)?
            .context("窗口管理器未提供 EWMH 窗口叠放列表")?;
        let desktop = self
            .property(self.root, self.atoms.desktop, AtomEnum::CARDINAL)?
            .and_then(|values| values.into_iter().next());
        let mut windows = Vec::new();
        for id in clients.into_iter().rev() {
            match self.describe(id, target, desktop) {
                Ok(Some(window)) => windows.push(window),
                Ok(None) => {}
                // 窗口在两次请求之间关闭是正常竞争；连接和权限错误必须向上传播。
                Err(error) if error.downcast_ref::<ReplyError>().is_some_and(|error| matches!(error, ReplyError::X11Error(error) if error.error_kind == ErrorKind::Window || error.error_kind == ErrorKind::Drawable)) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(visible_fullscreen(target.visibility_area(), &windows))
    }
}

#[derive(Default)]
pub(crate) struct Probe {
    session: Option<Session>,
}

impl Probe {
    pub fn fullscreen(&mut self, target: Target) -> Result<bool> {
        if self.session.is_none() {
            self.session = Some(Session::connect()?);
        }
        let result = self
            .session
            .as_ref()
            .context("X11 连接不存在")?
            .fullscreen(target);
        if result.is_err() {
            self.session = None;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn unresponsive_server_has_a_bounded_wait() {
        let (reader, _writer) = UnixStream::pair().unwrap();
        let error = poll_fd(
            reader.as_raw_fd(),
            libc::POLLIN,
            Instant::now() + Duration::from_millis(20),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn probe_deadline_also_applies_when_server_already_has_data() {
        let (reader, mut writer) = UnixStream::pair().unwrap();
        writer.write_all(b"x").unwrap();
        let (inner, _) = DefaultStream::from_unix_stream(reader).unwrap();
        let stream = TimedStream {
            inner,
            deadline: Cell::new(Instant::now() + PROBE_BUDGET),
        };
        stream.poll(PollMode::Readable).unwrap();
        let mut fds = Vec::new();
        let mut bytes = [0u8];
        assert_eq!(stream.read(&mut bytes, &mut fds).unwrap(), 1);
        assert_eq!(bytes, [b'x']);
        stream.deadline.set(Instant::now());
        assert_eq!(
            stream.read(&mut bytes, &mut fds).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
    }
}
