use crate::runtime::NodeRuntime;
use anyhow::Context;
use std::sync::{atomic::Ordering, Arc};
use std::time::Duration;
use tauri::{
    ipc::Channel,
    plugin::{Builder, PluginHandle},
    AppHandle, Manager, Wry,
};

struct NetworkPlugin(PluginHandle<Wry>);

/// 绑定物理网络必须早于 Rust 监听器和 mDNS 守护进程。
pub(crate) fn plugin() -> tauri::plugin::TauriPlugin<Wry> {
    Builder::new("android-network")
        .setup(|app, api| {
            let handle =
                api.register_android_plugin("com.meshshare.android.network", "NetworkPlugin")?;
            app.manage(NetworkPlugin(handle));
            Ok(())
        })
        .build()
}

pub(crate) fn observe(app: &AppHandle, runtime: Arc<NodeRuntime>) {
    runtime.request_network_rebind();
    let node = Arc::downgrade(&runtime);
    let channel = Channel::<()>::new(move |_| {
        if let Some(node) = node.upgrade() {
            if !node.stopping.load(Ordering::SeqCst) {
                node.request_network_rebind();
            }
        }
        Ok(())
    });
    let plugin = app.state::<NetworkPlugin>().0.clone();
    tokio::spawn(async move {
        for delay in [0, 1, 2] {
            if delay > 0 {
                tokio::time::sleep(Duration::from_secs(delay)).await;
            }
            if runtime.stopping.load(Ordering::SeqCst) {
                return;
            }
            let subscribe = plugin.run_mobile_plugin_async::<()>(
                "observe",
                serde_json::json!({ "channel": &channel }),
            );
            match tokio::time::timeout(Duration::from_secs(5), subscribe)
                .await
                .context("等待 Android 网络插件超时")
                .and_then(|result| result.context("登记 Android 网络监听失败"))
            {
                Ok(()) => return,
                Err(error) => eprintln!("监听 Android 网络绑定变化失败：{error:#}"),
            }
        }
    });
}
