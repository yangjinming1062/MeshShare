use tauri::{plugin::Builder, Manager, Wry};

/// Register the Android network adapter before the Rust node opens any
/// listeners. The native plugin binds future sockets to a physical LAN when a
/// full-tunnel VPN is the process default network, and keeps that decision
/// current while the app remains open.
pub(crate) fn plugin() -> tauri::plugin::TauriPlugin<Wry> {
    Builder::new("android-network")
        .setup(|app, api| {
            app.manage(
                api.register_android_plugin("com.meshshare.android.network", "NetworkPlugin")?,
            );
            Ok(())
        })
        .build()
}
