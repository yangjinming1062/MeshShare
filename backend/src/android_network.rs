use tauri::{plugin::Builder, Wry};

/// Register the Android network adapter before the Rust node opens any
/// listeners. The native plugin binds future sockets to a physical LAN when a
/// full-tunnel VPN is the process default network, and keeps that decision
/// current while the app remains open.
pub(crate) fn plugin() -> tauri::plugin::TauriPlugin<Wry> {
    Builder::new("android-network")
        .setup(|_app, api| {
            // 插件没有 Rust 侧命令，实例由 PluginManager 持有；不托管句柄，
            // 避免占用共享状态类型让其他插件拿不到自己的句柄。
            api.register_android_plugin("com.meshshare.android.network", "NetworkPlugin")?;
            Ok(())
        })
        .build()
}
