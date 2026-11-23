use super::*;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use image::imageops::FilterType;
use image::ImageFormat;
use std::io::Cursor;
use std::path::{Path, PathBuf};

const CLIPBOARD_THUMBNAIL_WIDTH: u32 = 96;
const CLIPBOARD_THUMBNAIL_HEIGHT: u32 = 72;

pub(super) fn clipboard_payload_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(CLIPBOARD_PAYLOAD_DIR)
}

/// 缩略图与图片载荷同目录，文件名由图片名派生，索引里不另存路径。
pub(super) fn clipboard_thumbnail_name(image_path: &str) -> String {
    let stem = Path::new(image_path)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or(image_path);
    format!("{stem}.thumb.png")
}

/// 首次请求时生成并落盘；写盘失败仍返回内存中的缩略图。
pub(super) fn load_or_create_clipboard_thumbnail(
    data_dir: &Path,
    record: &ClipboardHistoryRecord,
) -> Option<Vec<u8>> {
    let image_name = record.image_path.as_deref()?;
    if !safe_clipboard_payload_name(image_name, "png") {
        return None;
    }
    let directory = clipboard_payload_dir(data_dir);
    let thumb_name = clipboard_thumbnail_name(image_name);
    if let Some(bytes) = std::fs::read(directory.join(&thumb_name))
        .ok()
        .filter(|bytes| crate::clipboard::is_valid_png(bytes))
    {
        return Some(bytes);
    }
    let source = std::fs::read(directory.join(image_name)).ok()?;
    let thumbnail = crate::clipboard::decode_png(&source).ok()?.resize_to_fill(
        CLIPBOARD_THUMBNAIL_WIDTH,
        CLIPBOARD_THUMBNAIL_HEIGHT,
        FilterType::Triangle,
    );
    let mut bytes = Vec::new();
    thumbnail
        .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
        .ok()?;
    let _ = crate::fsutil::atomic_write(&directory.join(&thumb_name), &bytes);
    Some(bytes)
}

pub(super) fn is_clipboard_index_temp_name(name: &str, base: &str) -> bool {
    name.strip_prefix(base)
        .is_some_and(|suffix| suffix.starts_with(".tmp-"))
}

pub(super) fn remove_clipboard_temp_file(path: &Path) {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return;
    };
    if !(metadata.is_file() || metadata.file_type().is_symlink()) {
        return;
    }
    if let Err(error) = std::fs::remove_file(path) {
        eprintln!("清理剪贴板临时文件失败 {}：{error}", path.display());
    }
}

pub(super) fn cleanup_clipboard_temp_files(data_dir: &Path) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(data_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if is_clipboard_index_temp_name(name, CLIPBOARD_HISTORY_FILE)
            || is_clipboard_index_temp_name(name, CLIPBOARD_CURRENT_FILE)
        {
            remove_clipboard_temp_file(&entry.path());
        }
    }
    let payload_dir = clipboard_payload_dir(data_dir);
    let entries = match std::fs::read_dir(&payload_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_str().is_some_and(|name| name.contains(".tmp-")) {
            remove_clipboard_temp_file(&entry.path());
        }
    }
    Ok(())
}

pub(super) fn safe_clipboard_payload_name(name: &str, extension: &str) -> bool {
    let path = Path::new(name);
    path.components().count() == 1
        && path.file_name().and_then(|value| value.to_str()) == Some(name)
        && path.extension().and_then(|value| value.to_str()) == Some(extension)
        && name.len() <= 128
}

pub(super) fn clipboard_history_record_valid(
    data_dir: &Path,
    record: &ClipboardHistoryRecord,
) -> bool {
    if record.event_id.is_empty()
        || record.event_id.len() > 256
        || record.source_node_id.is_empty()
        || record.source_node_id.len() > 256
        || record.source_name.len() > 256
        || record.content_hash.len() != 64
        || (record.text_path.is_none() && record.image_path.is_none())
    {
        return false;
    }
    let payload_dir = clipboard_payload_dir(data_dir);
    let valid_file = |name: &str, extension, expected, limit| {
        expected <= limit
            && safe_clipboard_payload_name(name, extension)
            && std::fs::symlink_metadata(payload_dir.join(name))
                .is_ok_and(|meta| meta.is_file() && meta.len() == expected as u64)
    };
    record
        .text_path
        .as_deref()
        .is_none_or(|name| valid_file(name, "txt", record.text_len, CLIPBOARD_TEXT_LIMIT))
        && record.image_path.as_deref().is_none_or(|name| {
            valid_file(name, "png", record.image_len, CLIPBOARD_IMAGE_LIMIT)
                && std::fs::read(payload_dir.join(name))
                    .is_ok_and(|bytes| crate::clipboard::is_valid_png(&bytes))
        })
}

