//! 项目主页入口：把链接触发给系统默认浏览器。

use anyhow::Result;

/// 界面上的 GitHub 图标打开这个地址；换仓库只改这里。
const PROJECT_HOME: &str = "https://github.com/yangjinming1062/MeshShare";

#[tauri::command]
pub(crate) fn open_project_page() -> std::result::Result<(), String> {
    open_in_browser(PROJECT_HOME).map_err(|error| error.to_string())
}

#[cfg(target_os = "windows")]
fn open_in_browser(url: &str) -> Result<()> {
    use std::ptr;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    // ShellExecuteW 只接受 UTF-16 宽字符串；操作动词 "open" 表示交给默认程序。
    let verb: Vec<u16> = "open\0".encode_utf16().collect();
    let target: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
    // 不经过 shell，URL 不参与命令行解析；返回值大于 32 表示已交给系统。
    let status = unsafe {
        ShellExecuteW(
            ptr::null_mut(),
            verb.as_ptr(),
            target.as_ptr(),
            ptr::null(),
            ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    anyhow::ensure!(status as isize > 32, "无法用系统浏览器打开 {url}");
    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn open_in_browser(url: &str) -> Result<()> {
    use std::process::Command;

    // 两个平台都用系统自带的转发命令，参数直接传递，不经过 shell。
    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let mut child = Command::new(program)
        .arg(url)
        .spawn()
        .map_err(|error| anyhow::anyhow!("无法用系统浏览器打开 {url}：{error}"))?;
    // 转发命令很快退出，单独回收，避免界面长期运行留下僵尸进程。
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}
