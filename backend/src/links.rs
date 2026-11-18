//! 系统打开入口：项目主页交给默认浏览器，传输历史定位交给文件管理器。

use anyhow::{Context, Result};
use std::path::{Component, Path, PathBuf};

/// 界面上的 GitHub 图标打开这个地址；换仓库只改这里。
const PROJECT_HOME: &str = "https://github.com/yangjinming1062/MeshShare";

#[tauri::command]
pub(crate) fn open_project_page() -> std::result::Result<(), String> {
    open_in_browser(PROJECT_HOME).map_err(|error| error.to_string())
}

/// 打开传输历史条目的所在目录；select 指向条目本身时，平台支持就把条目选中。
#[tauri::command]
pub(crate) async fn open_transfer_location(
    directory: String,
    select: Option<String>,
) -> std::result::Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let directory = PathBuf::from(directory);
        let select = resolve_open_location(&directory, select.as_deref().map(Path::new))?;
        open_directory(&directory, select.as_deref())
    })
    .await
    .map_err(|error| format!("无法打开所在目录：{error}"))?
    .map_err(|error: anyhow::Error| format!("{error:#}"))
}

/// 校验目标目录并解析选中目标：select 缺失或不存在时只打开目录，越出目录范围一律拒绝。
fn resolve_open_location(directory: &Path, select: Option<&Path>) -> Result<Option<PathBuf>> {
    anyhow::ensure!(directory.is_absolute(), "定位目录必须是绝对路径");
    anyhow::ensure!(directory.is_dir(), "目录不存在或已被移动");
    let Some(select) = select.filter(|path| !path.as_os_str().is_empty()) else {
        return Ok(None);
    };
    let target = if select.is_absolute() {
        select.to_path_buf()
    } else {
        directory.join(select)
    };
    anyhow::ensure!(
        target.as_path() != directory
            && target.starts_with(directory)
            && !target.components().any(|part| part == Component::ParentDir),
        "定位目标不在该目录内"
    );
    Ok(target.exists().then_some(target))
}

#[cfg(target_os = "windows")]
fn open_directory(directory: &Path, select: Option<&Path>) -> Result<()> {
    use std::{os::windows::ffi::OsStrExt, ptr};
    use windows_result::HRESULT;
    use windows_sys::Win32::{
        System::Com::{CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_APARTMENTTHREADED},
        UI::Shell::{SHOpenFolderAndSelectItems, SHParseDisplayName},
    };

    let Some(select) = select else {
        return shell_open(directory.as_os_str());
    };
    let target: Vec<u16> = select.as_os_str().encode_wide().chain([0]).collect();
    // 直接传入 Shell 项目标识，路径中的逗号不参与 Explorer 命令行解析。
    unsafe {
        HRESULT(CoInitializeEx(ptr::null(), COINIT_APARTMENTTHREADED as u32))
            .ok()
            .context("无法初始化文件管理器")?;
        let mut item = ptr::null_mut();
        let parsed = HRESULT(SHParseDisplayName(
            target.as_ptr(),
            ptr::null_mut(),
            &mut item,
            0,
            ptr::null_mut(),
        ));
        let result = parsed.ok().context("无法解析定位目标").and_then(|()| {
            HRESULT(SHOpenFolderAndSelectItems(item, 0, ptr::null(), 0))
                .ok()
                .context("无法在文件管理器中选中目标")
        });
        CoTaskMemFree(item.cast());
        CoUninitialize();
        result
    }
}

#[cfg(target_os = "macos")]
fn open_directory(directory: &Path, select: Option<&Path>) -> Result<()> {
    let mut command = std::process::Command::new("/usr/bin/open");
    if let Some(select) = select {
        command.arg("-R").arg(select);
    } else {
        command.arg(directory);
    }
    let output = command.output().context("无法启动访达")?;
    anyhow::ensure!(
        output.status.success(),
        "无法打开所在目录（{}）{}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}

#[cfg(target_os = "linux")]
fn open_directory(directory: &Path, _select: Option<&Path>) -> Result<()> {
    use gtk::gio::{self, prelude::FileExt};

    let directory = gio::File::for_path(directory);
    gio::AppInfo::launch_default_for_uri(&directory.uri(), None::<&gio::AppLaunchContext>)
        .context("无法在文件管理器中打开所在目录")
}

#[cfg(target_os = "windows")]
fn shell_open(target: &std::ffi::OsStr) -> Result<()> {
    use std::{os::windows::ffi::OsStrExt, ptr};
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    let verb: Vec<u16> = "open\0".encode_utf16().collect();
    let target: Vec<u16> = target.encode_wide().chain([0]).collect();
    let status = unsafe {
        ShellExecuteW(
            ptr::null_mut(),
            verb.as_ptr(),
            target.as_ptr(),
            ptr::null(),
            ptr::null(),
            SW_SHOWNORMAL,
        )
    } as isize;
    anyhow::ensure!(status > 32, "系统无法打开指定位置（错误 {status}）");
    Ok(())
}

#[cfg(target_os = "windows")]
fn open_in_browser(url: &str) -> Result<()> {
    shell_open(std::ffi::OsStr::new(url)).with_context(|| format!("无法用系统浏览器打开 {url}"))
}

/// 转发命令很快退出，单独回收，避免界面长期运行留下僵尸进程。
#[cfg(not(target_os = "windows"))]
fn open_in_browser(url: &str) -> Result<()> {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let mut child = std::process::Command::new(program)
        .arg(url)
        .spawn()
        .with_context(|| format!("无法用系统浏览器打开 {url}"))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("meshshare-links-{tag}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create temp directory");
        dir
    }

    #[test]
    fn resolve_selects_item_inside_directory() {
        let dir = temp_dir("inside");
        let item = dir.join("报告.docx");
        std::fs::write(&item, b"x").expect("write item");
        assert_eq!(
            resolve_open_location(&dir, Some(Path::new("报告.docx"))).expect("resolve"),
            Some(item.clone())
        );
        assert_eq!(
            resolve_open_location(&dir, Some(&item)).expect("resolve absolute"),
            Some(item)
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn resolve_falls_back_to_directory_without_select() {
        let dir = temp_dir("fallback");
        let item = dir.join("gone.bin");
        assert_eq!(
            resolve_open_location(&dir, Some(&item)).expect("missing select"),
            None
        );
        assert_eq!(resolve_open_location(&dir, None).expect("no select"), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn resolve_rejects_missing_or_plain_file_directory() {
        let dir = temp_dir("bad-dir");
        let missing = dir.join("missing");
        assert!(resolve_open_location(&missing, None).is_err());
        let plain = dir.join("plain.txt");
        std::fs::write(&plain, b"x").expect("write file");
        assert!(resolve_open_location(&plain, None).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn resolve_rejects_select_outside_directory() {
        let root = temp_dir("outside");
        let dir = root.join("destination");
        std::fs::create_dir(&dir).expect("create destination");
        let sibling = root.join("outside-item");
        std::fs::write(&sibling, b"x").expect("write sibling");
        assert!(resolve_open_location(&dir, Some(Path::new("../outside-item"))).is_err());
        assert!(resolve_open_location(&dir, Some(&sibling)).is_err());
        let _ = std::fs::remove_dir_all(root);
    }
}
