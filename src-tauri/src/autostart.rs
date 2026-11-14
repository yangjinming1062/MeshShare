use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub const AUTOSTART_ARGUMENT: &str = "--meshshare-autostart";

#[cfg(any(target_os = "linux", target_os = "windows"))]
const APP_NAME: &str = "MeshShare";

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

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn atomic_write(path: &Path, contents: &str) -> Result<()> {
    let parent = path.parent().context("自启动配置路径没有父目录")?;
    std::fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("自启动配置文件名无效")?;
    let temporary = parent.join(format!(".{file_name}.tmp-{}", std::process::id()));
    if let Err(error) =
        std::fs::write(&temporary, contents).and_then(|()| std::fs::rename(&temporary, path))
    {
        let _ = std::fs::remove_file(&temporary);
        return Err(error.into());
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn remove_optional(path: PathBuf) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn home_directory() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("无法确定当前用户主目录")
}

#[cfg(target_os = "linux")]
fn config_directory() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
    {
        return Ok(path);
    }
    Ok(home_directory()?.join(".config"))
}

#[cfg(target_os = "macos")]
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(target_os = "linux")]
fn desktop_exec_argument(path: &Path) -> String {
    let mut escaped = String::from("\"");
    for character in path.to_string_lossy().chars() {
        if matches!(character, '\\' | '"' | '`' | '$') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped.push('"');
    escaped
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn shell_single_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

#[cfg(target_os = "linux")]
mod platform {
    use super::{
        atomic_write, config_directory, desktop_exec_argument, shell_single_quote, APP_NAME,
    };
    use anyhow::Result;
    use std::path::{Path, PathBuf};

    fn desktop_file() -> Result<PathBuf> {
        Ok(config_directory()?
            .join("autostart")
            .join(format!("{APP_NAME}.desktop")))
    }

    fn wrapper_file() -> Result<PathBuf> {
        Ok(config_directory()?.join(APP_NAME).join("autostart.sh"))
    }

    pub fn enable(executable: &Path) -> Result<()> {
        let path = desktop_file()?;
        let wrapper = wrapper_file()?;
        let wrapper_contents = format!(
            "#!/bin/sh\nset -eu\nexecutable={executable}\nautostart_file={autostart_file}\nif [ -x \"$executable\" ]; then\n  exec \"$executable\" {argument}\nfi\nrm -f \"$autostart_file\" \"$0\"\n",
            executable = shell_single_quote(executable),
            autostart_file = shell_single_quote(&path),
            argument = super::AUTOSTART_ARGUMENT,
        );
        atomic_write(&wrapper, &wrapper_contents)?;
        let shell = desktop_exec_argument(Path::new("/bin/sh"));
        let wrapper = desktop_exec_argument(&wrapper);
        let contents = format!(
            "[Desktop Entry]\nType=Application\nVersion=1.0\nName={APP_NAME}\nComment={APP_NAME} startup\nExec={shell} {wrapper}\nStartupNotify=false\nTerminal=false\n",
        );
        atomic_write(&path, &contents)
    }

    pub fn disable() -> Result<()> {
        let path = desktop_file()?;
        let wrapper = wrapper_file()?;
        super::remove_optional(path)?;
        super::remove_optional(wrapper)
    }

    pub fn is_enabled() -> Result<bool> {
        Ok(desktop_file()?.exists())
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{atomic_write, home_directory, shell_single_quote, xml_escape};
    use anyhow::{Context, Result};
    use std::path::{Path, PathBuf};

    const BUNDLE_IDENTIFIER: &str = "com.meshshare.desktop";

    fn launch_agent_file() -> Result<PathBuf> {
        Ok(home_directory()?
            .join("Library")
            .join("LaunchAgents")
            .join(format!("{BUNDLE_IDENTIFIER}.plist")))
    }

    fn wrapper_file() -> Result<PathBuf> {
        Ok(home_directory()?
            .join("Library")
            .join("Application Support")
            .join("MeshShare")
            .join("autostart.sh"))
    }

    pub fn enable(executable: &Path) -> Result<()> {
        let path = launch_agent_file()?;
        let wrapper = wrapper_file()?;
        let wrapper_contents = format!(
            "#!/bin/sh\nset -eu\nexecutable={executable}\nautostart_file={autostart_file}\nif [ -x \"$executable\" ]; then\n  exec \"$executable\" {argument}\nfi\nrm -f \"$autostart_file\" \"$0\"\n",
            executable = shell_single_quote(executable),
            autostart_file = shell_single_quote(&path),
            argument = super::AUTOSTART_ARGUMENT,
        );
        atomic_write(&wrapper, &wrapper_contents)?;
        let executable = xml_escape(
            Path::new("/bin/sh")
                .to_str()
                .context("当前程序路径不是有效的 UTF-8")?,
        );
        let wrapper = xml_escape(wrapper.to_str().context("自启动脚本路径不是有效的 UTF-8")?);
        let contents = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>Label</key><string>{BUNDLE_IDENTIFIER}</string><key>AssociatedBundleIdentifiers</key><array><string>{BUNDLE_IDENTIFIER}</string></array><key>ProgramArguments</key><array><string>{executable}</string><string>{wrapper}</string></array><key>RunAtLoad</key><true/><key>LimitLoadToSessionType</key><string>Aqua</string></dict></plist>\n",
        );
        atomic_write(&path, &contents)
    }

    pub fn disable() -> Result<()> {
        let path = launch_agent_file()?;
        let wrapper = wrapper_file()?;
        super::remove_optional(path)?;
        super::remove_optional(wrapper)
    }

    pub fn is_enabled() -> Result<bool> {
        Ok(launch_agent_file()?.exists())
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use super::APP_NAME;
    use anyhow::{Context, Result};
    use std::path::Path;
    use windows_registry::CURRENT_USER;
    use windows_result::HRESULT;

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const ERROR_FILE_NOT_FOUND: HRESULT = HRESULT::from_win32(2);

    fn is_not_found(error: &windows_result::Error) -> bool {
        error.code() == ERROR_FILE_NOT_FOUND
    }

    fn command(executable: &Path) -> anyhow::Result<String> {
        let executable = executable
            .to_str()
            .context("当前程序路径不是有效的 UTF-8")?;
        Ok(format!("\"{executable}\" {}", super::AUTOSTART_ARGUMENT))
    }

    pub fn enable(executable: &Path) -> Result<()> {
        let key = CURRENT_USER
            .create(RUN_KEY)
            .map_err(|error| anyhow::anyhow!("打开 Windows 用户启动项失败：{error}"))?;
        key.set_string(APP_NAME, command(executable)?)
            .map_err(|error| anyhow::anyhow!("写入 Windows 用户启动项失败：{error}"))?;
        Ok(())
    }

    pub fn disable() -> Result<()> {
        let key = match CURRENT_USER.options().read().write().open(RUN_KEY) {
            Ok(key) => key,
            Err(error) if is_not_found(&error) => return Ok(()),
            Err(error) => {
                return Err(anyhow::anyhow!("打开 Windows 用户启动项失败：{error}"));
            }
        };
        match key.remove_value(APP_NAME) {
            Ok(()) => Ok(()),
            Err(error) if is_not_found(&error) => Ok(()),
            Err(error) => Err(anyhow::anyhow!("删除 Windows 用户启动项失败：{error}")),
        }
    }

    pub fn is_enabled() -> Result<bool> {
        let key = match CURRENT_USER.open(RUN_KEY) {
            Ok(key) => key,
            Err(error) if is_not_found(&error) => return Ok(false),
            Err(error) => {
                return Err(anyhow::anyhow!("打开 Windows 用户启动项失败：{error}"));
            }
        };
        match key.get_string(APP_NAME) {
            Ok(_) => Ok(true),
            Err(error) if is_not_found(&error) => Ok(false),
            Err(error) => Err(anyhow::anyhow!("读取 Windows 用户启动项失败：{error}")),
        }
    }
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

#[cfg(test)]
mod tests {
    use super::launched_from_args;

    #[test]
    fn startup_argument_requires_the_exact_flag() {
        assert!(launched_from_args(["meshshare", "--meshshare-autostart"]));
        assert!(!launched_from_args(["meshshare", "--autostart"]));
    }

    #[cfg(unix)]
    #[test]
    fn shell_script_paths_escape_single_quotes() {
        use super::shell_single_quote;
        use std::path::Path;

        assert_eq!(
            shell_single_quote(Path::new("/tmp/Mesh Share/o'h")),
            "'/tmp/Mesh Share/o'\\''h'"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn desktop_exec_quotes_spaces_and_shell_characters() {
        use super::desktop_exec_argument;
        use std::path::Path;

        assert_eq!(
            desktop_exec_argument(Path::new("/Applications/Mesh Share/$app")),
            "\"/Applications/Mesh Share/\\$app\""
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn plist_values_are_xml_escaped() {
        use super::xml_escape;

        assert_eq!(xml_escape("a<&\"'"), "a&lt;&amp;&quot;&apos;");
    }
}
