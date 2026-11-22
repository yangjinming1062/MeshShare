use super::*;

#[derive(Debug, Clone)]
pub struct MobileTransferRecord {
    pub session_id: String,
    pub offer: TransferOffer,
    pub progress: TransferProgress,
    pub staged_directory: PathBuf,
    pub staged_items: HashSet<String>,
    pub downloaded_items: HashSet<String>,
    pub retryable_downloads: HashSet<String>,
    /// 创建时算好的清单序列化字节数，用于全局清单内存预算。
    pub manifest_bytes: u64,
    pub updated_at_ms: i64,
}

fn mobile_view(record: &MobileTransferRecord) -> MobileTransferView {
    let mut downloaded_item_ids = record.downloaded_items.iter().cloned().collect::<Vec<_>>();
    downloaded_item_ids.sort();
    let mut retryable_download_item_ids = record
        .retryable_downloads
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    retryable_download_item_ids.sort();
    MobileTransferView {
        session_id: record.session_id.clone(),
        progress: record.progress.clone(),
        downloaded_item_ids,
        retryable_download_item_ids,
    }
}

/// 清单在全局内存预算里的占用；创建传输时算一次并记录在案。
fn mobile_manifest_size(offer: &TransferOffer) -> u64 {
    serde_json::to_vec(offer)
        .map(|value| value.len() as u64)
        .unwrap_or_default()
}

impl NodeRuntime {
    fn mobile_staging_root(&self) -> PathBuf {
        self.data_dir.join("mobile-transfers")
    }

    /// 只返回暂存根目录的直接子目录。
    ///
    /// 路径保存在内存记录中，因此该检查刻意只做词法判断，
    /// 拒绝根目录本身、父级组件以及嵌套路径。
    /// 它是清理调用方执行递归删除之前的最后一道守卫。
    fn is_mobile_staging_directory(&self, directory: &Path) -> bool {
        let Ok(relative) = directory.strip_prefix(self.mobile_staging_root()) else {
            return false;
        };
        let mut components = relative.components();
        matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
    }

    pub fn create_mobile_session(&self, label: String) -> Result<MobileSession, String> {
        let now = now_millis();
        self.prune_expired_mobile_sessions_at(now);
        let mut sessions = self.mobile_sessions.write();
        if sessions.len() >= MAX_MOBILE_SESSIONS {
            return Err("手机会话数量已达到上限".to_string());
        }
        let session = MobileSession {
            session_id: Uuid::new_v4().to_string(),
            node_id: self.node_id(),
            node_name: self.node_info().node_name,
            label,
            created_at_ms: now,
            last_seen_ms: now,
        };
        sessions.insert(session.session_id.clone(), session.clone());
        Ok(session)
    }

    pub fn touch_mobile_session(&self, session_id: &str) -> Option<MobileSession> {
        let now = now_millis();
        self.prune_expired_mobile_sessions_at(now);
        let mut sessions = self.mobile_sessions.write();
        let session = sessions.get_mut(session_id)?;
        session.last_seen_ms = now;
        Some(session.clone())
    }

    pub fn prune_expired_mobile_sessions(&self) {
        self.prune_expired_mobile_sessions_at(now_millis());
    }

    fn prune_expired_mobile_sessions_at(&self, now: i64) {
        self.mobile_sessions
            .write()
            .retain(|_, session| now.saturating_sub(session.last_seen_ms) <= MOBILE_SESSION_TTL_MS);
    }

    pub fn mobile_sessions_snapshot(&self) -> Vec<MobileSession> {
        self.prune_expired_mobile_sessions();
        let sessions = self.mobile_sessions.read();
        let mut result = sessions.values().cloned().collect::<Vec<_>>();
        result.sort_by_key(|session| session.created_at_ms);
        result
    }

