use super::*;
use std::path::{Path, PathBuf};

pub(super) const ANDROID_DEFAULT_NODE_NAME: &str = "移动设备";
pub(super) const DESKTOP_DEFAULT_NODE_NAME: &str = "MeshShare 节点";
/// 与对端 `validate_node_info` 的名称上限一致：超长名称会让整个握手消息被丢弃。
pub(super) const MAX_NODE_NAME_BYTES: usize = 256;

pub(super) fn truncate_node_name(name: &str) -> String {
    if name.len() <= MAX_NODE_NAME_BYTES {
        return name.to_string();
    }
    let mut end = MAX_NODE_NAME_BYTES;
    while end > 0 && !name.is_char_boundary(end) {
        end -= 1;
    }
    name[..end].to_string()
}

pub(super) fn system_hostname() -> Option<String> {
    hostname::get()
        .ok()
        .and_then(|value| value.into_string().ok())
}

/// 主机名只是系统占位值时不能当设备名：Android 的 net.hostname 默认就是 localhost，
/// 照抄会让每台手机都以同一个名字出现在其他节点的设备列表里。
pub(super) fn is_placeholder_node_name(name: &str) -> bool {
    let name = name.trim().to_ascii_lowercase();
    name.is_empty()
        || matches!(
            name.as_str(),
            "localhost" | "localhost.localdomain" | "ip6-localhost" | "ip6-loopback"
        )
}

pub(super) fn default_node_name(profile: NodeProfile, hostname: Option<String>) -> String {
    if let Some(name) = hostname.filter(|value| !is_placeholder_node_name(value)) {
        return truncate_node_name(name.trim());
    }
    if profile.is_android() {
        ANDROID_DEFAULT_NODE_NAME.to_string()
    } else {
        DESKTOP_DEFAULT_NODE_NAME.to_string()
    }
}

/// 节点名称以系统设备名为权威：系统名可用时覆盖已保存名称，不可用时保留有效的旧名。
/// 返回是否改写了名称；调用方负责把结果落盘。
pub(super) fn sync_node_name(
    name: &mut String,
    profile: NodeProfile,
    hostname: Option<String>,
) -> bool {
    let resolved = match hostname.filter(|value| !is_placeholder_node_name(value)) {
        Some(hostname) => truncate_node_name(hostname.trim()),
        None if is_placeholder_node_name(name) => default_node_name(profile, None),
        None => return false,
    };
    if name == &resolved {
        return false;
    }
    *name = resolved;
    true
}

pub(super) fn load_or_create_config(
    data_dir: &Path,
    profile: NodeProfile,
) -> anyhow::Result<PersistedConfig> {
    load_or_create_config_with_hostname(data_dir, profile, system_hostname())
}

