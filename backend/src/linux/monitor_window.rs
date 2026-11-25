use crate::desktop::monitor_window::{
    clamp_position_to_work_area, point_is_in_work_area, MONITOR_HEIGHT, MONITOR_WIDTH,
};
use crate::model::{GpuMemoryKind, HardwareMetrics, MonitorPosition};
use crate::{
    apply_monitor_locked, apply_monitor_visible, current_monitor_capabilities, NodeRuntime,
};
use gtk::{gdk, glib::Propagation, prelude::*, Orientation};
use std::{cell::RefCell, sync::Arc};
use tauri::{AppHandle, Manager, PhysicalPosition, PhysicalRect, PhysicalSize};

const MONITOR_CSS: &[u8] = br#"
window { background-color: rgba(13, 21, 38, 0.78); border-radius: 10px; border: 1px solid #53637a; }
label { color: #ffffff; font: 10px monospace; }
"#;

struct MonitorWindow {
    window: gtk::Window,
    values: [gtk::Label; 6],
}

thread_local! {
    static MONITOR: RefCell<Option<MonitorWindow>> = const { RefCell::new(None) };
}

pub(crate) fn get(_: &AppHandle) -> Option<gtk::Window> {
    MONITOR.with_borrow(|monitor| monitor.as_ref().map(|monitor| monitor.window.clone()))
}

pub(crate) fn ensure_window(app: &AppHandle) {
    if get(app).is_some() || !app.state::<Arc<NodeRuntime>>().monitor_visible() {
        return;
    }
    let window = gtk::Window::new(gtk::WindowType::Toplevel);
    window.set_title("MeshShare · 硬件监控");
    window.set_default_size(MONITOR_WIDTH, MONITOR_HEIGHT);
    window.set_decorated(false);
    window.set_resizable(false);
    window.set_accept_focus(false);
    window.set_focus_on_map(false);
    window.set_skip_taskbar_hint(true);
    window.set_keep_above(current_monitor_capabilities(app).can_keep_above);
    if let Some(visual) = WidgetExt::screen(&window).and_then(|screen| screen.rgba_visual()) {
        window.set_visual(Some(&visual));
    }
    window.add_events(gdk::EventMask::BUTTON_PRESS_MASK);

    let style = gtk::CssProvider::new();
    if let Err(error) = style.load_from_data(MONITOR_CSS) {
        eprintln!("加载悬浮条样式失败：{error}");
    }
    window
        .style_context()
        .add_provider(&style, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
    let grid = gtk::Grid::builder()
        .margin_start(10)
        .margin_end(10)
        .margin_top(6)
        .margin_bottom(6)
        .column_spacing(5)
        .row_spacing(2)
        .column_homogeneous(true)
        .build();
    let names = ["CPU", "GPU", "↑", "内存", "显存", "↓"];
    let values = std::array::from_fn(|index| {
        let cell = gtk::Box::new(Orientation::Horizontal, 3);
        let name = gtk::Label::new(Some(names[index]));
        let value = gtk::Label::new(Some("—"));
        for label in [&name, &value] {
            label
                .style_context()
                .add_provider(&style, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
            cell.add(label);
        }
        grid.attach(&cell, (index % 3) as i32, (index / 3) as i32, 1, 1);
        value
    });
    window.add(&grid);
    install_events(&window, app);
    window.realize();
    if current_monitor_capabilities(app).can_persist_position {
        position_window(&window, &app.state::<Arc<NodeRuntime>>());
    }
    MONITOR.with_borrow_mut(|monitor| *monitor = Some(MonitorWindow { window, values }));
}

fn install_events(window: &gtk::Window, app: &AppHandle) {
    let menu = gtk::Menu::new();
    let lock = gtk::MenuItem::with_label("锁定悬浮条（忽略鼠标）");
    lock.set_sensitive(current_monitor_capabilities(app).can_click_through);
    let handle = app.clone();
    lock.connect_activate(move |_| {
        let node = handle.state::<Arc<NodeRuntime>>();
        if let Err(error) = apply_monitor_locked(&handle, &node, true) {
            eprintln!("{error}");
        }
    });
    let hide = gtk::MenuItem::with_label("隐藏悬浮条");
    let handle = app.clone();
    hide.connect_activate(move |_| hide_monitor(&handle));
    menu.append(&lock);
    menu.append(&hide);
    menu.show_all();
    window.connect_button_press_event(move |window, event| {
        match event.button() {
            1 => {
                let (x, y) = event.root();
                window.begin_move_drag(1, x as i32, y as i32, event.time());
            }
            3 => menu.popup_at_pointer(Some(event)),
            _ => return Propagation::Proceed,
        }
        Propagation::Stop
    });
    let handle = app.clone();
    window.connect_delete_event(move |_, _| {
        hide_monitor(&handle);
        Propagation::Stop
    });
}

fn hide_monitor(app: &AppHandle) {
    if let Err(error) = apply_monitor_visible(app, &app.state::<Arc<NodeRuntime>>(), false) {
        eprintln!("{error}");
    }
}

pub(crate) fn set_click_through(window: &gtk::Window, locked: bool) {
    let empty_region = locked.then(gtk::cairo::Region::create);
    window.input_shape_combine_region(empty_region.as_ref());
}

pub(crate) fn release_window(window: &gtk::Window, runtime: &NodeRuntime) {
    save_position(window, runtime);
    MONITOR.with_borrow_mut(|monitor| *monitor = None);
    // 窗口及其所有 GTK 引用只在主线程使用。
    unsafe { window.destroy() };
}

pub(crate) fn refresh(window: &gtk::Window, runtime: &NodeRuntime) {
    save_position(window, runtime);
    let metrics = runtime.local_metrics().filter(|metrics| {
        crate::clock::now_millis().saturating_sub(metrics.sampled_at_ms) <= 5_000
    });
    let values = display_values(metrics.as_ref());
    MONITOR.with_borrow(|monitor| {
        if let Some(monitor) = monitor.as_ref() {
            for (label, value) in monitor.values.iter().zip(values) {
                label.set_text(&value);
            }
        }
    });
}

fn display_values(metrics: Option<&HardwareMetrics>) -> [String; 6] {
    fn percent(value: Option<f64>) -> String {
        value.map_or_else(|| "—".into(), |value| format!("{}%", value.round()))
    }
    fn ratio(used: Option<u64>, total: Option<u64>) -> Option<f64> {
        Some(used? as f64 * 100.0 / total.filter(|total| *total > 0)? as f64)
    }
    fn rate(value: Option<u64>) -> String {
        match value {
            Some(value) if value >= 1024 * 1024 => {
                format!("{:.1} MB/s", value as f64 / 1_048_576.0)
            }
            Some(value) if value >= 1024 => format!("{:.1} KB/s", value as f64 / 1024.0),
            Some(value) => format!("{value} B/s"),
            None => "—".into(),
        }
    }
    let Some(metrics) = metrics else {
        return std::array::from_fn(|_| "—".into());
    };
    let gpu = metrics
        .gpus
        .iter()
        .filter_map(|gpu| gpu.utilization_percent.map(f64::from))
        .reduce(f64::max);
    let video_memory = metrics
        .gpus
        .iter()
        .filter(|gpu| gpu.memory_kind == GpuMemoryKind::Dedicated)
        .filter_map(|gpu| ratio(gpu.memory_used_bytes, gpu.memory_total_bytes))
        .reduce(f64::max);
    [
        percent(metrics.cpu_usage_percent.map(f64::from)),
        percent(gpu),
        rate(metrics.network_up_bytes_per_sec),
        percent(ratio(metrics.memory_used_bytes, metrics.memory_total_bytes)),
        percent(video_memory),
        rate(metrics.network_down_bytes_per_sec),
    ]
}

fn work_area(monitor: &gdk::Monitor) -> PhysicalRect<i32, u32> {
    let rect = monitor.workarea();
    let scale = monitor.scale_factor();
    PhysicalRect {
        position: PhysicalPosition::new(rect.x() * scale, rect.y() * scale),
        size: PhysicalSize::new(
            (rect.width() * scale) as u32,
            (rect.height() * scale) as u32,
        ),
    }
}

fn position_window(window: &gtk::Window, runtime: &NodeRuntime) {
    let display = window.display();
    let saved = runtime.monitor_position();
    let target = (0..display.n_monitors())
        .filter_map(|index| display.monitor(index))
        .find(|monitor| {
            saved
                .as_ref()
                .is_some_and(|point| point_is_in_work_area(point, &work_area(monitor)))
        });
    let position = target.as_ref().and(saved);
    let Some(target) = target
        .or_else(|| display.primary_monitor())
        .or_else(|| display.monitor(0))
    else {
        return;
    };
    let scale = target.scale_factor();
    let size = PhysicalSize::new(
        (MONITOR_WIDTH * scale) as u32,
        (MONITOR_HEIGHT * scale) as u32,
    );
    let position = clamp_position_to_work_area(position, &work_area(&target), size);
    window.move_(position.x / scale, position.y / scale);
}

fn save_position(window: &gtk::Window, runtime: &NodeRuntime) {
    if !window.display().backend().is_x11() || !window.is_mapped() {
        return;
    }
    let (x, y) = window.position();
    let scale = window.scale_factor();
    let position = MonitorPosition {
        x: x * scale,
        y: y * scale,
    };
    if runtime.monitor_position().as_ref() != Some(&position) {
        if let Err(error) = runtime.set_monitor_position(position) {
            eprintln!("保存悬浮条位置失败：{error:#}");
        }
    }
}
