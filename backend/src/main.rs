#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if std::env::args().any(|arg| arg == "--meshshare-cleanup-autostart") {
        meshshare_lib::cleanup_autostart();
        return;
    }
    #[cfg(target_os = "linux")]
    configure_linux_webkit();
    meshshare_lib::run();
}

#[cfg(target_os = "linux")]
fn configure_linux_webkit() {
    const COMPOSITING: &str = "WEBKIT_DISABLE_COMPOSITING_MODE";
    let has_render_device = std::fs::read_dir("/dev/dri").is_ok_and(|entries| {
        entries
            .flatten()
            .any(|entry| entry.file_name().as_encoded_bytes().starts_with(b"renderD"))
    });
    if !has_render_device && std::env::var_os(COMPOSITING).is_none() {
        // 在 GTK、WebKit 和工作线程启动前关闭软件合成，避免为窗口常驻额外图形缓冲。
        std::env::set_var(COMPOSITING, "1");
    }
}
