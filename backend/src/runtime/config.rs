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
            let mut config: PersistedConfig = serde_json::from_str(&contents)
                .map_err(|error| anyhow::anyhow!("节点配置文件损坏：{error}"))?;
            anyhow::ensure!(
                config.headless == (profile == NodeProfile::Headless),
                "无界面服务与桌面节点需要分别使用独立的数据目录"
            );
            config.clipboard_history_count = config
                .clipboard_history_count
                .clamp(1, MAX_CLIPBOARD_HISTORY_COUNT);
            config.clipboard_history_days = config
                .clipboard_history_days
                .clamp(1, MAX_CLIPBOARD_HISTORY_DAYS);
            if sync_node_name(&mut config.node_name, profile, hostname) {
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
        headless: profile == NodeProfile::Headless,
        node_id: Uuid::new_v4().to_string(),
        node_name: default_node_name(profile, hostname),
        monitor_visible: profile != NodeProfile::Headless,
        monitor_position: None,
        monitor_locked: false,
        autostart_enabled: true,
        update_frequency: UpdateFrequency::default(),
        update_last_checked_at_ms: None,
        pending_update_version: None,
        clipboard_history_mode: ClipboardHistoryMode::default(),
        clipboard_history_count: DEFAULT_CLIPBOARD_HISTORY_COUNT,
        clipboard_history_days: DEFAULT_CLIPBOARD_HISTORY_DAYS,
        receive_mode: ReceiveMode::default(),
        remote_file_access: profile == NodeProfile::Headless,
        #[cfg(all(feature = "gui", not(target_os = "android")))]
        terminal_access_migration: TerminalAccessMigration::NoLegacyPolicy,
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UpdateFrequency {
    Daily,
    #[default]
    Weekly,
    Disabled,
}

impl UpdateFrequency {
    pub fn period_ms(self) -> Option<i64> {
        match self {
            Self::Daily => Some(24 * 60 * 60 * 1000),
            Self::Weekly => Some(7 * 24 * 60 * 60 * 1000),
            Self::Disabled => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct PersistedConfig {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    headless: bool,
    pub(super) node_id: String,
    pub(super) node_name: String,
    pub(super) monitor_visible: bool,
    pub(super) monitor_position: Option<MonitorPosition>,
    pub(super) monitor_locked: bool,
    pub(super) autostart_enabled: bool,
    pub(super) update_frequency: UpdateFrequency,
    pub(super) update_last_checked_at_ms: Option<i64>,
    pub(super) pending_update_version: Option<String>,
    pub(super) clipboard_history_mode: ClipboardHistoryMode,
    pub(super) clipboard_history_count: usize,
    pub(super) clipboard_history_days: u32,
    pub(super) receive_mode: ReceiveMode,
    pub(super) receive_directory: Option<String>,
    /// 所选接收目录的 macOS 安全作用域书签。
    /// 其他平台保持未设置，继续使用 path 字段。
    pub(super) receive_directory_bookmark: Option<Vec<u8>>,
    /// 是否允许其他设备在远程管理中浏览本机目录并选择发送。
    remote_file_access: bool,
    #[cfg(all(feature = "gui", not(target_os = "android")))]
    #[serde(default)]
    terminal_access_migration: TerminalAccessMigration,
}

#[cfg(all(feature = "gui", not(target_os = "android")))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TerminalAccessMigration {
    // 已有 node.json 缺少此字段时按旧版处理。旧版从未启用终端也不会创建授权文件。
    #[default]
    Pending,
    NoLegacyPolicy,
    AcceptedPolicy(String),
}

#[cfg(all(feature = "gui", not(target_os = "android")))]
impl TerminalAccessMigration {
    fn current(data_dir: &Path) -> anyhow::Result<Self> {
        use anyhow::{ensure, Context};
        use std::io::Read;
        const MAX_BYTES: u64 = 1024 * 1024;

        let path = data_dir.join("terminal-access.json");
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::NoLegacyPolicy)
            }
            Err(error) => return Err(error).context("无法检查本机终端授权记录"),
        };
        ensure!(
            metadata.is_file() && metadata.len() <= MAX_BYTES,
            "本机终端授权文件无效"
        );
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .context("无法打开本机终端授权文件")?
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .context("无法读取本机终端授权文件")?;
        ensure!(bytes.len() as u64 <= MAX_BYTES, "本机终端授权文件过大");
        Ok(Self::AcceptedPolicy(format!("{:x}", Sha256::digest(bytes))))
    }
}

