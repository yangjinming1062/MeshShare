use crate::runtime::file_modified_ms;
use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub const DEFAULT_BROWSE_ENTRIES: usize = 500;
pub const MAX_BROWSE_ENTRIES: usize = 2_000;
/// 单个目录最多扫描的条目数；超过后按截断返回，避免超大目录拖垮请求。
const MAX_SCANNED_ENTRIES: usize = 10_000;

/// 远程管理页可浏览的起始位置。
#[derive(Debug, Clone, Serialize)]
pub struct BrowseRoot {
    pub label: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BrowseRoots {
    pub home: Option<String>,
    pub roots: Vec<BrowseRoot>,
}

/// 目录项只暴露元数据，文件内容不经过这些接口。
#[derive(Debug, Clone, Serialize)]
pub struct BrowseEntry {
    pub name: String,
    pub path: String,
    pub directory: bool,
    pub symlink: bool,
    pub size: u64,
    pub modified_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BrowseListing {
    pub path: String,
    pub parent: Option<String>,
    pub entries: Vec<BrowseEntry>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowseError {
    NotFound,
    NotDirectory,
    Denied(String),
    Io(String),
}

/// 起始位置：用户主目录加各平台的卷根，去重后只保留真实存在的目录。
pub fn roots() -> BrowseRoots {
    let home = home_directory();
    let mut roots = Vec::new();
    let mut seen = HashSet::new();
    if let Some(home) = home.as_deref() {
        push_root(&mut roots, &mut seen, "主目录".to_string(), home);
    }
    for (label, path) in system_roots() {
        push_root(&mut roots, &mut seen, label, &path);
    }
    BrowseRoots {
        home: home.map(|path| path.to_string_lossy().into_owned()),
        roots,
    }
}

/// 列出目录的一层内容；请求路径必须是绝对路径，返回值里的路径都已规范化。
pub fn list(path: &Path, offset: usize, limit: usize) -> Result<BrowseListing, BrowseError> {
    if !path.is_absolute() {
        return Err(BrowseError::NotDirectory);
    }
    let canonical = std::fs::canonicalize(path).map_err(map_io_error)?;
    let metadata = std::fs::metadata(&canonical).map_err(map_io_error)?;
    if !metadata.is_dir() {
        return Err(BrowseError::NotDirectory);
    }
    let limit = limit.clamp(1, MAX_BROWSE_ENTRIES);
    let (mut entries, scan_capped) = collect_entries(&canonical)?;
    let total = entries.len();
    let start = offset.min(total);
    let end = start.saturating_add(limit).min(total);
    entries.truncate(end);
    entries.drain(..start);
    Ok(BrowseListing {
        path: canonical.to_string_lossy().into_owned(),
        parent: canonical
            .parent()
            .map(|parent| parent.to_string_lossy().into_owned()),
        entries,
        truncated: scan_capped || total > end,
    })
}

fn collect_entries(directory: &Path) -> Result<(Vec<BrowseEntry>, bool), BrowseError> {
    let read = std::fs::read_dir(directory).map_err(map_io_error)?;
    let mut entries = Vec::new();
    let mut capped = false;
    for item in read {
        if entries.len() >= MAX_SCANNED_ENTRIES {
            capped = true;
            break;
        }
        // 单个条目读不到（权限、临时文件消失）不应让整页失败。
        let Ok(item) = item else { continue };
        let symlink = item
            .file_type()
            .map(|kind| kind.is_symlink())
            .unwrap_or(false);
        let path = item.path();
        // 非 UTF-8 名称无法在 JSON 里往返寻址，跳过而不是返回占位字符。
        let (Some(name), Some(path_text)) = (
            item.file_name().to_str().map(str::to_string),
            path.to_str().map(str::to_string),
        ) else {
            continue;
        };
        // 跟随符号链接取目标类型，指向目录的链接才能被进入；断链按普通文件处理。
        let metadata = std::fs::metadata(&path).ok();
        entries.push(BrowseEntry {
            name,
            path: path_text,
            directory: metadata.as_ref().is_some_and(|value| value.is_dir()),
            symlink,
            size: metadata
                .as_ref()
                .filter(|value| value.is_file())
                .map(std::fs::Metadata::len)
                .unwrap_or(0),
            modified_ms: metadata.as_ref().and_then(file_modified_ms),
        });
    }
    entries.sort_by(|left, right| {
        right
            .directory
            .cmp(&left.directory)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok((entries, capped))
}

fn push_root(roots: &mut Vec<BrowseRoot>, seen: &mut HashSet<String>, label: String, path: &Path) {
    if !path.is_dir() {
        return;
    }
    let value = path.to_string_lossy().into_owned();
    if seen.insert(value.clone()) {
        roots.push(BrowseRoot { label, path: value });
    }
}

fn home_directory() -> Option<PathBuf> {
    let raw = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    let path = PathBuf::from(raw);
    if !path.is_dir() {
        return None;
    }
    Some(std::fs::canonicalize(&path).unwrap_or(path))
}

#[cfg(windows)]
fn system_roots() -> Vec<(String, PathBuf)> {
    let mut roots = Vec::new();
    for letter in b'A'..=b'Z' {
        let path = PathBuf::from(format!("{}:\\", letter as char));
        if path.is_dir() {
            roots.push((format!("{} 盘", letter as char), path));
        }
    }
    roots
}

#[cfg(target_os = "macos")]
fn system_roots() -> Vec<(String, PathBuf)> {
    vec![
        ("文件系统".to_string(), PathBuf::from("/")),
        ("卷".to_string(), PathBuf::from("/Volumes")),
    ]
}

#[cfg(all(unix, not(target_os = "macos")))]
fn system_roots() -> Vec<(String, PathBuf)> {
    vec![("文件系统".to_string(), PathBuf::from("/"))]
}

fn map_io_error(error: std::io::Error) -> BrowseError {
    match error.kind() {
        std::io::ErrorKind::NotFound => BrowseError::NotFound,
        std::io::ErrorKind::PermissionDenied => BrowseError::Denied(error.to_string()),
        _ => BrowseError::Io(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("meshshare-browse-{label}-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn listing_puts_directories_first_and_sorts_case_insensitively() {
        let root = temp_dir("order");
        std::fs::create_dir_all(root.join("beta")).expect("create beta");
        std::fs::create_dir_all(root.join("Alpha")).expect("create alpha");
        std::fs::write(root.join("Gamma.txt"), b"data").expect("write file");
        let listing = list(&root, 0, DEFAULT_BROWSE_ENTRIES).expect("list directory");
        let names: Vec<&str> = listing
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        assert_eq!(names, vec!["Alpha", "beta", "Gamma.txt"]);
        assert!(listing.entries[0].directory);
        assert!(!listing.entries[2].directory);
        assert_eq!(listing.entries[2].size, 4);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn listing_pages_with_offset_and_reports_truncation() {
        let root = temp_dir("page");
        std::fs::create_dir_all(&root).expect("create root");
        for name in ["a", "b", "c"] {
            std::fs::write(root.join(name), b"").expect("write file");
        }
        let first = list(&root, 0, 2).expect("first page");
        assert_eq!(first.entries.len(), 2);
        assert!(first.truncated);
        let second = list(&root, 2, 2).expect("second page");
        assert_eq!(second.entries.len(), 1);
        assert!(!second.truncated);
        let empty = list(&root, 9, 2).expect("offset past end");
        assert!(empty.entries.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn listing_rejects_missing_paths_and_files() {
        let root = temp_dir("reject");
        std::fs::create_dir_all(&root).expect("create root");
        let file = root.join("file.txt");
        std::fs::write(&file, b"x").expect("write file");
        assert!(matches!(
            list(&root.join("missing"), 0, 10),
            Err(BrowseError::NotFound)
        ));
        assert!(matches!(list(&file, 0, 10), Err(BrowseError::NotDirectory)));
        assert!(matches!(
            list(&PathBuf::from("relative"), 0, 10),
            Err(BrowseError::NotDirectory)
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn listing_normalizes_dot_segments() {
        let root = temp_dir("normalize");
        let child = root.join("child");
        std::fs::create_dir_all(&child).expect("create child");
        let requested = child.join("..").join("child");
        let listing = list(&requested, 0, 10).expect("list normalized path");
        let canonical = std::fs::canonicalize(&child).expect("canonical child");
        assert_eq!(listing.path, canonical.to_string_lossy());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn listing_marks_symlinked_directories_as_enterable() {
        let root = temp_dir("symlink");
        let target = root.join("target");
        std::fs::create_dir_all(&target).expect("create target");
        std::os::unix::fs::symlink(&target, root.join("link")).expect("create symlink");
        let listing = list(&root, 0, 10).expect("list directory");
        let link = listing
            .entries
            .iter()
            .find(|entry| entry.name == "link")
            .expect("find symlink entry");
        assert!(link.symlink);
        assert!(link.directory);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn roots_always_include_the_home_directory() {
        let roots = roots();
        if let Some(home) = roots.home {
            assert!(roots.roots.iter().any(|root| root.path == home));
        }
    }
}
