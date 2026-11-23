use super::*;

/// 传输历史与尚未送达对端的同步删除。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(super) struct TransferHistoryStore {
    pub(super) entries: Vec<TransferHistoryEntry>,
    pub(super) pending_deletions: HashMap<String, Vec<String>>,
}

impl NodeRuntime {
    fn history_entry(&self, record: &TransferRecord) -> TransferHistoryEntry {
        let receive = match record.progress.direction.as_deref() {
            Some("receive") => true,
            Some(_) => false,
            None => record.offer().target_node_id == self.node_id(),
        };
        let (direction, peer_node_id, peer_name) = if receive {
            (
                "receive".to_string(),
                record.offer().source_node_id.clone(),
                record
                    .peer_name
                    .clone()
                    .unwrap_or_else(|| record.offer().source_name.clone()),
            )
        } else {
            (
                "send".to_string(),
                record.offer().target_node_id.clone(),
                record
                    .peer_name
                    .clone()
                    .unwrap_or_else(|| short_node_id(&record.offer().target_node_id)),
            )
        };
        let item_root = record
            .received_location
            .as_ref()
            .filter(|(directory, _)| {
                receive && record.accepted_destination.as_ref() == Some(directory)
            })
            .map(|(_, name)| name.clone());
        TransferHistoryEntry {
            transfer_id: record.progress.transfer_id.clone(),
            direction,
            peer_node_id,
            peer_name,
            status: record.progress.status.clone(),
            completed_bytes: record.progress.completed_bytes,
            total_bytes: record.progress.total_bytes,
            completed_items: record.progress.completed_items,
            total_items: record.progress.total_items,
            source_paths: record.source_paths.clone(),
            destination: record.progress.destination.clone(),
            item_root,
            error: record.progress.error.clone(),
            retry_count: record.progress.retry_count,
            session_id: None,
            finished_at_ms: now_millis(),
        }
    }

    fn mobile_history_entry(&self, record: &MobileTransferRecord) -> TransferHistoryEntry {
        let peer_name = self
            .mobile_sessions
            .read()
            .get(&record.session_id)
            .map(|session| session.label.clone())
            .filter(|label| !label.trim().is_empty())
            .unwrap_or_else(|| "网页会话".to_string());
        TransferHistoryEntry {
            transfer_id: record.progress.transfer_id.clone(),
            direction: "send".to_string(),
            peer_node_id: record.session_id.clone(),
            peer_name,
            status: record.progress.status.clone(),
            completed_bytes: record.progress.completed_bytes,
            total_bytes: record.progress.total_bytes,
            completed_items: record.progress.completed_items,
            total_items: record.progress.total_items,
            source_paths: Vec::new(),
            destination: None,
            item_root: None,
            error: record.progress.error.clone(),
            retry_count: record.progress.retry_count,
            session_id: Some(record.session_id.clone()),
            finished_at_ms: now_millis(),
        }
    }

    fn push_transfer_history(&self, entry: TransferHistoryEntry) {
        let mut store = self.transfer_history.lock();
        store
            .entries
            .retain(|item| item.transfer_id != entry.transfer_id);
        store.entries.insert(0, entry);
        store.entries.truncate(TRANSFER_HISTORY_LIMIT);
        if let Err(error) = persist_transfer_history(&self.data_dir, &store) {
            eprintln!("保存传输历史失败：{error}");
        }
    }

    pub(super) fn record_terminal_transfer(&self, record: &TransferRecord) {
        if is_terminal_transfer(&record.progress.status) {
            self.push_transfer_history(self.history_entry(record));
        }
    }

    pub(super) fn record_terminal_mobile_transfer(&self, record: &MobileTransferRecord) {
        if is_terminal_transfer(&record.progress.status) {
            self.push_transfer_history(self.mobile_history_entry(record));
        }
    }

