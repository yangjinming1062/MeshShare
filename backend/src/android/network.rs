use tauri::{plugin::Builder, Wry};

/// 在节点打开任何监听之前注册 Android 网络适配器。全隧道 VPN 成为进程默认网络时，
/// 原生插件把后续 socket 绑定到局域网物理网卡，并在 App 运行期间持续更新这一选择。
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
