use super::*;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use std::path::{Path, PathBuf};

pub(super) fn clipboard_payload_dir(data_dir: &Path) -> PathBuf {
    data_dir.join(CLIPBOARD_PAYLOAD_DIR)
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
    if let Some(path) = record.text_path.as_deref() {
        let full = payload_dir.join(path);
        if !safe_clipboard_payload_name(path, "txt")
            || std::fs::symlink_metadata(&full)
                .map(|metadata| {
                    !metadata.is_file()
                        || metadata.file_type().is_symlink()
                        || metadata.len() as usize != record.text_len
                })
                .unwrap_or(true)
            || record.text_len > CLIPBOARD_TEXT_LIMIT
        {
            return false;
        }
    }
    if let Some(path) = record.image_path.as_deref() {
        let full = payload_dir.join(path);
        let valid_png = std::fs::read(&full)
            .ok()
            .is_some_and(|bytes| crate::clipboard::is_valid_png(&bytes));
        if !safe_clipboard_payload_name(path, "png")
            || std::fs::symlink_metadata(&full)
                .map(|metadata| {
                    !metadata.is_file()
                        || metadata.file_type().is_symlink()
                        || metadata.len() as usize != record.image_len
                })
                .unwrap_or(true)
            || record.image_len > CLIPBOARD_IMAGE_LIMIT
            || !valid_png
        {
            return false;
        }
    }
    true
}

pub(super) fn transfer_history_path(data_dir: &Path) -> PathBuf {
    data_dir.join(TRANSFER_HISTORY_FILE)
}

/// 历史损坏不能阻止节点启动，读取失败按空历史处理。
pub(super) fn load_transfer_history(data_dir: &Path) -> TransferHistoryStore {
    let contents = match std::fs::read(transfer_history_path(data_dir)) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return TransferHistoryStore::default()
        }
        Err(error) => {
            eprintln!("读取传输历史失败：{error}");
            return TransferHistoryStore::default();
        }
    };
    let mut store = match serde_json::from_slice::<TransferHistoryStore>(&contents) {
        Ok(store) => store,
        Err(error) => {
            eprintln!("传输历史文件损坏，已忽略：{error}");
            return TransferHistoryStore::default();
        }
    };
    store
        .entries
        .retain(|entry| is_terminal_transfer(&entry.status));
    store.entries.truncate(TRANSFER_HISTORY_LIMIT);
    store.pending_deletions.retain(|peer_id, ids| {
        if peer_id.is_empty() || peer_id.len() > 256 {
            return false;
        }
        ids.retain(|id| !id.trim().is_empty() && id.len() <= 256);
        ids.dedup();
        ids.truncate(MAX_PENDING_HISTORY_DELETIONS_PER_PEER);
        !ids.is_empty()
    });
    store
}

pub(super) fn persist_transfer_history(
    data_dir: &Path,
    store: &TransferHistoryStore,
) -> anyhow::Result<()> {
    crate::fsutil::atomic_write(
        &transfer_history_path(data_dir),
        &serde_json::to_vec_pretty(store)?,
    )
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
    let referenced = history
        .iter()
        .flat_map(|record| [record.text_path.as_deref(), record.image_path.as_deref()])
        .flatten()
        .collect::<HashSet<_>>();
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
    clipboard_snapshot_hash(&snapshot).ok().map(|_| snapshot)
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
    recorded_at_ms: i64,
    content_hash: String,
) -> anyhow::Result<ClipboardHistoryRecord> {
    let storage_id = Uuid::new_v4().to_string();
    let text_path = match snapshot
        .text
        .as_ref()
        .map(|text| write_clipboard_payload(data_dir, &storage_id, "txt", text.as_bytes()))
        .transpose()
    {
        Ok(path) => path,
        Err(error) => {
            remove_clipboard_payload_set(data_dir, &storage_id);
            return Err(error);
        }
    };
    let image_bytes = match snapshot
        .image_png
        .as_deref()
        .map(|value| BASE64.decode(value))
        .transpose()
    {
        Ok(bytes) => bytes,
        Err(error) => {
            remove_clipboard_payload_set(data_dir, &storage_id);
            return Err(error.into());
        }
    };
    let image_path = match image_bytes
        .as_deref()
        .map(|bytes| write_clipboard_payload(data_dir, &storage_id, "png", bytes))
        .transpose()
    {
        Ok(path) => path,
        Err(error) => {
            remove_clipboard_payload_set(data_dir, &storage_id);
            return Err(error);
        }
    };
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
        image_len: image_bytes.map_or(0, |bytes| bytes.len()),
    })
}

pub(super) fn remove_clipboard_payload_set(data_dir: &Path, storage_id: &str) {
    let directory = clipboard_payload_dir(data_dir);
    for extension in ["txt", "png"] {
        let _ = std::fs::remove_file(directory.join(format!("{storage_id}.{extension}")));
    }
    for extension in ["txt", "png"] {
        remove_clipboard_temp_file(&crate::fsutil::temp_path(
            &directory.join(format!("{storage_id}.{extension}")),
        ));
    }
}

pub(super) fn load_history_snapshot(
    data_dir: &Path,
    record: &ClipboardHistoryRecord,
) -> anyhow::Result<ClipboardSnapshot> {
    let directory = clipboard_payload_dir(data_dir);
    let text = record
        .text_path
        .as_deref()
        .map(|path| std::fs::read(directory.join(path)))
        .transpose()?
        .map(|bytes| String::from_utf8(bytes).map_err(anyhow::Error::from))
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
}

pub(super) fn clipboard_snapshot_hash(snapshot: &ClipboardSnapshot) -> anyhow::Result<String> {
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
    Ok(crate::clipboard::payload_hash(
        snapshot.text.as_deref(),
        image.as_deref(),
    ))
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
        ClipboardHistoryMode::Count => history.truncate(
            config
                .clipboard_history_count
                .clamp(1, MAX_CLIPBOARD_HISTORY_COUNT),
        ),
        ClipboardHistoryMode::Days => {
            let age = i64::from(
                config
                    .clipboard_history_days
                    .clamp(1, MAX_CLIPBOARD_HISTORY_DAYS),
            )
            .saturating_mul(CLIPBOARD_DAY_MS);
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
