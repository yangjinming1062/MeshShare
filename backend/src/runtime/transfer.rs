use super::*;

#[derive(Debug, Clone)]
pub struct TransferRecord {
    pub offer: TransferOffer,
    pub progress: TransferProgress,
    pub accepted_destination: Option<PathBuf>,
    pub received_items: HashSet<String>,
    /// 记录首个已提交项目的目录和顶层名称，定位时须与当前接收目录相符。
    pub(super) received_location: Option<(PathBuf, String)>,
    /// 创建时快照的对端显示名。对端随后离线时历史仍能显示设备名。
    pub peer_name: Option<String>,
    /// 发送方向用户选择的根路径；接收与暂存发送为空。
    pub source_paths: Vec<String>,
    /// 已提交条目的累计字节；进度展示在此基础上叠加当前条目的在途字节。
    pub committed_bytes: u64,
    /// 当前条目已接收但尚未提交的字节，条目提交后清空。
    pub upload_in_flight: Option<(String, u64)>,
    pub updated_at_ms: i64,
}

impl NodeRuntime {
    pub fn add_transfer(&self, offer: TransferOffer) -> bool {
        let _update_gate = self.transfer_update_gate.lock();
        if self.update_installing.load(Ordering::SeqCst) {
            return false;
        }
        let mut progress = transfer_progress(&offer, "receive");
        let accepted_destination = self.default_receive_directory();
        if let Some(destination) = accepted_destination.as_ref() {
            progress.destination = Some(crate::fsutil::display_absolute_path(destination));
            progress.status = TransferStatus::Accepted;
        }
        let mut transfers = self.transfers.write();
        if transfers.contains_key(&offer.transfer_id) {
            return false;
        }
        if !evict_terminal_transfer_if_needed(&mut transfers) {
            return false;
        }
        transfers.insert(
            offer.transfer_id.clone(),
            TransferRecord {
                offer: offer.clone(),
                progress: progress.clone(),
                accepted_destination,
                received_items: HashSet::new(),
                received_location: None,
                peer_name: Some(offer.source_name.clone()),
                source_paths: Vec::new(),
                committed_bytes: 0,
                upload_in_flight: None,
                updated_at_ms: now_millis(),
            },
        );
        drop(transfers);
        // 进度事件同时携带提议。先发布进度，
        // 使自动接受的传输不会在
        // 分时收到两个事件的 UI 中短暂显示为新的确认请求。
        self.publish_event(WireMessage::TransferProgress { progress });
        self.publish_event(WireMessage::TransferOffer { offer });
        true
    }

    /// WebSocket 与 HTTP 确认在同一把锁内对照当前代次、状态和清单，避免晚到的消息重新打开终态。
    pub(crate) fn apply_peer_progress(
        &self,
        peer: &str,
        mut next: TransferProgress,
        acknowledged_item: Option<&str>,
    ) -> bool {
        let _gate = self.transfer_update_gate.lock();
        if is_active_transfer(&next.status) && self.update_installing.load(Ordering::SeqCst) {
            return false;
        }
        let mut transfers = self.transfers.write();
        let Some(record) = transfers.get_mut(&next.transfer_id) else {
            return false;
        };
        if (record.offer.source_node_id != peer && record.offer.target_node_id != peer)
            || !valid_progress_transition(&record.progress, &next)
        {
            return false;
        }
        if let Some(id) = acknowledged_item {
            if record.offer.target_node_id != peer
                || !matches!(
                    next.status,
                    TransferStatus::Uploading | TransferStatus::Completed
                )
                || next.completed_items == 0
                || next.retry_count != record.progress.retry_count
            {
                return false;
            }
            record.received_items.insert(id.to_string());
            record.committed_bytes = record.committed_bytes.max(next.completed_bytes);
            record.upload_in_flight = None;
        }
        let was_terminal = is_terminal_transfer(&record.progress.status);
        next.offer = Some(record.offer.clone());
        next.direction = record.progress.direction.clone();
        next.awaiting_offer_reply = false;
        record.progress = next;
        record.updated_at_ms = now_millis();
        match (was_terminal, is_terminal_transfer(&record.progress.status)) {
            (false, true) => self.record_terminal_transfer(record),
            (true, false) => self.remove_transfer_history_entry(&record.progress.transfer_id),
            _ => {}
        }
        self.publish_event(WireMessage::TransferProgress {
            progress: record.progress.clone(),
        });
        true
    }

    pub fn transfer(&self, transfer_id: &str) -> Option<TransferRecord> {
        self.transfers.read().get(transfer_id).cloned()
    }