    pub fn create_mobile_transfer(
        &self,
        session_id: &str,
        offer: TransferOffer,
    ) -> Result<MobileTransferView, String> {
        if self.touch_mobile_session(session_id).is_none() {
            return Err("手机会话已过期".to_string());
        }
        let _update_gate = self.transfer_update_gate.lock();
        if self.update_installing.load(Ordering::SeqCst) {
            return Err(UPDATE_INSTALLING_MESSAGE.to_string());
        }
        let manifest_bytes = mobile_manifest_size(&offer);
        if self
            .mobile_manifest_bytes
            .load(Ordering::SeqCst)
            .checked_add(manifest_bytes)
            .is_none_or(|total| total > MAX_MOBILE_MANIFEST_BYTES)
        {
            return Err("手机传输清单资源暂时不足，请稍后重试".to_string());
        }
        let mut transfers = self.mobile_transfers.write();
        if transfers
            .values()
            .any(|record| record.offer.transfer_id == offer.transfer_id)
        {
            return Err("手机传输任务已存在".to_string());
        }
        if transfers.len() >= MAX_MOBILE_TRANSFER_RECORDS {
            return Err("手机传输记录资源暂时不足，请稍后重试".to_string());
        }
        let progress = transfer_progress(&offer, "receive");
        // `transfer_id` 由对端提供，也会出现在 HTTP API 中。
        // 它只作为协议键使用，绝不作为路径组件：
        // 新生成的 UUID 给每个传输一个不透明的暂存目录，
        // 不存在路径穿越或与根目录重名的风险。
        let staged_directory = self.mobile_staging_root().join(Uuid::new_v4().to_string());
        let record = MobileTransferRecord {
            session_id: session_id.to_string(),
            offer,
            progress,
            staged_directory,
            staged_items: HashSet::new(),
            downloaded_items: HashSet::new(),
            retryable_downloads: HashSet::new(),
            manifest_bytes,
            updated_at_ms: now_millis(),
        };
        let view = mobile_view(&record);
        transfers.insert(record.offer.transfer_id.clone(), record);
        self.mobile_manifest_bytes
            .fetch_add(manifest_bytes, Ordering::SeqCst);
        Ok(view)
    }

    pub fn mobile_transfers_snapshot(&self, session_id: Option<&str>) -> Vec<MobileTransferView> {
        let mut transfers = self
            .mobile_transfers
            .read()
            .values()
            .filter(|record| session_id.is_none_or(|id| id == record.session_id))
            .map(mobile_view)
            .collect::<Vec<_>>();
        transfers.sort_by_key(|view| view.progress.transfer_id.clone());
        transfers
    }

    pub fn touch_mobile_session_transfers(&self, session_id: &str) {
        let now = now_millis();
        let mut transfers = self.mobile_transfers.write();
        for record in transfers.values_mut() {
            if record.session_id == session_id
                && matches!(
                    record.progress.status,
                    TransferStatus::Waiting
                        | TransferStatus::Accepted
                        | TransferStatus::Uploading
                        | TransferStatus::Ready
                )
            {
                record.updated_at_ms = now;
            }
        }
    }

    pub fn touch_mobile_transfer_activity(&self, transfer_id: &str) -> bool {
        let mut transfers = self.mobile_transfers.write();
        let Some(record) = transfers.get_mut(transfer_id) else {
            return false;
        };
        let downloading = matches!(
            record.progress.status,
            TransferStatus::Accepted | TransferStatus::Uploading | TransferStatus::Ready
        ) || (record.progress.status == TransferStatus::Completed
            && !record.retryable_downloads.is_empty());
        if !downloading {
            return false;
        }
        record.updated_at_ms = now_millis();
        true
    }

