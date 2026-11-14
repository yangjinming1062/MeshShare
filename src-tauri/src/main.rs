#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if std::env::args().any(|arg| arg == "--meshshare-cleanup-autostart") {
        meshshare_lib::cleanup_autostart();
        return;
    }
    meshshare_lib::run();
}