    pub fn set_transfer_acceptance(
        &self,
        transfer_id: &str,
        destination: PathBuf,
    ) -> Option<TransferProgress> {
        let mut transfers = self.transfers.write();
        let record = transfers.get_mut(transfer_id)?;
        if !matches!(record.progress.status, TransferStatus::Waiting) {
            return None;
        }
        record.accepted_destination = Some(destination.clone());
        record.updated_at_ms = now_millis();
        record.progress.destination = Some(crate::fsutil::display_absolute_path(&destination));
        record.progress.status = TransferStatus::Accepted;
        record.progress.offer = Some(record.offer.clone());
        Some(record.progress.clone())
    }

    /// digest 是已提交文件内容的摘要，回填到清单后下载校验以它为准。
    pub fn mark_item_received(
        &self,
        transfer_id: &str,
        item_id: &str,
        bytes: u64,
        digest: String,
    ) -> Option<TransferProgress> {
        let mut transfers = self.transfers.write();
        let record = transfers.get_mut(transfer_id)?;
        if matches!(
            record.progress.status,
            TransferStatus::Completed
                | TransferStatus::Ready
                | TransferStatus::Rejected
                | TransferStatus::Failed
                | TransferStatus::Cancelled
        ) {
            return None;
        }
        let item = record
            .offer
            .items
            .iter_mut()
            .find(|item| item.item_id == item_id)?;
        if item.size != bytes {
            return None;
        }
        item.sha256 = digest;
        if record.received_items.insert(item_id.to_string()) {
            record.committed_bytes = record.committed_bytes.saturating_add(bytes);
            record.progress.completed_items = record.progress.completed_items.saturating_add(1);
            let directory = record.accepted_destination.as_ref();
            let saved_directory = record
                .received_location
                .as_ref()
                .map(|(directory, _)| directory);
            if directory != saved_directory {
                record.received_location = directory.and_then(|directory| {
                    let std::path::Component::Normal(name) =
                        Path::new(&item.relative_path).components().next()?
                    else {
                        return None;
                    };
                    Some((directory.clone(), name.to_string_lossy().into_owned()))
                });
            }
        }
        record.upload_in_flight = None;
        record.updated_at_ms = now_millis();
        if record.progress.completed_items >= record.progress.total_items {
            record.progress.status = TransferStatus::Completed;
        } else {
            record.progress.status = TransferStatus::Uploading;
        }
        record.progress.completed_bytes = record
            .progress
            .completed_bytes
            .max(record.committed_bytes.min(record.offer.total_bytes));
        record.progress.offer = Some(record.offer.clone());
        if record.progress.status == TransferStatus::Completed {
            self.record_terminal_transfer(record);
        }
        Some(record.progress.clone())
    }

    /// 已落盘文件校验通过后，重复 PUT 也表示本代上传已经开始。
    pub(crate) fn confirm_received_item(
        &self,
        transfer_id: &str,
        item_id: &str,
        generation: u32,
    ) -> Option<TransferProgress> {
        let mut transfers = self.transfers.write();
        let record = transfers.get_mut(transfer_id)?;
        if record.progress.retry_count != generation || !record.received_items.contains(item_id) {
            return None;
        }
        match record.progress.status {
            TransferStatus::Accepted => record.progress.status = TransferStatus::Uploading,
            TransferStatus::Uploading | TransferStatus::Completed => {}
            _ => return None,
        }
        record.updated_at_ms = now_millis();
        Some(record.progress.clone())
    }

    pub fn fail_transfer(
        &self,
        transfer_id: &str,
        message: String,
        status: TransferStatus,
    ) -> Option<TransferProgress> {
        self.fail_transfer_at_generation(transfer_id, message, status, None)
    }

    pub(crate) fn fail_transfer_at_generation(
        &self,
        transfer_id: &str,
        message: String,
        status: TransferStatus,
        expected: Option<u32>,
    ) -> Option<TransferProgress> {
        let mut transfers = self.transfers.write();
        let record = transfers.get_mut(transfer_id)?;
        if expected.is_some_and(|generation| generation != record.progress.retry_count) {
            return None;
        }
        if matches!(
            record.progress.status,
            TransferStatus::Completed
                | TransferStatus::Ready
                | TransferStatus::Rejected
                | TransferStatus::Failed
                | TransferStatus::Cancelled
        ) {
            return Some(record.progress.clone());
        }
        record.progress.status = status;
        record.progress.awaiting_offer_reply = false;
        record.progress.error = Some(message);
        record.upload_in_flight = None;
        record.updated_at_ms = now_millis();
        record.progress.offer = Some(record.offer.clone());
        self.record_terminal_transfer(record);
        Some(record.progress.clone())
    }