    pub(super) fn remove_transfer_history_entry(&self, transfer_id: &str) {
        let mut store = self.transfer_history.lock();
        let before = store.entries.len();
        store
            .entries
            .retain(|entry| entry.transfer_id != transfer_id);
        if store.entries.len() == before {
            return;
        }
        if let Err(error) = persist_transfer_history(&self.data_dir, &store) {
            eprintln!("保存传输历史失败：{error}");
        }
    }

    pub fn transfer_history_snapshot(&self, session_id: Option<&str>) -> Vec<TransferHistoryEntry> {
        self.transfer_history
            .lock()
            .entries
            .iter()
            .filter(|entry| {
                session_id.is_none_or(|value| entry.session_id.as_deref() == Some(value))
            })
            .cloned()
            .collect()
    }

    /// 删除本机历史与登记待发通知使用同一份索引，重启后仍可继续投递。
    pub fn clear_transfer_history(&self, session_id: Option<&str>) -> anyhow::Result<usize> {
        let self_id = self.node_id();
        let mut store = self.transfer_history.lock();
        let mut next = store.clone();
        let mut peers = HashSet::new();
        next.entries.retain(|entry| {
            if session_id.is_some_and(|id| entry.session_id.as_deref() != Some(id)) {
                return true;
            }
            if entry.session_id.is_none() && entry.peer_node_id != self_id {
                let queue = next
                    .pending_deletions
                    .entry(entry.peer_node_id.clone())
                    .or_default();
                if !queue.contains(&entry.transfer_id) {
                    if queue.len() == MAX_HISTORY_DELETE_BATCH {
                        queue.remove(0);
                        eprintln!("对端的历史删除通知达到上限，已丢弃最早一条");
                    }
                    queue.push(entry.transfer_id.clone());
                }
                peers.insert(entry.peer_node_id.clone());
            }
            false
        });
        let cleared = store.entries.len() - next.entries.len();
        if cleared == 0 {
            return Ok(0);
        }
        persist_transfer_history(&self.data_dir, &next)?;
        *store = next;
        drop(store);
        for peer in peers {
            self.flush_history_deletions(&peer);
        }
        Ok(cleared)
    }

    /// 只删除与发送方配对的记录，网页会话记录不参与同步删除。
    pub fn forget_transfer_history_from_peer(
        &self,
        peer_id: &str,
        transfer_ids: &[String],
    ) -> usize {
        if transfer_ids.is_empty() {
            return 0;
        }
        let mut store = self.transfer_history.lock();
        let previous = store.clone();
        let before = store.entries.len();
        store.entries.retain(|entry| {
            !(entry.session_id.is_none()
                && entry.peer_node_id == peer_id
                && transfer_ids.contains(&entry.transfer_id))
        });
        let removed = before.saturating_sub(store.entries.len());
        if removed == 0 {
            return 0;
        }
        if let Err(error) = persist_transfer_history(&self.data_dir, &store) {
            *store = previous;
            eprintln!("保存传输历史同步删除失败：{error}");
            return 0;
        }
        removed
    }

    pub fn flush_history_deletions(&self, peer_id: &str) -> bool {
        let mut store = self.transfer_history.lock();
        let Some(ids) = store.pending_deletions.get(peer_id) else {
            return false;
        };
        // 已完成 Hello 的节点才有能力声明；离线或队列已满时保留通知。
        let peers = self.peers.read();
        let Some(peer) = peers.get(peer_id) else {
            return false;
        };
        let supported = peer
            .capabilities
            .iter()
            .any(|item| item == TRANSFER_HISTORY_CAPABILITY);
        if supported
            && !self.send_to_peer(
                peer_id,
                WireMessage::TransferHistoryDeleted {
                    transfer_ids: ids.clone(),
                },
            )
        {
            return false;
        }
        drop(peers);
        store.pending_deletions.remove(peer_id);
        if let Err(error) = persist_transfer_history(&self.data_dir, &store) {
            eprintln!("保存传输历史失败：{error}");
        }
        supported
    }
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
        ids.truncate(MAX_HISTORY_DELETE_BATCH);
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
