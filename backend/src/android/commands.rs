//! Android 专属命令：接收目录解析与「所有文件访问权限」授权入口。

use super::storage::AndroidStoragePluginHandle;
use serde::Serialize;
use std::path::PathBuf;
use tauri::{AppHandle, Manager};

const ANDROID_PUBLIC_DOWNLOAD_DIR: &str = "/storage/emulated/0/Download";

/// 公共下载目录只有在授予「所有文件访问权限」后才可写，用一次真实写入判断。
fn public_download_directory() -> Option<PathBuf> {
    let directory = PathBuf::from(ANDROID_PUBLIC_DOWNLOAD_DIR).join("MeshShare");
    std::fs::create_dir_all(&directory).ok()?;
    let probe = directory.join(".meshshare-write-probe");
    match std::fs::write(&probe, []) {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            Some(directory)
        }
        Err(_) => None,
    }
}

fn private_download_directory(app: &AppHandle) -> Result<PathBuf, String> {
    let directory = app
        .path()
        .download_dir()
        .map_err(|error| format!("无法取得 Android 应用存储目录：{error}"))?
        .join("MeshShare");
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("无法创建 Android 接收目录：{error}"))?;
    Ok(directory)
}

fn receive_target(app: &AppHandle) -> Result<(PathBuf, bool), String> {
    if let Some(directory) = public_download_directory() {
        return Ok((directory, true));
    }
    Ok((private_download_directory(app)?, false))
}

#[derive(Serialize)]
pub(crate) struct AndroidReceiveStorage {
    directory: String,
    public_download: bool,
}

#[tauri::command]
pub(crate) fn android_receive_storage(app: AppHandle) -> Result<AndroidReceiveStorage, String> {
    let (directory, public_download) = receive_target(&app)?;
    Ok(AndroidReceiveStorage {
        directory: directory.to_string_lossy().into_owned(),
        public_download,
    })
}

#[tauri::command]
pub(crate) fn android_request_receive_storage(app: AppHandle) -> Result<(), String> {
    app.state::<AndroidStoragePluginHandle>()
        .0
        .run_mobile_plugin::<()>("requestAllFilesAccess", ())
        .map_err(|error| format!("无法打开存储授权页面：{error}"))
}

#[tauri::command]
pub(crate) fn android_receive_directory(app: AppHandle) -> Result<String, String> {
    let (directory, _) = receive_target(&app)?;
    Ok(directory.to_string_lossy().into_owned())
}

/// 在系统文件管理器里打开接收目录；只接受目录，避免把任意路径交给 Intent。
#[tauri::command]
pub(crate) fn android_reveal_path(app: AppHandle, path: String) -> Result<(), String> {
    if !PathBuf::from(&path).is_dir() {
        return Err("目录不存在或已被移动".to_string());
    }
    app.state::<AndroidStoragePluginHandle>()
        .0
        .run_mobile_plugin::<()>("revealPath", serde_json::json!({ "path": path }))
        .map_err(|error| format!("无法打开所在目录：{error}"))
}