#[cfg(all(feature = "gui", not(target_os = "android")))]
impl PersistedConfig {
    pub(super) fn terminal_host_ready(&self, data_dir: &Path) -> bool {
        match TerminalAccessMigration::current(data_dir) {
            Ok(current) => self.terminal_access_migration == current,
            Err(error) => {
                eprintln!("保持入站终端关闭：{error:#}");
                false
            }
        }
    }
}

/// 界面使用的接收设置：配置保留规范化路径，展示时去掉 Windows 扩展长度前缀。
fn receive_settings_view(config: &PersistedConfig) -> ReceiveSettings {
    ReceiveSettings {
        mode: config.receive_mode,
        default_path: config
            .receive_directory
            .as_deref()
            .map(|path| crate::fsutil::display_absolute_path(Path::new(path))),
    }
}

pub(super) fn history_settings(config: &PersistedConfig) -> ClipboardHistorySettings {
    ClipboardHistorySettings {
        mode: config.clipboard_history_mode,
        count: config.clipboard_history_count,
        days: config.clipboard_history_days,
    }
}

impl NodeRuntime {
    #[cfg(all(feature = "gui", not(target_os = "android")))]
    pub(crate) fn allow_remote_terminal(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.profile == NodeProfile::Desktop,
            "只有本机桌面可以确认终端访问"
        );
        if self.ssh.host_fingerprint().is_none() {
            self.update_config(|config| {
                config.terminal_access_migration =
                    TerminalAccessMigration::current(&self.data_dir)?;
                Ok(())
            })?;
            self.ssh.activate_host();
        }
        Ok(())
    }

    pub fn monitor_visible(&self) -> bool {
        self.config.lock().monitor_visible
    }

    /// 配置修改统一顺序：锁内改副本，写盘成功后回写内存；写盘失败保持原值。
    fn update_config(
        &self,
        change: impl FnOnce(&mut PersistedConfig) -> anyhow::Result<()>,
    ) -> anyhow::Result<PersistedConfig> {
        let mut config = self.config.lock();
        let mut next = config.clone();
        change(&mut next)?;
        persist_config(&self.data_dir, &next)?;
        *config = next.clone();
        Ok(next)
    }

    pub fn set_monitor_visible(&self, visible: bool) -> anyhow::Result<()> {
        self.update_config(|config| {
            config.monitor_visible = visible;
            Ok(())
        })
        .map(|_| ())
    }

    #[cfg(any(test, feature = "gui"))]
    pub fn monitor_locked(&self) -> bool {
        self.config.lock().monitor_locked
    }

    pub fn set_monitor_locked(&self, locked: bool) -> anyhow::Result<()> {
        self.update_config(|config| {
            config.monitor_locked = locked;
            Ok(())
        })
        .map(|_| ())
    }

    #[cfg(any(test, all(feature = "gui", not(target_os = "android"))))]
    pub fn monitor_position(&self) -> Option<MonitorPosition> {
        self.config.lock().monitor_position.clone()
    }

    #[cfg(any(test, all(feature = "gui", not(target_os = "android"))))]
    pub fn set_monitor_position(&self, position: MonitorPosition) -> anyhow::Result<()> {
        self.update_config(|config| {
            config.monitor_position = Some(position);
            Ok(())
        })
        .map(|_| ())
    }

    /// 切换剪贴板同步开关并返回新状态；托盘与控制面共用。
    #[cfg(all(feature = "gui", not(target_os = "android")))]
    pub fn toggle_sync_enabled(&self) -> bool {
        !self.sync_enabled.fetch_xor(true, Ordering::SeqCst)
    }

    pub fn autostart_enabled(&self) -> bool {
        self.config.lock().autostart_enabled
    }

    pub fn set_autostart_enabled(&self, enabled: bool) -> anyhow::Result<()> {
        self.update_config(|config| {
            config.autostart_enabled = enabled;
            Ok(())
        })
        .map(|_| ())
    }

    pub fn remote_file_access(&self) -> bool {
        self.config.lock().remote_file_access
    }

    #[cfg(any(test, feature = "gui"))]
    pub fn set_remote_file_access(&self, enabled: bool) -> anyhow::Result<()> {
        self.update_config(|config| {
            config.remote_file_access = enabled;
            Ok(())
        })
        .map(|_| ())
    }

    pub fn receive_settings(&self) -> ReceiveSettings {
        receive_settings_view(&self.config.lock())
    }

    pub fn set_receive_settings(
        &self,
        mode: ReceiveMode,
        directory: Option<PathBuf>,
    ) -> anyhow::Result<ReceiveSettings> {
        let canonical_directory = directory
            .as_deref()
            .map(validate_receive_directory)
            .transpose()?;
        if mode == ReceiveMode::Auto && canonical_directory.is_none() {
            anyhow::bail!("自动接收必须设置默认目录");
        }
        let config = self.update_config(|config| {
            config.receive_mode = mode;
            config.receive_directory = canonical_directory
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned());
            #[cfg(target_os = "macos")]
            {
                config.receive_directory_bookmark = canonical_directory
                    .as_deref()
                    .and_then(|path| crate::macos::security_scope::create_bookmark(path).ok());
                if let Some(bookmark) = config.receive_directory_bookmark.as_deref() {
                    if let Err(error) = crate::macos::security_scope::activate_bookmark(bookmark) {
                        eprintln!("激活接收目录授权失败：{error}");
                    }
                }
            }
            Ok(())
        })?;
        Ok(receive_settings_view(&config))
    }

    pub(super) fn default_receive_directory(&self) -> Option<PathBuf> {
        let directory = {
            let config = self.config.lock();
            if config.receive_mode != ReceiveMode::Auto {
                return None;
            }
            config.receive_directory.clone()?
        };
        validate_receive_directory(Path::new(&directory)).ok()
    }

    pub fn update_settings(&self) -> (UpdateFrequency, Option<i64>, Option<String>) {
        let config = self.config.lock();
        (
            config.update_frequency,
            config.update_last_checked_at_ms,
            config.pending_update_version.clone(),
        )
    }

    pub fn set_update_frequency(&self, frequency: UpdateFrequency) -> anyhow::Result<()> {
        self.update_config(|config| {
            config.update_frequency = frequency;
            Ok(())
        })
        .map(|_| ())
    }

    #[cfg(any(test, not(target_os = "android")))]
    pub fn set_update_check_state(
        &self,
        checked_at_ms: Option<i64>,
        pending_version: Option<String>,
    ) -> anyhow::Result<()> {
        self.update_config(|config| {
            config.update_last_checked_at_ms = checked_at_ms;
            config.pending_update_version = pending_version;
            Ok(())
        })
        .map(|_| ())
    }

    pub fn clipboard_history_settings(&self) -> ClipboardHistorySettings {
        history_settings(&self.config.lock())
    }

    pub fn set_clipboard_history_settings(
        &self,
        mode: ClipboardHistoryMode,
        value: u64,
    ) -> anyhow::Result<ClipboardHistorySettings> {
        let config = self.update_config(|config| {
            match mode {
                ClipboardHistoryMode::Count => {
                    let count = usize::try_from(value)
                        .ok()
                        .filter(|value| (1..=MAX_CLIPBOARD_HISTORY_COUNT).contains(value))
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "剪贴板历史条数必须在 1 到 {MAX_CLIPBOARD_HISTORY_COUNT} 之间"
                            )
                        })?;
                    config.clipboard_history_count = count;
                }
                ClipboardHistoryMode::Days => {
                    let days = u32::try_from(value)
                        .ok()
                        .filter(|value| (1..=MAX_CLIPBOARD_HISTORY_DAYS).contains(value))
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "剪贴板历史天数必须在 1 到 {MAX_CLIPBOARD_HISTORY_DAYS} 之间"
                            )
                        })?;
                    config.clipboard_history_days = days;
                }
            }
            config.clipboard_history_mode = mode;
            Ok(())
        })?;
        self.prune_clipboard_history();
        Ok(history_settings(&config))
    }
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