    pub fn retry_transfer(
        &self,
        transfer_id: &str,
        status: TransferStatus,
    ) -> Result<TransferProgress, TransferRetryError> {
        if !matches!(status, TransferStatus::Waiting | TransferStatus::Accepted) {
            return Err(TransferRetryError::NotRetryable);
        }
        let _update_gate = self.transfer_update_gate.lock();
        if self.update_installing.load(Ordering::SeqCst) {
            return Err(TransferRetryError::InstallingUpdate);
        }
        let mut transfers = self.transfers.write();
        let record = transfers
            .get_mut(transfer_id)
            .ok_or(TransferRetryError::NotRetryable)?;
        if record.progress.status != TransferStatus::Failed {
            return Err(TransferRetryError::NotRetryable);
        }
        let retry_count = record
            .progress
            .retry_count
            .checked_add(1)
            .ok_or(TransferRetryError::NotRetryable)?;
        if status == TransferStatus::Waiting {
            record.accepted_destination = None;
            record.progress.destination = None;
        }
        record.progress.retry_count = retry_count;
        record.progress.status = status;
        record.progress.awaiting_offer_reply = false;
        record.progress.error = None;
        record.upload_in_flight = None;
        record.updated_at_ms = now_millis();
        record.progress.offer = Some(record.offer.clone());
        self.remove_transfer_history_entry(transfer_id);
        Ok(record.progress.clone())
    }

    /// 删除失败的传输记录。未失败的记录会保留并拒绝删除，
    /// 使活动任务不会被直接丢弃而不先取消。
    pub fn remove_failed_transfer(&self, transfer_id: &str) -> Option<TransferProgress> {
        let mut transfers = self.transfers.write();
        if transfers.get(transfer_id)?.progress.status != TransferStatus::Failed {
            return None;
        }
        transfers.remove(transfer_id).map(|record| record.progress)
    }

    pub fn set_transfer_source_paths(&self, transfer_id: &str, paths: Vec<String>) {
        if let Some(record) = self.transfers.write().get_mut(transfer_id) {
            record.source_paths = paths;
        }
    }
}

impl NodeRuntime {
    pub fn transfers_snapshot(&self) -> Vec<TransferProgress> {
        self.prune_transfers();
        let mut transfers = self
            .transfers
            .read()
            .values()
            .map(|record| record.progress.clone())
            .collect::<Vec<_>>();
        transfers.sort_by_key(|progress| progress.transfer_id.clone());
        transfers
    }

    pub fn prune_transfers(&self) {
        let now = now_millis();
        let mut transfers = self.transfers.write();
        let mut stale = Vec::new();
        for record in transfers.values_mut() {
            if is_terminal_transfer(&record.progress.status) {
                continue;
            }
            // 已接受却没有开始上传说明发送端没有接上，用更短的窗口暴露问题。
            let accepted = record.progress.status == TransferStatus::Accepted;
            let idle_timeout = if accepted {
                TRANSFER_ACCEPTED_IDLE_TIMEOUT_MS
            } else {
                TRANSFER_ACTIVITY_TIMEOUT_MS
            };
            if now.saturating_sub(record.updated_at_ms) > idle_timeout {
                record.progress.status = TransferStatus::Failed;
                record.progress.awaiting_offer_reply = false;
                record.progress.error = Some(
                    if accepted {
                        "接收端已确认，但发送端没有开始上传，已自动失败"
                    } else {
                        "传输长时间没有进展，已自动失败"
                    }
                    .to_string(),
                );
                record.progress.offer = Some(record.offer.clone());
                record.updated_at_ms = now;
                self.record_terminal_transfer(record);
                stale.push((
                    record.progress.clone(),
                    record.offer.source_node_id.clone(),
                    record.offer.target_node_id.clone(),
                ));
            }
        }
        transfers.retain(|_, record| {
            !is_terminal_transfer(&record.progress.status)
                || now.saturating_sub(record.updated_at_ms)
                    <= if record.progress.status == TransferStatus::Failed {
                        FAILED_TRANSFER_RETENTION_MS
                    } else {
                        TRANSFER_RETENTION_MS
                    }
        });
        let mut terminal_ids = transfers
            .iter()
            .filter(|(_, record)| is_terminal_transfer(&record.progress.status))
            .map(|(id, record)| (id.clone(), record.updated_at_ms))
            .collect::<Vec<_>>();
        terminal_ids.sort_by_key(|(_, created_at)| *created_at);
        let remove_count = terminal_ids.len().saturating_sub(MAX_TERMINAL_TRANSFERS);
        for (transfer_id, _) in terminal_ids.into_iter().take(remove_count) {
            transfers.remove(&transfer_id);
        }
        let mut failed_ids = transfers
            .iter()
            .filter(|(_, record)| record.progress.status == TransferStatus::Failed)
            .map(|(id, record)| (id.clone(), record.updated_at_ms))
            .collect::<Vec<_>>();
        failed_ids.sort_by_key(|(_, updated_at)| *updated_at);
        let failed_remove_count = failed_ids.len().saturating_sub(MAX_FAILED_TRANSFERS);
        for (transfer_id, _) in failed_ids.into_iter().take(failed_remove_count) {
            transfers.remove(&transfer_id);
        }
        drop(transfers);
        for (progress, source, target) in stale {
            let peer = if target == self.node_id() {
                source
            } else {
                target
            };
            if peer != self.node_id() {
                let _ = self.send_to_peer(
                    &peer,
                    WireMessage::TransferAction {
                        transfer_id: progress.transfer_id.clone(),
                        action: crate::model::TransferAction::Fail,
                        reason: progress.error.clone(),
                        retry_count: progress.retry_count,
                    },
                );
            }
            self.publish_event(WireMessage::TransferProgress { progress });
        }
    }