pub(super) fn load_clipboard_history(
    data_dir: &Path,
) -> anyhow::Result<Vec<ClipboardHistoryRecord>> {
    let path = data_dir.join(CLIPBOARD_HISTORY_FILE);
    let contents = match std::fs::read(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let records = match serde_json::from_slice::<Vec<ClipboardHistoryRecord>>(&contents) {
        Ok(records) => records,
        Err(error) => {
            eprintln!("剪贴板历史索引损坏，已忽略：{error}");
            return Ok(Vec::new());
        }
    };
    Ok(records
        .into_iter()
        .take(MAX_CLIPBOARD_HISTORY_COUNT)
        .filter(|record| clipboard_history_record_valid(data_dir, record))
        .collect())
}

pub(super) fn persist_clipboard_history(
    data_dir: &Path,
    history: &[ClipboardHistoryRecord],
) -> anyhow::Result<()> {
    std::fs::create_dir_all(clipboard_payload_dir(data_dir))?;
    crate::fsutil::atomic_write(
        &data_dir.join(CLIPBOARD_HISTORY_FILE),
        &serde_json::to_vec_pretty(history)?,
    )?;
    let mut referenced = HashSet::new();
    for record in history {
        if let Some(path) = record.text_path.as_deref() {
            referenced.insert(path.to_string());
        }
        if let Some(path) = record.image_path.as_deref() {
            referenced.insert(path.to_string());
            referenced.insert(clipboard_thumbnail_name(path));
        }
    }
    let entries = match std::fs::read_dir(clipboard_payload_dir(data_dir)) {
        Ok(entries) => entries,
        Err(error) => {
            // 索引已经原子替换完成。
            // 清理失败不能上报为事务失败，
            // 否则调用方会回滚内存，而磁盘已指向新索引。
            // 无引用的陈旧文件在下次启动时重试是安全的。
            eprintln!("清理剪贴板历史 payload 失败：{error}");
            return Ok(());
        }
    };
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.contains(".tmp-") {
            remove_clipboard_temp_file(&entry.path());
        } else if !referenced.contains(name) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    Ok(())
}

pub(super) fn persist_clipboard_current(
    data_dir: &Path,
    snapshot: &ClipboardSnapshot,
) -> anyhow::Result<()> {
    crate::fsutil::atomic_write(
        &data_dir.join(CLIPBOARD_CURRENT_FILE),
        &serde_json::to_vec(snapshot)?,
    )
}

pub(super) fn load_clipboard_current(data_dir: &Path) -> Option<ClipboardSnapshot> {
    let contents = std::fs::read(data_dir.join(CLIPBOARD_CURRENT_FILE)).ok()?;
    let snapshot = serde_json::from_slice::<ClipboardSnapshot>(&contents).ok()?;
    validated_clipboard_image(&snapshot).ok().map(|_| snapshot)
}

pub(super) fn write_clipboard_payload(
    data_dir: &Path,
    storage_id: &str,
    extension: &str,
    bytes: &[u8],
) -> anyhow::Result<String> {
    let directory = clipboard_payload_dir(data_dir);
    std::fs::create_dir_all(&directory)?;
    let name = format!("{storage_id}.{extension}");
    crate::fsutil::atomic_write(&directory.join(&name), bytes)?;
    Ok(name)
}

pub(super) fn write_clipboard_history_record(
    data_dir: &Path,
    snapshot: &ClipboardSnapshot,
    image: Option<&[u8]>,
    recorded_at_ms: i64,
    content_hash: String,
) -> anyhow::Result<ClipboardHistoryRecord> {
    let storage_id = Uuid::new_v4().to_string();
    let text_path = snapshot
        .text
        .as_ref()
        .map(|text| write_clipboard_payload(data_dir, &storage_id, "txt", text.as_bytes()))
        .transpose()?;
    let image_path = image
        .map(|bytes| write_clipboard_payload(data_dir, &storage_id, "png", bytes))
        .transpose()
        .inspect_err(|_| {
            if let Some(name) = &text_path {
                let _ = std::fs::remove_file(clipboard_payload_dir(data_dir).join(name));
            }
        })?;
    Ok(ClipboardHistoryRecord {
        event_id: snapshot.event_id.clone(),
        source_node_id: snapshot.source_node_id.clone(),
        source_name: snapshot.source_name.clone(),
        clock: snapshot.clock.clone(),
        updated_at_ms: snapshot.updated_at_ms,
        recorded_at_ms,
        content_hash,
        text_path,
        image_path,
        text_len: snapshot.text.as_ref().map_or(0, String::len),
        image_len: image.map_or(0, |bytes| bytes.len()),
    })
}

pub(super) fn load_history_snapshot(
    data_dir: &Path,
    record: &ClipboardHistoryRecord,
) -> anyhow::Result<ClipboardSnapshot> {
    let directory = clipboard_payload_dir(data_dir);
    let text = record
        .text_path
        .as_deref()
        .map(|path| std::fs::read_to_string(directory.join(path)))
        .transpose()?;
    let image_png = record
        .image_path
        .as_deref()
        .map(|path| {
            std::fs::read(directory.join(path))
                .map(|bytes| BASE64.encode(bytes))
                .map_err(anyhow::Error::from)
        })
        .transpose()?;
    Ok(ClipboardSnapshot {
        event_id: record.event_id.clone(),
        source_node_id: record.source_node_id.clone(),
        source_name: record.source_name.clone(),
        clock: record.clock.clone(),
        text,
        image_png,
        updated_at_ms: record.updated_at_ms,
    })
}

pub(super) fn remove_clipboard_record_payloads(data_dir: &Path, record: &ClipboardHistoryRecord) {
    let directory = clipboard_payload_dir(data_dir);
    for path in [record.text_path.as_deref(), record.image_path.as_deref()]
        .into_iter()
        .flatten()
    {
        let _ = std::fs::remove_file(directory.join(path));
    }
    if let Some(image_path) = record.image_path.as_deref() {
        let _ = std::fs::remove_file(directory.join(clipboard_thumbnail_name(image_path)));
    }
}

pub(super) fn validated_clipboard_image(
    snapshot: &ClipboardSnapshot,
) -> anyhow::Result<Option<Vec<u8>>> {
    anyhow::ensure!(
        !snapshot.event_id.trim().is_empty()
            && snapshot.event_id.len() <= 256
            && !snapshot.source_node_id.trim().is_empty()
            && snapshot.source_node_id.len() <= 256
            && snapshot.source_name.len() <= 256
            && snapshot.clock.node_id == snapshot.source_node_id
            && (snapshot.text.is_some() || snapshot.image_png.is_some()),
        "剪贴板事件标识或来源无效"
    );
    let image = snapshot
        .image_png
        .as_deref()
        .map(|value| BASE64.decode(value))
        .transpose()?;
    if snapshot
        .text
        .as_ref()
        .is_some_and(|value| value.len() > CLIPBOARD_TEXT_LIMIT)
        || image
            .as_ref()
            .is_some_and(|value| value.len() > CLIPBOARD_IMAGE_LIMIT)
    {
        return Err(anyhow::anyhow!("剪贴板内容超过大小限制"));
    }
    if image
        .as_deref()
        .is_some_and(|bytes| !crate::clipboard::is_valid_png(bytes))
    {
        return Err(anyhow::anyhow!("剪贴板图片不是有效的 PNG"));
    }
    Ok(image)
}

pub(super) fn prune_clipboard_history_records(
    history: &mut Vec<ClipboardHistoryRecord>,
    config: &PersistedConfig,
    now: i64,
) {
    prune_clipboard_history_records_with_limit(history, config, now, MAX_CLIPBOARD_HISTORY_BYTES);
}

pub(super) fn prune_clipboard_history_records_with_limit(
    history: &mut Vec<ClipboardHistoryRecord>,
    config: &PersistedConfig,
    now: i64,
    max_bytes: u64,
) {
    match config.clipboard_history_mode {
        ClipboardHistoryMode::Count => history.truncate(config.clipboard_history_count),
        ClipboardHistoryMode::Days => {
            let age = i64::from(config.clipboard_history_days) * CLIPBOARD_DAY_MS;
            let threshold = now.saturating_sub(age);
            history.retain(|record| record.recorded_at_ms >= threshold);
            history.truncate(MAX_CLIPBOARD_HISTORY_COUNT);
        }
    }
    let mut total_bytes = history
        .iter()
        .map(|record| (record.text_len as u64).saturating_add(record.image_len as u64))
        .sum::<u64>();
    while total_bytes > max_bytes && history.len() > 1 {
        let Some(record) = history.pop() else { break };
        total_bytes = total_bytes
            .saturating_sub((record.text_len as u64).saturating_add(record.image_len as u64));
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ClipboardHistoryRecord {
    pub(super) event_id: String,
    pub(super) source_node_id: String,
    pub(super) source_name: String,
    pub(super) clock: crate::model::Hlc,
    pub(super) updated_at_ms: i64,
    pub(super) recorded_at_ms: i64,
    pub(super) content_hash: String,
    pub(super) text_path: Option<String>,
    pub(super) image_path: Option<String>,
    pub(super) text_len: usize,
    pub(super) image_len: usize,
}

impl NodeRuntime {
    pub fn clipboard_status(&self) -> ClipboardStatus {
        self.clipboard_status.read().clone()
    }

    #[cfg(feature = "gui")]
    pub fn set_clipboard_status(&self, status: ClipboardStatus) {
        *self.clipboard_status.write() = status;
    }

    #[cfg(all(feature = "gui", not(target_os = "android")))]
    pub fn set_clipboard_available(&self, session: ClipboardSession) {
        self.set_clipboard_status(ClipboardStatus {
            session,
            available: true,
            retrying: false,
            message: None,
        });
    }

    #[cfg(feature = "gui")]
    pub fn set_clipboard_unavailable(
        &self,
        session: ClipboardSession,
        error: impl Into<String>,
        retrying: bool,
    ) {
        let detail = error.into();
        let message = match session {
            ClipboardSession::Wayland => format!(
                "Wayland 系统剪贴板不可用：{detail}。请启用 XWayland，或使用提供 data-control 协议的合成器"
            ),
            ClipboardSession::X11 => format!("X11 系统剪贴板不可用：{detail}"),
            ClipboardSession::Other => format!("系统剪贴板不可用：{detail}"),
            ClipboardSession::Unknown => format!("无法连接系统剪贴板：{detail}"),
        };
        self.set_clipboard_status(ClipboardStatus {
            session,
            available: false,
            retrying,
            message: Some(message),
        });
    }

    pub fn prune_clipboard_history(&self) -> usize {
        let _update_guard = self.clipboard_update.lock();
        let mut history = self.clipboard_history.lock();
        self.prune_clipboard_history_locked(&mut history, &self.config.lock().clone())
    }

    fn prune_clipboard_history_locked(
        &self,
        history: &mut Vec<ClipboardHistoryRecord>,
        config: &PersistedConfig,
    ) -> usize {
        let mut next = history.clone();
        prune_clipboard_history_records(&mut next, config, now_millis());
        let removed = history.len() - next.len();
        if removed > 0 {
            if let Err(error) = persist_clipboard_history(&self.data_dir, &next) {
                eprintln!("保存剪贴板历史清理结果失败：{error}");
                return 0;
            }
            *history = next;
        }
        removed
    }

    pub fn clipboard_history_list(&self) -> ClipboardHistoryList {
        let _update_guard = self.clipboard_update.lock();
        let mut history = self.clipboard_history.lock();
        let config = self.config.lock().clone();
        self.prune_clipboard_history_locked(&mut history, &config);
        ClipboardHistoryList {
            settings: history_settings(&config),
            items: history
                .iter()
                .map(|record| self.history_item(record))
                .collect(),
        }
    }

    pub fn clipboard_history_detail(&self, event_id: &str) -> Option<ClipboardHistoryDetail> {
        let _update_guard = self.clipboard_update.lock();
        let history = self.clipboard_history.lock();
        history
            .iter()
            .find(|record| record.event_id == event_id)
            .and_then(|record| {
                load_history_snapshot(&self.data_dir, record)
                    .ok()
                    .map(|snapshot| ClipboardHistoryDetail {
                        item: self.history_item(record),
                        snapshot,
                    })
            })
    }

    pub fn clipboard_history_thumbnail(&self, event_id: &str) -> Option<Vec<u8>> {
        let _update_guard = self.clipboard_update.lock();
        let history = self.clipboard_history.lock();
        history
            .iter()
            .find(|record| record.event_id == event_id)
            .and_then(|record| load_or_create_clipboard_thumbnail(&self.data_dir, record))
    }

    fn history_item(&self, record: &ClipboardHistoryRecord) -> ClipboardHistoryItem {
        let text_preview = record.text_path.as_ref().and_then(|path| {
            let path = self.data_dir.join(CLIPBOARD_PAYLOAD_DIR).join(path);
            use std::io::Read;
            let mut bytes = Vec::new();
            std::fs::File::open(path)
                .ok()?
                .take(CLIPBOARD_HISTORY_PREVIEW_BYTES as u64)
                .read_to_end(&mut bytes)
                .ok()?;
            Some(String::from_utf8_lossy(&bytes).into_owned())
        });
        ClipboardHistoryItem {
            event_id: record.event_id.clone(),
            source_node_id: record.source_node_id.clone(),
            source_name: record.source_name.clone(),
            updated_at_ms: record.updated_at_ms,
            recorded_at_ms: record.recorded_at_ms,
            has_text: record.text_path.is_some(),
            has_image: record.image_path.is_some(),
            text_preview,
            size_bytes: (record.text_len as u64).saturating_add(record.image_len as u64),
        }
    }

    pub fn clear_clipboard_history(&self) -> anyhow::Result<usize> {
        let _update_guard = self.clipboard_update.lock();
        let mut history = self.clipboard_history.lock();
        let count = history.len();
        persist_clipboard_history(&self.data_dir, &[])?;
        history.clear();
        Ok(count)
    }
}

impl NodeRuntime {
    pub fn accept_clipboard(&self, event: ClipboardEvent, source_name: String) -> bool {
        let _update_guard = self.clipboard_update.lock();
        self.accept_clipboard_locked(event, source_name, true)
    }

    fn accept_clipboard_locked(
        &self,
        event: ClipboardEvent,
        source_name: String,
        write_to_system: bool,
    ) -> bool {
        let now = now_millis();
        let mut seen = self.seen_events.lock();
        seen.retain(|_, timestamp| now.saturating_sub(*timestamp) < 10 * 60 * 1000);
        if seen.contains_key(&event.event_id) {
            return false;
        }
        let event_id = event.event_id.clone();
        let snapshot = ClipboardSnapshot {
            event_id: event.event_id,
            source_node_id: event.origin_node_id,
            source_name,
            clock: event.clock,
            text: event.text,
            image_png: event.image_png,
            updated_at_ms: event.updated_at_ms,
        };
        let image = match validated_clipboard_image(&snapshot) {
            Ok(image) => image,
            Err(error) => {
                eprintln!("忽略无效剪贴板内容：{error}");
                return false;
            }
        };
        // 只有校验通过的负载才登记为已见，避免非法内容占用去重标识。
        seen.insert(event_id, now);
        while seen.len() > MAX_SEEN_CLIPBOARD_EVENTS {
            let Some(oldest) = seen
                .iter()
                .min_by_key(|(_, timestamp)| **timestamp)
                .map(|(event_id, _)| event_id.clone())
            else {
                break;
            };
            seen.remove(&oldest);
        }
        let (replayed_current, newer) =
            self.clipboard
                .lock()
                .snapshot
                .as_ref()
                .map_or((false, true), |current| {
                    (
                        current.event_id == snapshot.event_id,
                        snapshot.clock.compare(&current.clock).is_gt(),
                    )
                });
        if !replayed_current {
            if let Err(error) =
                self.upsert_clipboard_history_locked(&snapshot, image.as_deref(), now)
            {
                eprintln!("保存剪贴板历史失败：{error}");
            }
        }
        if !newer {
            return false;
        }
        self.clock.observe(&snapshot.clock);
        if let Err(error) = persist_clipboard_current(&self.data_dir, &snapshot) {
            eprintln!("保存当前剪贴板失败：{error}");
        }
        let mut clipboard = self.clipboard.lock();
        clipboard.snapshot = Some(snapshot);
        #[cfg(not(target_os = "android"))]
        {
            clipboard.pending_system_write =
                write_to_system && self.profile == NodeProfile::Desktop;
            if clipboard.pending_system_write {
                self.system_clipboard_ready.notify_one();
            }
        }
        #[cfg(target_os = "android")]
        let _ = write_to_system;
        true
    }

    fn upsert_clipboard_history_locked(
        &self,
        snapshot: &ClipboardSnapshot,
        image: Option<&[u8]>,
        recorded_at_ms: i64,
    ) -> anyhow::Result<()> {
        let content_hash = crate::clipboard::payload_hash(snapshot.text.as_deref(), image);
        let mut history = self.clipboard_history.lock();
        let duplicate_index = history.iter().position(|record| {
            record.event_id == snapshot.event_id || record.content_hash == content_hash
        });
        // 先写入替换用的载荷，再修改索引或删除旧记录。
        // 这样磁盘或权限失败时，
        // 旧历史条目及其载荷仍然可用于恢复。
        let record = write_clipboard_history_record(
            &self.data_dir,
            snapshot,
            image,
            recorded_at_ms,
            content_hash,
        )?;
        let new_record = record.clone();
        let mut next = history.clone();
        if let Some(index) = duplicate_index {
            next.remove(index);
        }
        next.insert(0, record);
        let config = self.config.lock().clone();
        prune_clipboard_history_records(&mut next, &config, recorded_at_ms);
        if let Err(error) = persist_clipboard_history(&self.data_dir, &next) {
            // 旧索引不引用新载荷。
            // 索引写入失败之后再删除它，同时保留旧条目。
            remove_clipboard_record_payloads(&self.data_dir, &new_record);
            return Err(error);
        }
        *history = next;
        Ok(())
    }

    /// 系统读操作可能等待其他应用。若共享状态期间已更新，保留较新的事件。
    #[cfg(all(any(feature = "gui", test), not(target_os = "android")))]
    pub fn create_local_clipboard(
        &self,
        text: Option<String>,
        image_png: Option<Vec<u8>>,
        observed_event_id: Option<&str>,
    ) -> Option<ClipboardEvent> {
        let source_name = self.node.read().node_name.clone();
        let event = self.local_clipboard_event(text, image_png, &source_name)?;
        let _update_guard = self.clipboard_update.lock();
        if self.clipboard_event_id().as_deref() != observed_event_id {
            return None;
        }
        self.accept_clipboard_locked(event.clone(), source_name, false)
            .then_some(event)
    }

    pub fn create_local_clipboard_named(
        &self,
        text: Option<String>,
        image_png: Option<Vec<u8>>,
        source_name: String,
    ) -> Option<ClipboardEvent> {
        let event = self.local_clipboard_event(text, image_png, &source_name)?;
        self.accept_clipboard(event.clone(), source_name)
            .then_some(event)
    }

    fn local_clipboard_event(
        &self,
        text: Option<String>,
        image_png: Option<Vec<u8>>,
        source_name: &str,
    ) -> Option<ClipboardEvent> {
        if !self.sync_enabled.load(Ordering::SeqCst) {
            return None;
        }
        if source_name.len() > 256
            || text
                .as_ref()
                .is_some_and(|value| value.len() > crate::model::CLIPBOARD_TEXT_LIMIT)
        {
            return None;
        }
        let image_png = image_png
            .filter(|value| value.len() <= crate::model::CLIPBOARD_IMAGE_LIMIT)
            .map(|value| BASE64.encode(value));
        if text.is_none() && image_png.is_none() {
            return None;
        }
        Some(ClipboardEvent {
            event_id: Uuid::new_v4().to_string(),
            origin_node_id: self.node_id(),
            clock: self.clock.tick(),
            text,
            image_png,
            updated_at_ms: now_millis(),
        })
    }

    pub fn restore_clipboard_history(&self, event_id: &str) -> Option<ClipboardEvent> {
        let detail = self.clipboard_history_detail(event_id)?;
        let image = detail
            .snapshot
            .image_png
            .as_deref()
            .and_then(|value| BASE64.decode(value).ok());
        let source_name = self.node.read().node_name.clone();
        self.create_local_clipboard_named(detail.snapshot.text, image, source_name)
    }

    pub fn clipboard_snapshot(&self) -> Option<ClipboardSnapshot> {
        self.clipboard.lock().snapshot.clone()
    }

    #[cfg(all(any(feature = "gui", test), not(target_os = "android")))]
    pub(crate) fn clipboard_event_id(&self) -> Option<String> {
        self.clipboard
            .lock()
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.event_id.clone())
    }

    #[cfg(all(any(feature = "gui", test), not(target_os = "android")))]
    pub(crate) fn take_system_clipboard_write(&self) -> Option<ClipboardSnapshot> {
        let mut clipboard = self.clipboard.lock();
        if std::mem::take(&mut clipboard.pending_system_write) {
            clipboard.snapshot.clone()
        } else {
            None
        }
    }

    #[cfg(all(feature = "gui", not(target_os = "android")))]
    pub(crate) fn wait_for_system_clipboard_write(&self, timeout: Duration) {
        let mut clipboard = self.clipboard.lock();
        if !clipboard.pending_system_write {
            self.system_clipboard_ready
                .wait_for(&mut clipboard, timeout);
        }
    }
}
