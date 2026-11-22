use crate::autostart::{atomic_write, home_directory, remove_optional, wrapper_script, APP_NAME};
use anyhow::Result;
use std::path::{Path, PathBuf};

fn config_directory() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
    {
        return Ok(path);
    }
    Ok(home_directory()?.join(".config"))
}

fn desktop_file() -> Result<PathBuf> {
    Ok(config_directory()?
        .join("autostart")
        .join(format!("{APP_NAME}.desktop")))
}

fn wrapper_file() -> Result<PathBuf> {
    Ok(config_directory()?.join(APP_NAME).join("autostart.sh"))
}

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

pub(crate) fn enable(executable: &Path) -> Result<()> {
    let path = desktop_file()?;
    let wrapper = wrapper_file()?;
    atomic_write(&wrapper, &wrapper_script(executable, &path))?;
    let wrapper = desktop_exec_argument(&wrapper);
    let contents = format!(
        "[Desktop Entry]\nType=Application\nVersion=1.0\nName={APP_NAME}\nComment={APP_NAME} startup\nExec=/bin/sh {wrapper}\nStartupNotify=false\nTerminal=false\n",
    );
    atomic_write(&path, &contents)
}

pub(crate) fn disable() -> Result<()> {
    let path = desktop_file()?;
    let wrapper = wrapper_file()?;
    remove_optional(path)?;
    remove_optional(wrapper)
}

pub(crate) fn is_enabled() -> Result<bool> {
    // 主配置与 wrapper 缺一不可：wrapper 被清掉后登录不会真正启动。
    Ok(desktop_file()?.exists() && wrapper_file()?.exists())
}

#[cfg(test)]
mod tests {
    use super::desktop_exec_argument;
    use std::path::Path;

    #[test]
    fn desktop_exec_quotes_spaces_and_shell_characters() {
        assert_eq!(
            desktop_exec_argument(Path::new("/Applications/Mesh Share/$app")),
            "\"/Applications/Mesh Share/\\$app\""
        );
    }
}
