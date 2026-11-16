use crate::autostart::{APP_NAME, AUTOSTART_ARGUMENT};
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
    Ok(format!("\"{executable}\" {AUTOSTART_ARGUMENT}"))
}

pub(crate) fn enable(executable: &Path) -> Result<()> {
    let key = CURRENT_USER
        .create(RUN_KEY)
        .map_err(|error| anyhow::anyhow!("打开 Windows 用户启动项失败：{error}"))?;
    key.set_string(APP_NAME, command(executable)?)
        .map_err(|error| anyhow::anyhow!("写入 Windows 用户启动项失败：{error}"))?;
    Ok(())
}

pub(crate) fn disable() -> Result<()> {
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

pub(crate) fn is_enabled() -> Result<bool> {
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