    pub fn transfer_source_id(&self, transfer_id: &str) -> Option<String> {
        self.transfers
            .read()
            .get(transfer_id)
            .map(|record| record.offer.source_node_id.clone())
    }

    pub fn transfer_peer_ids(&self, transfer_id: &str) -> Option<(String, String)> {
        self.transfers.read().get(transfer_id).map(|record| {
            (
                record.offer.source_node_id.clone(),
                record.offer.target_node_id.clone(),
            )
        })
    }

    pub fn mark_transfer_accepted(
        &self,
        transfer_id: &str,
        generation: u32,
    ) -> Option<TransferProgress> {
        let mut transfers = self.transfers.write();
        let record = transfers.get_mut(transfer_id)?;
        if generation != record.progress.retry_count {
            return None;
        }
        if !matches!(record.progress.status, TransferStatus::Waiting) {
            return Some(record.progress.clone());
        }
        record.progress.status = TransferStatus::Accepted;
        record.progress.awaiting_offer_reply = false;
        record.updated_at_ms = now_millis();
        record.progress.offer = Some(record.offer.clone());
        Some(record.progress.clone())
    }

    pub fn try_lock_transfer_item(&self, transfer_id: &str, item_id: &str) -> bool {
        self.transfer_locks
            .lock()
            .insert((transfer_id.to_string(), item_id.to_string()))
    }

    pub fn unlock_transfer_item(&self, transfer_id: &str, item_id: &str) {
        self.transfer_locks
            .lock()
            .remove(&(transfer_id.to_string(), item_id.to_string()));
    }

    pub fn register_desktop_temp_file(&self, path: &Path) -> anyhow::Result<()> {
        if !is_desktop_temp_file_path(path) {
            return Err(anyhow::anyhow!("桌面临时文件路径无效"));
        }
        let mut files = self.desktop_temp_files.lock();
        if files.contains(path) {
            return Ok(());
        }
        if files.len() >= MAX_DESKTOP_TEMP_FILES {
            return Err(anyhow::anyhow!("桌面临时文件登记数量已达到上限"));
        }
        let mut next = files.clone();
        next.insert(path.to_path_buf());
        persist_desktop_temp_files(&self.data_dir, &next)?;
        *files = next;
        Ok(())
    }

    pub fn unregister_desktop_temp_file(&self, path: &Path) {
        let mut files = self.desktop_temp_files.lock();
        if !files.remove(path) {
            return;
        }
        if let Err(error) = persist_desktop_temp_files(&self.data_dir, &files) {
            eprintln!("保存桌面临时文件登记失败：{error}");
        }
    }

    pub fn transfer_upload_active(&self, transfer_id: &str) -> bool {
        self.transfers
            .read()
            .get(transfer_id)
            .is_some_and(|record| {
                matches!(
                    record.progress.status,
                    TransferStatus::Accepted | TransferStatus::Uploading
                )
            })
    }