pub(super) fn load_or_create_config_with_hostname(
    data_dir: &Path,
    profile: NodeProfile,
    hostname: Option<String>,
) -> anyhow::Result<PersistedConfig> {
    let path = data_dir.join("node.json");
    match std::fs::read_to_string(&path) {
        Ok(contents) => {
            let value: serde_json::Value = serde_json::from_str(&contents)
                .map_err(|error| anyhow::anyhow!("节点配置文件损坏：{error}"))?;
            let mut config: PersistedConfig = serde_json::from_value(value.clone())
                .map_err(|error| anyhow::anyhow!("节点配置文件损坏：{error}"))?;
            if value.get("monitor_visible").is_none() {
                config.monitor_visible = !matches!(
                    value.get("monitor_display").and_then(|item| item.as_str()),
                    Some("tray" | "disabled")
                );
            }
            config.clipboard_history_count = config
                .clipboard_history_count
                .clamp(1, MAX_CLIPBOARD_HISTORY_COUNT);
            config.clipboard_history_days = config
                .clipboard_history_days
                .clamp(1, MAX_CLIPBOARD_HISTORY_DAYS);
            if sync_node_name(&mut config.node_name, profile, hostname.clone()) {
                if let Err(error) = persist_config(data_dir, &config) {
                    eprintln!("保存节点名称失败：{error}");
                }
            }
            return Ok(config);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let config = PersistedConfig {
        node_id: Uuid::new_v4().to_string(),
        node_name: default_node_name(profile, hostname),
        monitor_visible: true,
        monitor_position: None,
        monitor_locked: false,
        autostart_enabled: default_autostart_enabled(),
        update_frequency: UpdateFrequency::default(),
        update_last_checked_at_ms: None,
        pending_update_version: None,
        update_proxy: None,
        clipboard_history_mode: ClipboardHistoryMode::default(),
        clipboard_history_count: default_clipboard_history_count(),
        clipboard_history_days: default_clipboard_history_days(),
        receive_mode: default_receive_mode(),
        remote_file_access: false,
        receive_directory: None,
        receive_directory_bookmark: None,
    };
    persist_config(data_dir, &config)?;
    Ok(config)
}

#[cfg(target_os = "macos")]
pub(super) fn activate_receive_directory_access(config: &PersistedConfig) -> Option<PathBuf> {
    let bookmark = config.receive_directory_bookmark.as_deref()?;
    match crate::macos::security_scope::activate_bookmark(bookmark) {
        Ok(path) => Some(path),
        Err(error) => {
            eprintln!("恢复接收目录授权失败：{error}");
            None
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

pub(super) fn desktop_temp_registry_path(data_dir: &Path) -> PathBuf {
    data_dir.join(DESKTOP_TEMP_REGISTRY_FILE)
}

pub(super) fn is_desktop_temp_file_path(path: &Path) -> bool {
    if !path.is_absolute() {
        return false;
    }
    let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
        return false;
    };
    name.contains(".meshshare-") && name.ends_with(".part")
}

pub(super) fn load_and_cleanup_desktop_temp_files(
    data_dir: &Path,
) -> anyhow::Result<HashSet<PathBuf>> {
    let registry_path = desktop_temp_registry_path(data_dir);
    let contents = match std::fs::read(&registry_path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    let registered = if contents.is_empty() {
        Vec::new()
    } else {
        serde_json::from_slice::<Vec<PathBuf>>(&contents)
            .map_err(|error| anyhow::anyhow!("桌面临时文件登记损坏：{error}"))?
    };
    if registered.len() > MAX_DESKTOP_TEMP_FILES {
        return Err(anyhow::anyhow!("桌面临时文件登记超过数量上限"));
    }
    let mut remaining = HashSet::new();
    for path in registered {
        if !is_desktop_temp_file_path(&path) {
            eprintln!("忽略无效的桌面临时文件登记：{}", path.display());
            continue;
        }
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                eprintln!("读取桌面临时文件失败 {}：{error}", path.display());
                remaining.insert(path);
                continue;
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            eprintln!("跳过非普通桌面临时文件：{}", path.display());
            remaining.insert(path);
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) => {
                eprintln!("清理桌面临时文件失败 {}：{error}", path.display());
                remaining.insert(path);
            }
        }
    }
    persist_desktop_temp_files(data_dir, &remaining)?;
    Ok(remaining)
}

pub(super) fn persist_desktop_temp_files(
    data_dir: &Path,
    files: &HashSet<PathBuf>,
) -> anyhow::Result<()> {
    let mut entries = files.iter().cloned().collect::<Vec<_>>();
    entries.sort();
    crate::fsutil::atomic_write(
        &desktop_temp_registry_path(data_dir),
        &serde_json::to_vec_pretty(&entries)?,
    )
}

pub(super) fn persist_config(data_dir: &Path, config: &PersistedConfig) -> anyhow::Result<()> {
    crate::fsutil::atomic_write(
        &data_dir.join("node.json"),
        &serde_json::to_vec_pretty(config)?,
    )
}
