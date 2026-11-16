use anyhow::{Context, Result};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::path::Path;
use std::path::PathBuf;

#[cfg(target_os = "linux")]
use crate::linux::autostart as platform;
#[cfg(target_os = "macos")]
use crate::macos::autostart as platform;
#[cfg(target_os = "windows")]
use crate::windows::autostart as platform;

pub const AUTOSTART_ARGUMENT: &str = "--meshshare-autostart";

#[cfg(any(target_os = "linux", target_os = "windows"))]
pub(crate) const APP_NAME: &str = "MeshShare";

#[derive(Debug, Clone)]
pub struct Registration {
    executable: PathBuf,
}

impl Registration {
    pub fn current() -> Result<Self> {
        Ok(Self {
            executable: std::env::current_exe().context("无法获取当前程序路径")?,
        })
    }

    pub fn enable(&self) -> Result<()> {
        platform::enable(&self.executable)
    }

    pub fn disable(&self) -> Result<()> {
        platform::disable()
    }

    /// 按目标状态注册或移除登录项，调用方负责失败时回滚配置。
    pub fn set_enabled(&self, enabled: bool) -> Result<()> {
        if enabled {
            self.enable()
        } else {
            self.disable()
        }
    }

    pub fn is_enabled(&self) -> Result<bool> {
        platform::is_enabled()
    }
}

pub fn launched_from_args<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    args.into_iter()
        .any(|arg| arg.as_ref() == AUTOSTART_ARGUMENT)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod platform {
    use anyhow::Result;
    use std::path::Path;

    pub fn enable(_: &Path) -> Result<()> {
        anyhow::bail!("当前平台不支持开机自启动")
    }

    pub fn disable() -> Result<()> {
        anyhow::bail!("当前平台不支持开机自启动")
    }

    pub fn is_enabled() -> Result<bool> {
        anyhow::bail!("当前平台不支持开机自启动")
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn atomic_write(path: &Path, contents: &str) -> Result<()> {
    let parent = path.parent().context("自启动配置路径没有父目录")?;
    std::fs::create_dir_all(parent)?;
    crate::fsutil::atomic_write(path, contents.as_bytes())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn remove_optional(path: PathBuf) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn home_directory() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("无法确定当前用户主目录")
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn shell_single_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

/// Linux 与 macOS 共用的启动脚本：可执行文件已被移动或删除时自清理登录项。
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn wrapper_script(executable: &Path, autostart_file: &Path) -> String {
    format!(
        "#!/bin/sh\nset -eu\nexecutable={executable}\nautostart_file={autostart_file}\nif [ -x \"$executable\" ]; then\n  exec \"$executable\" {argument}\nfi\nrm -f \"$autostart_file\" \"$0\"\n",
        executable = shell_single_quote(executable),
        autostart_file = shell_single_quote(autostart_file),
        argument = AUTOSTART_ARGUMENT,
    )
}

#[cfg(test)]
mod tests {
    use super::launched_from_args;

    #[test]
    fn startup_argument_requires_the_exact_flag() {
        assert!(launched_from_args(["meshshare", "--meshshare-autostart"]));
        assert!(!launched_from_args(["meshshare", "--autostart"]));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn shell_script_paths_escape_single_quotes() {
        use super::shell_single_quote;
        use std::path::Path;

        assert_eq!(
            shell_single_quote(Path::new("/tmp/Mesh Share/o'h")),
            "'/tmp/Mesh Share/o'\\''h'"
        );
    }
}