    pub fn prune_mobile_transfers(&self) -> Vec<PathBuf> {
        let sessions = self.mobile_sessions.read();
        let mut transfers = self.mobile_transfers.write();
        let now = now_millis();
        let failed_snapshots = transfers
            .values_mut()
            .filter(|record| {
                sessions.contains_key(&record.session_id)
                    && !is_terminal_transfer(&record.progress.status)
                    && now.saturating_sub(record.updated_at_ms) > TRANSFER_ACTIVITY_TIMEOUT_MS
            })
            .map(|record| {
                record.progress.status = TransferStatus::Failed;
                record.progress.error = Some("手机传输长时间没有进展，已自动失败".to_string());
                record.updated_at_ms = now;
                record.clone()
            })
            .collect::<Vec<_>>();
        let mut cleanup = failed_snapshots
            .iter()
            .map(|record| record.staged_directory.clone())
            .collect::<Vec<_>>();
        let mut remove = transfers
            .iter()
            .filter(|(_, record)| {
                (!sessions.contains_key(&record.session_id)
                    && record.progress.status != TransferStatus::Failed)
                    || (is_terminal_transfer(&record.progress.status)
                        && now.saturating_sub(record.updated_at_ms)
                            > if record.progress.status == TransferStatus::Failed {
                                FAILED_TRANSFER_RETENTION_MS
                            } else {
                                TRANSFER_RETENTION_MS
                            })
            })
            .map(|(transfer_id, record)| (transfer_id.clone(), record.staged_directory.clone()))
            .collect::<Vec<_>>();
        let mut terminal_by_session = HashMap::<String, Vec<(String, i64, PathBuf)>>::new();
        for (transfer_id, record) in transfers.iter() {
            if sessions.contains_key(&record.session_id)
                && is_terminal_transfer(&record.progress.status)
            {
                terminal_by_session
                    .entry(record.session_id.clone())
                    .or_default()
                    .push((
                        transfer_id.clone(),
                        record.updated_at_ms,
                        record.staged_directory.clone(),
                    ));
            }
        }
        for records in terminal_by_session.values_mut() {
            records.sort_by_key(|(_, created_at, _)| *created_at);
            let count = records
                .len()
                .saturating_sub(MAX_MOBILE_TERMINAL_TRANSFERS_PER_SESSION);
            remove.extend(
                records
                    .iter()
                    .take(count)
                    .map(|(transfer_id, _, directory)| (transfer_id.clone(), directory.clone())),
            );
            records.retain(|(transfer_id, _, _)| {
                transfers
                    .get(transfer_id)
                    .is_some_and(|record| record.progress.status == TransferStatus::Failed)
            });
            let count = records
                .len()
                .saturating_sub(MAX_MOBILE_FAILED_TRANSFERS_PER_SESSION);
            remove.extend(
                records
                    .iter()
                    .take(count)
                    .map(|(transfer_id, _, directory)| (transfer_id.clone(), directory.clone())),
            );
        }
        let mut failed_ids = transfers
            .iter()
            .filter(|(_, record)| record.progress.status == TransferStatus::Failed)
            .map(|(transfer_id, record)| {
                (
                    transfer_id.clone(),
                    record.updated_at_ms,
                    record.staged_directory.clone(),
                )
            })
            .collect::<Vec<_>>();
        failed_ids.sort_by_key(|(_, updated_at, _)| *updated_at);
        let failed_remove_count = failed_ids.len().saturating_sub(MAX_MOBILE_FAILED_TRANSFERS);
        remove.extend(
            failed_ids
                .into_iter()
                .take(failed_remove_count)
                .map(|(transfer_id, _, directory)| (transfer_id, directory)),
        );
        remove.sort_by(|left, right| left.0.cmp(&right.0));
        remove.dedup_by(|left, right| left.0 == right.0);
        let mut released_manifest_bytes = 0u64;
        for (transfer_id, _) in &remove {
            if let Some(record) = transfers.remove(transfer_id) {
                released_manifest_bytes =
                    released_manifest_bytes.saturating_add(record.manifest_bytes);
            }
        }
        if released_manifest_bytes > 0 {
            self.mobile_manifest_bytes
                .fetch_sub(released_manifest_bytes, Ordering::SeqCst);
        }
        cleanup.extend(remove.into_iter().map(|(_, directory)| directory));
        drop(transfers);
        drop(sessions);
        for record in failed_snapshots {
            self.record_terminal_mobile_transfer(&record);
        }
        cleanup
            .into_iter()
            .filter_map(|directory| {
                self.is_mobile_staging_directory(&directory)
                    .then_some(directory)
            })
            .collect()
    }

    pub fn mobile_transfer(&self, transfer_id: &str) -> Option<MobileTransferRecord> {
        self.mobile_transfers.read().get(transfer_id).cloned()
    }

    /// 删除失败的移动端传输记录，并释放其清单预算。
    pub fn remove_failed_mobile_transfer(&self, transfer_id: &str) -> Option<MobileTransferRecord> {
        let mut transfers = self.mobile_transfers.write();
        if transfers.get(transfer_id)?.progress.status != TransferStatus::Failed {
            return None;
        }
        let record = transfers.remove(transfer_id)?;
        if record.manifest_bytes > 0 {
            self.mobile_manifest_bytes
                .fetch_sub(record.manifest_bytes, Ordering::SeqCst);
        }
        Some(record)
    }

    pub fn set_mobile_transfer_status(
        &self,
        transfer_id: &str,
        session_id: &str,
        status: TransferStatus,
    ) -> Option<(MobileTransferView, Option<PathBuf>)> {
        let mut transfers = self.mobile_transfers.write();
        let record = transfers.get_mut(transfer_id)?;
        if record.session_id != session_id {
            return None;
        }
        let current = &record.progress.status;
        let reason = match status {
            TransferStatus::Accepted if *current == TransferStatus::Waiting => None,
            TransferStatus::Rejected if *current == TransferStatus::Waiting => {
                Some("手机端拒绝了传输")
            }
            TransferStatus::Cancelled if is_active_transfer(current) => Some("手机传输已取消"),
            _ => return Some((mobile_view(record), None)),
        };
        record.progress.status = status;
        record.updated_at_ms = now_millis();
        record.progress.error = reason.map(str::to_string);
        let view = mobile_view(record);
        let cleanup = if reason.is_some() {
            let snapshot = record.clone();
            drop(transfers);
            self.record_terminal_mobile_transfer(&snapshot);
            Some(snapshot.staged_directory)
        } else {
            None
        };
        Some((view, cleanup))
    }

