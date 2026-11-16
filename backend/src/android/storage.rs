//! Android 接收目录的「所有文件访问权限」桥接。

use tauri::plugin::{Builder, PluginHandle, TauriPlugin};
use tauri::{Manager, Wry};

/// 插件句柄按插件分别包装托管：`App::manage` 对同一类型只保留第一个状态。
pub(crate) struct AndroidStoragePluginHandle(pub(crate) PluginHandle<Wry>);

pub(crate) fn plugin() -> TauriPlugin<Wry> {
    Builder::new("android-storage")
        .setup(|app, api| {
            let handle =
                api.register_android_plugin("com.meshshare.android.storage", "StoragePlugin")?;
            if !app.manage(AndroidStoragePluginHandle(handle)) {
                return Err("Android 存储插件句柄已存在".into());
            }
            Ok(())
        })
        .build()
}