    pub fn begin_transfer_upload(&self, transfer_id: &str) -> Option<TransferProgress> {
        let mut transfers = self.transfers.write();
        let record = transfers.get_mut(transfer_id)?;
        if !matches!(
            record.progress.status,
            TransferStatus::Accepted | TransferStatus::Uploading
        ) {
            return None;
        }
        record.progress.status = TransferStatus::Uploading;
        record.updated_at_ms = now_millis();
        record.progress.offer = Some(record.offer.clone());
        Some(record.progress.clone())
    }

    /// 在条目提交前记录当前已接收字节并生成对外进度。
    ///
    /// `completed_bytes` 取已提交字节与在途字节的高水位，保证进度只增不减
    /// 且不超过总量；`completed_items` 仍只统计已提交条目。
    pub fn report_transfer_progress(
        &self,
        transfer_id: &str,
        item_id: &str,
        received_bytes: u64,
    ) -> Option<TransferProgress> {
        let mut transfers = self.transfers.write();
        let record = transfers.get_mut(transfer_id)?;
        if !matches!(
            record.progress.status,
            TransferStatus::Accepted | TransferStatus::Uploading
        ) || record.received_items.contains(item_id)
        {
            return None;
        }
        let item_size = record
            .offer
            .items
            .iter()
            .find(|item| item.item_id == item_id)?
            .size;
        let received = received_bytes.min(item_size);
        let received = match &record.upload_in_flight {
            Some((current, bytes)) if current == item_id => (*bytes).max(received),
            _ => received,
        };
        record.upload_in_flight = Some((item_id.to_string(), received));
        let displayed = record
            .committed_bytes
            .saturating_add(received)
            .min(record.offer.total_bytes);
        record.progress.completed_bytes = record.progress.completed_bytes.max(displayed);
        record.updated_at_ms = now_millis();
        record.progress.offer = Some(record.offer.clone());
        Some(record.progress.clone())
    }

    pub fn touch_transfer_activity(&self, transfer_id: &str) -> bool {
        let mut transfers = self.transfers.write();
        let Some(record) = transfers.get_mut(transfer_id) else {
            return false;
        };
        if !matches!(
            record.progress.status,
            TransferStatus::Accepted | TransferStatus::Uploading
        ) {
            return false;
        }
        record.updated_at_ms = now_millis();
        true
    }
}

fn valid_progress_transition(current: &TransferProgress, next: &TransferProgress) -> bool {
    use TransferStatus::*;
    if next.total_bytes != current.total_bytes
        || next.total_items != current.total_items
        || !(current.completed_bytes..=current.total_bytes).contains(&next.completed_bytes)
        || !(current.completed_items..=current.total_items).contains(&next.completed_items)
        || (matches!(next.status, Completed | Ready)
            && (next.completed_bytes != next.total_bytes
                || next.completed_items != next.total_items))
        || next
            .error
            .as_ref()
            .is_some_and(|error| error.len() > crate::model::MAX_WIRE_ERROR_LENGTH)
        || next
            .destination
            .as_ref()
            .is_some_and(|path| path.len() > 4096)
        || next
            .direction
            .as_deref()
            .is_some_and(|direction| !matches!(direction, "send" | "receive"))
    {
        return false;
    }
    if current.retry_count != next.retry_count {
        return current.status == Failed
            && current.retry_count.checked_add(1) == Some(next.retry_count)
            && matches!(next.status, Waiting | Accepted | Uploading);
    }
    match current.status {
        Waiting => true,
        Accepted => next.status != Waiting,
        Uploading => matches!(
            next.status,
            Uploading | Completed | Ready | Failed | Cancelled
        ),
        Ready => matches!(next.status, Ready | Completed | Cancelled),
        Rejected | Failed | Cancelled | Completed => next.status == current.status,
    }
}

pub(super) fn evict_terminal_transfer_if_needed(
    transfers: &mut HashMap<String, TransferRecord>,
) -> bool {
    if transfers.len() < MAX_TRANSFER_RECORDS {
        return true;
    }
    let terminal_id = transfers
        .iter()
        .filter(|(_, record)| {
            matches!(
                record.progress.status,
                TransferStatus::Completed
                    | TransferStatus::Rejected
                    | TransferStatus::Failed
                    | TransferStatus::Cancelled
            )
        })
        .min_by_key(|(_, record)| record.updated_at_ms)
        .map(|(transfer_id, _)| transfer_id.clone());
    if let Some(transfer_id) = terminal_id {
        transfers.remove(&transfer_id);
        true
    } else {
        false
    }
}
