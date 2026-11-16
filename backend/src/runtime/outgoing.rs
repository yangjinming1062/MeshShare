use super::*;
use std::path::{Path, PathBuf};

pub(super) fn outgoing_media_type(relative_path: &str) -> String {
    let extension = relative_path
        .rsplit('.')
        .next()
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "txt" => "text/plain",
        "zip" => "application/zip",
        "apk" => "application/vnd.android.package-archive",
        _ => "application/octet-stream",
    }
    .to_string()
}

pub(super) fn push_outgoing_source(
    sources: &mut Vec<PreparedSource>,
    seen: &mut HashSet<String>,
    prefix: &str,
    relative: &Path,
    path: &Path,
) -> Result<(), String> {
    let segments = relative
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    if segments.is_empty() {
        return Err("文件相对路径无效".to_string());
    }
    let relative_path = if prefix.is_empty() {
        segments.join("/")
    } else {
        format!("{prefix}/{}", segments.join("/"))
    };
    if relative_path.len() > OUTGOING_MAX_RELATIVE_PATH
        || segments
            .iter()
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return Err(format!("文件相对路径无效：{relative_path}"));
    }
    if sources.len() >= OUTGOING_PREPARE_MAX_ITEMS {
        return Err("一次发送的项目过多，请分批发送".to_string());
    }
    if !seen.insert(relative_path.clone()) {
        return Err(format!("存在重名文件：{relative_path}"));
    }
    let metadata =
        std::fs::metadata(path).map_err(|error| format!("无法读取 {relative_path}：{error}"))?;
    if !metadata.is_file() {
        return Ok(());
    }
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .map(str::to_string)
        .ok_or_else(|| format!("{relative_path} 的文件名无效"))?;
    sources.push(PreparedSource {
        file_name,
        relative_path,
        path: path.to_path_buf(),
        size: metadata.len(),
        modified_ms: file_modified_ms(&metadata),
    });
    Ok(())
}

pub(super) fn display_absolute_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return format!(r"\\{rest}");
    }
    text.strip_prefix(r"\\?\")
        .map(str::to_string)
        .unwrap_or_else(|| text.into_owned())
}

/// 把用户选中的文件与文件夹展开成待发送清单，文件夹按选中目录名保留层级。
pub(super) fn expand_outgoing_paths(
    paths: &[String],
) -> Result<(Vec<PreparedSource>, Vec<String>), String> {
    let mut sources = Vec::new();
    let mut roots = Vec::new();
    let mut seen = HashSet::new();
    for raw in paths {
        let requested = PathBuf::from(raw);
        if !requested.is_absolute() {
            return Err("发送路径必须是绝对路径".to_string());
        }
        let canonical = std::fs::canonicalize(&requested)
            .map_err(|error| format!("无法访问 {raw}：{error}"))?;
        let metadata =
            std::fs::metadata(&canonical).map_err(|error| format!("无法读取 {raw}：{error}"))?;
        if roots.len() < MAX_TRANSFER_HISTORY_PATHS {
            let root = display_absolute_path(&canonical);
            if root.len() <= 4_096 && !roots.contains(&root) {
                roots.push(root);
            }
        }
        if metadata.is_dir() {
            let prefix = canonical
                .file_name()
                .and_then(|value| value.to_str())
                .map(str::to_string)
                .ok_or_else(|| format!("{raw} 的文件夹名无效"))?;
            for entry in WalkDir::new(&canonical).follow_links(false) {
                let entry = entry.map_err(|error| format!("遍历 {raw} 失败：{error}"))?;
                if !entry.file_type().is_file() {
                    continue;
                }
                let relative = entry
                    .path()
                    .strip_prefix(&canonical)
                    .map_err(|_| format!("{raw} 的相对路径无效"))?;
                push_outgoing_source(&mut sources, &mut seen, &prefix, relative, entry.path())?;
            }
        } else if metadata.is_file() {
            let name = canonical
                .file_name()
                .map(PathBuf::from)
                .ok_or_else(|| format!("{raw} 的文件名无效"))?;
            push_outgoing_source(&mut sources, &mut seen, "", &name, &canonical)?;
        } else {
            return Err(format!("{raw} 不是普通文件或文件夹"));
        }
    }
    sources.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok((sources, roots))
}

pub fn validate_receive_directory(path: &Path) -> anyhow::Result<PathBuf> {
    if !path.is_absolute() {
        anyhow::bail!("接收目录必须是绝对路径");
    }
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        anyhow::bail!("接收目录必须是实际目录，不能是符号链接");
    }
    Ok(std::fs::canonicalize(path)?)
}
