use anyhow::Result;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

/// 同目录临时文件名：清理逻辑按 `.tmp-` 后缀识别残留。
pub(crate) fn temp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("meshshare");
    path.with_file_name(format!("{name}.tmp-{}", std::process::id()))
}

/// 面向界面的绝对路径：Windows 的 `canonicalize` 返回 `\\?\` 扩展长度前缀，
/// 展示与跨请求往返都使用去掉前缀的形式，需要时再重新规范化。
pub(crate) fn display_absolute_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    text.strip_prefix(r"\\?\")
        .map(str::to_string)
        .unwrap_or_else(|| text.into_owned())
}

/// 删除目录；目录不存在视为成功，用于启动时清空暂存区。
pub(crate) fn remove_dir_if_exists(path: &Path) -> Result<()> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// 先写同目录临时文件再重命名，读取方不会看到半份数据。
pub(crate) fn atomic_write(path: &Path, contents: &[u8]) -> Result<()> {
    atomic_write_with(path, false, |mut file| Ok(file.write_all(contents)?))
}

pub(crate) fn atomic_write_json(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    atomic_write_with(path, false, |file| {
        let mut writer = BufWriter::new(file);
        serde_json::to_writer(&mut writer, value)?;
        Ok(writer.flush()?)
    })
}

/// 私钥等敏感文件：写入时把权限收紧到仅当前用户可读写（仅 Unix 生效）。
pub(crate) fn atomic_write_private(path: &Path, contents: &[u8]) -> Result<()> {
    atomic_write_with(path, true, |mut file| Ok(file.write_all(contents)?))
}

fn atomic_write_with(
    path: &Path,
    private: bool,
    write: impl FnOnce(File) -> Result<()>,
) -> Result<()> {
    let temporary = temp_path(path);
    if let Err(error) = open_temporary(&temporary, private)
        .map_err(anyhow::Error::from)
        .and_then(write)
    {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    commit(&temporary, path)
}

fn open_temporary(path: &Path, private: bool) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = private;
    options.open(path)
}

/// 提交临时文件。Windows 上临时文件可能被杀毒或索引服务短暂占用，
/// 失败后稍等重试一次；目标文件始终保持原样，重试失败只清理临时文件。
fn commit(temporary: &Path, path: &Path) -> Result<()> {
    match std::fs::rename(temporary, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            #[cfg(windows)]
            {
                std::thread::sleep(std::time::Duration::from_millis(50));
                if std::fs::rename(temporary, path).is_ok() {
                    return Ok(());
                }
            }
            let _ = std::fs::remove_file(temporary);
            Err(error.into())
        }
    }
}

pub(crate) fn file_modified_ms(metadata: &std::fs::Metadata) -> Option<i64> {
    metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|value| value.as_millis() as i64)
}