    pub fn stage_mobile_item(
        &self,
        transfer_id: &str,
        item_id: &str,
        bytes: u64,
        digest: String,
    ) -> Option<MobileTransferView> {
        let mut transfers = self.mobile_transfers.write();
        let record = transfers.get_mut(transfer_id)?;
        if !matches!(
            record.progress.status,
            TransferStatus::Accepted | TransferStatus::Uploading
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
        if record.staged_items.insert(item_id.to_string()) {
            record.progress.completed_items = record.progress.completed_items.saturating_add(1);
            record.progress.completed_bytes = record.progress.completed_bytes.saturating_add(bytes);
        }
        record.updated_at_ms = now_millis();
        record.progress.status = if record.progress.completed_items >= record.progress.total_items {
            TransferStatus::Ready
        } else {
            TransferStatus::Uploading
        };
        record.progress.offer = Some(record.offer.clone());
        Some(mobile_view(record))
    }

    pub fn begin_mobile_upload(&self, transfer_id: &str) -> Option<MobileTransferView> {
        let mut transfers = self.mobile_transfers.write();
        let record = transfers.get_mut(transfer_id)?;
        if !matches!(
            record.progress.status,
            TransferStatus::Accepted | TransferStatus::Uploading
        ) {
            return None;
        }
        record.progress.status = TransferStatus::Uploading;
        record.updated_at_ms = now_millis();
        Some(mobile_view(record))
    }

    pub fn mark_mobile_item_downloaded(
        &self,
        transfer_id: &str,
        item_id: &str,
        keep_for_retry: bool,
    ) -> Option<MobileTransferView> {
        let mut transfers = self.mobile_transfers.write();
        let record = transfers.get_mut(transfer_id)?;
        if !matches!(
            record.progress.status,
            TransferStatus::Ready | TransferStatus::Completed
        ) || !record.staged_items.contains(item_id)
        {
            return None;
        }
        let first_download = record.downloaded_items.insert(item_id.to_string());
        if keep_for_retry {
            if first_download {
                record.retryable_downloads.insert(item_id.to_string());
            }
        } else {
            record.retryable_downloads.remove(item_id);
        }
        record.updated_at_ms = now_millis();
        let was_completed = record.progress.status == TransferStatus::Completed;
        if record.downloaded_items.len() >= record.progress.total_items {
            record.progress.status = TransferStatus::Completed;
        }
        let view = mobile_view(record);
        let snapshot = record.clone();
        drop(transfers);
        if view.progress.status == TransferStatus::Completed && !was_completed {
            self.record_terminal_mobile_transfer(&snapshot);
        }
        Some(view)
    }

    pub fn fail_mobile_transfer(
        &self,
        transfer_id: &str,
        message: String,
    ) -> Option<MobileTransferView> {
        let mut transfers = self.mobile_transfers.write();
        let record = transfers.get_mut(transfer_id)?;
        if matches!(
            record.progress.status,
            TransferStatus::Ready
                | TransferStatus::Completed
                | TransferStatus::Rejected
                | TransferStatus::Failed
                | TransferStatus::Cancelled
        ) {
            return Some(mobile_view(record));
        }
        record.progress.status = TransferStatus::Failed;
        record.progress.error = Some(message);
        record.updated_at_ms = now_millis();
        let view = mobile_view(record);
        let snapshot = record.clone();
        drop(transfers);
        self.record_terminal_mobile_transfer(&snapshot);
        Some(view)
    }

    pub fn mobile_item_path(&self, transfer_id: &str, item_id: &str) -> Option<PathBuf> {
        let record = self.mobile_transfers.read().get(transfer_id)?.clone();
        if !record.staged_items.contains(item_id) {
            return None;
        }
        let item = record
            .offer
            .items
            .iter()
            .find(|item| item.item_id == item_id)?;
        Some(record.staged_directory.join(&item.relative_path))
    }

    pub fn mobile_item_already_staged(
        &self,
        transfer_id: &str,
        item_id: &str,
    ) -> Option<MobileTransferView> {
        let record = self.mobile_transfers.read().get(transfer_id)?.clone();
        record
            .staged_items
            .contains(item_id)
            .then(|| mobile_view(&record))
    }
}
