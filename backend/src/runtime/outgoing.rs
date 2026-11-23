use super::*;
use crate::fsutil::file_modified_ms;
use std::path::{Path, PathBuf};

fn outgoing_media_type(relative_path: &str) -> String {
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

fn push_outgoing_source(
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

/// 把用户选中的文件与文件夹展开成待发送清单，文件夹按选中目录名保留层级。
fn expand_outgoing_paths(paths: &[String]) -> Result<(Vec<PreparedSource>, Vec<String>), String> {
    if paths.is_empty() || paths.len() > OUTGOING_PREPARE_MAX_SELECTION {
        return Err("需要选择 1 到 512 个文件或文件夹".into());
    }
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
            let root = crate::fsutil::display_absolute_path(&canonical);
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
    if sources.is_empty() {
        return Err("选择的内容里没有可发送的文件".into());
    }
    sources.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok((sources, roots))
}

/// 出站任务每一项的源：桌面端直接引用原文件，Android 仍先把字节复制进暂存目录。
#[derive(Debug, Clone)]
enum OutgoingItemSource {
    Staged,
    Original {
        path: PathBuf,
        size: u64,
        modified_ms: Option<i64>,
    },
}

/// 发送端已经接手本次传输：清单完整后就不再需要页面持有源文件。
#[derive(Debug, Clone)]
pub(super) struct StagedOutgoing {
    pub offer: Arc<TransferOffer>,
    pub directory: PathBuf,
    sources: HashMap<String, OutgoingItemSource>,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone)]
struct PreparedSource {
    file_name: String,
    relative_path: String,
    path: PathBuf,
    size: u64,
    modified_ms: Option<i64>,
}

impl PreparedSource {
    fn item(&self, index: usize, sha256: String) -> TransferItem {
        TransferItem {
            item_id: format!("{index}-{}", Uuid::new_v4()),
            name: self.file_name.clone(),
            relative_path: self.relative_path.clone(),
            size: self.size,
            sha256,
            media_type: outgoing_media_type(&self.relative_path),
            directory: false,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ManagedMobilePath {
    pub(crate) item_index: usize,
    pub(crate) item_id: String,
    pub(crate) path: PathBuf,
    size: u64,
    modified_ms: Option<i64>,
}

impl ManagedMobilePath {
    pub(crate) async fn verify_file(&self, file: &tokio::fs::File) -> Result<(), String> {
        let metadata = file.metadata().await.map_err(|e| e.to_string())?;
        if !metadata.is_file()
            || metadata.len() != self.size
            || file_modified_ms(&metadata) != self.modified_ms
        {
            return Err("源文件已变化，请重新发送".into());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum OutgoingPrepareState {
    Preparing,
    Failed(String),
}

#[derive(Debug, Clone)]
pub(super) struct OutgoingPrepare {
    target_node_id: String,
    sources: Vec<PreparedSource>,
    source_roots: Vec<String>,
    total_bytes: u64,
    hashed_bytes: u64,
    items_ready: usize,
    state: OutgoingPrepareState,
    created_at_ms: i64,
}

/// 页面轮询的准备进度。`state` 为 `preparing` 时还在枚举与计算摘要。
#[derive(Debug, Clone, Serialize)]
pub struct OutgoingPrepareView {
    pub transfer_id: String,
    pub state: String,
    pub error: Option<String>,
    pub hashed_bytes: u64,
    pub total_bytes: u64,
    pub items_ready: usize,
    pub total_items: usize,
}

const MAX_CONCURRENT_SOURCE_SCANS: u64 = 8;

struct SourceScan<'a>(&'a AtomicU64);

impl Drop for SourceScan<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl NodeRuntime {
    fn reserve_source_scan(&self) -> Result<SourceScan<'_>, String> {
        let _gate = self.transfer_update_gate.lock();
        if self.update_installing.load(Ordering::SeqCst) {
            return Err(UPDATE_INSTALLING_MESSAGE.to_owned());
        }
        if self.source_scans.load(Ordering::SeqCst) >= MAX_CONCURRENT_SOURCE_SCANS {
            return Err("正在扫描的发送目录过多".to_owned());
        }
        self.source_scans.fetch_add(1, Ordering::SeqCst);
        Ok(SourceScan(&self.source_scans))
    }

    pub(crate) fn offer_mobile_paths(
        &self,
        session_id: &str,
        paths: &[String],
    ) -> Result<MobileTransferView, String> {
        let _scan = self.reserve_source_scan()?;
        let (sources, _) = expand_outgoing_paths(paths)?;
        let mut items = Vec::with_capacity(sources.len());
        let mut originals = Vec::with_capacity(sources.len());
        let mut total_bytes = 0u64;
        for (index, source) in sources.into_iter().enumerate() {
            let item = source.item(index, String::new());
            total_bytes = total_bytes.checked_add(item.size).ok_or("文件总大小溢出")?;
            originals.push(ManagedMobilePath {
                item_index: index,
                item_id: item.item_id.clone(),
                path: source.path,
                size: source.size,
                modified_ms: source.modified_ms,
            });
            items.push(item);
        }
        let node = self.node_info();
        let offer = TransferOffer {
            transfer_id: Uuid::new_v4().to_string(),
            source_node_id: node.node_id.clone(),
            target_node_id: node.node_id,
            source_name: node.node_name,
            total_bytes,
            items,
            created_at_ms: now_millis(),
        };
        crate::server::validate_mobile_offer(&offer)?;
        self.register_mobile_offer(session_id, offer, Some(originals.into()))
    }

    pub(super) fn has_preparing_outgoing(&self) -> bool {
        if self.source_scans.load(Ordering::SeqCst) != 0
            || self
                .outgoing_prepares
                .lock()
                .values()
                .any(|p| p.state == OutgoingPrepareState::Preparing)
        {
            return true;
        }
        // 已经建立的任务由传输状态判定，失败时保留的重试素材不阻止升级。
        let staged = self
            .staged_outgoing
            .lock()
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let transfers = self.transfers.read();
        staged.iter().any(|id| !transfers.contains_key(id))
    }

    pub fn add_outgoing_transfer(&self, offer: impl Into<Arc<TransferOffer>>) -> TransferProgress {
        let offer = offer.into();
        let peer_name = self.peer_name(&offer.target_node_id);
        let _update_gate = self.transfer_update_gate.lock();
        if self.update_installing.load(Ordering::SeqCst) {
            let mut failed = transfer_progress(offer, "send");
            failed.status = TransferStatus::Failed;
            failed.error = Some(UPDATE_INSTALLING_MESSAGE.to_string());
            return failed;
        }
        let mut transfers = self.transfers.write();
        if let Some(record) = transfers.get(&offer.transfer_id) {
            return record.progress.clone();
        }
        let mut progress = transfer_progress(offer.clone(), "send");
        if !evict_terminal_transfer_if_needed(&mut transfers) {
            let mut failed = progress;
            failed.status = TransferStatus::Failed;
            failed.error = Some("节点传输记录资源暂时不足，请稍后重试".to_string());
            return failed;
        }
        progress.awaiting_offer_reply = true;
        transfers.insert(
            offer.transfer_id.clone(),
            TransferRecord {
                progress: progress.clone(),
                accepted_destination: None,
                received_items: HashSet::new(),
                received_location: None,
                peer_name,
                source_paths: Vec::new(),
                committed_bytes: 0,
                upload_in_flight: None,
                updated_at_ms: now_millis(),
            },
        );
        drop(transfers);
        self.publish_event(WireMessage::TransferProgress {
            progress: progress.clone(),
        });
        progress
    }

    fn outgoing_staging_directory(&self, transfer_id: &str) -> PathBuf {
        self.data_dir.join(OUTGOING_STAGING_DIR).join(transfer_id)
    }

    fn valid_staging_id(value: &str) -> bool {
        !value.is_empty()
            && value.len() <= 128
            && !value.contains("..")
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    }

    /// 页面把源文件写给本机节点时先登记清单，上传阶段就不再依赖页面状态。
    /// Android 的文件选择器只给 content URI，只能走这条复制路径。
    pub fn begin_outgoing_staging(&self, offer: TransferOffer) -> Result<(), String> {
        if !Self::valid_staging_id(&offer.transfer_id) {
            return Err("传输标识无效".to_string());
        }
        if offer
            .items
            .iter()
            .any(|item| !Self::valid_staging_id(&item.item_id))
        {
            return Err("传输项目标识无效".to_string());
        }
        let _gate = self.transfer_update_gate.lock();
        if self.update_installing.load(Ordering::SeqCst) {
            return Err(UPDATE_INSTALLING_MESSAGE.to_owned());
        }
        let directory = self.outgoing_staging_directory(&offer.transfer_id);
        let mut staged = self.staged_outgoing.lock();
        if staged.contains_key(&offer.transfer_id) {
            return Err("传输已在暂存中".to_string());
        }
        if staged.len() >= MAX_TRANSFER_RECORDS {
            return Err("暂存任务数量已达到上限".to_string());
        }
        std::fs::create_dir_all(&directory)
            .map_err(|error| format!("无法创建暂存目录：{error}"))?;
        staged.insert(
            offer.transfer_id.clone(),
            StagedOutgoing {
                offer: offer.into(),
                directory,
                sources: HashMap::new(),
                created_at_ms: now_millis(),
            },
        );
        Ok(())
    }

    /// 桌面端按原路径直传：登记选中的文件与文件夹，由节点在后台枚举、计算摘要并登记清单。
    pub fn begin_outgoing_paths(
        self: &Arc<Self>,
        target_node_id: String,
        paths: Vec<String>,
    ) -> Result<OutgoingPrepareView, String> {
        let _scan = self.reserve_source_scan()?;
        let (sources, source_roots) = expand_outgoing_paths(&paths)?;
        let total_bytes = sources.iter().map(|source| source.size).sum::<u64>();
        let total_items = sources.len();
        let transfer_id = Uuid::new_v4().to_string();
        {
            let mut prepares = self.outgoing_prepares.lock();
            if prepares.len() >= MAX_TRANSFER_RECORDS {
                return Err("同时准备的发送任务过多，请稍后重试".to_string());
            }
            prepares.insert(
                transfer_id.clone(),
                OutgoingPrepare {
                    target_node_id,
                    sources,
                    source_roots,
                    total_bytes,
                    hashed_bytes: 0,
                    items_ready: 0,
                    state: OutgoingPrepareState::Preparing,
                    created_at_ms: now_millis(),
                },
            );
        }
        let runtime = self.clone();
        let id = transfer_id.clone();
        tokio::spawn(async move {
            runtime.prepare_outgoing_paths(&id).await;
        });
        Ok(OutgoingPrepareView {
            transfer_id,
            state: "preparing".to_string(),
            error: None,
            hashed_bytes: 0,
            total_bytes,
            items_ready: 0,
            total_items,
        })
    }

    fn outgoing_prepare_source_roots(&self, transfer_id: &str) -> Vec<String> {
        self.outgoing_prepares
            .lock()
            .get(transfer_id)
            .map(|prepare| prepare.source_roots.clone())
            .unwrap_or_default()
    }

    pub fn outgoing_prepare_view(&self, transfer_id: &str) -> Option<OutgoingPrepareView> {
        let prepares = self.outgoing_prepares.lock();
        let prepare = prepares.get(transfer_id)?;
        let (state, error) = match &prepare.state {
            OutgoingPrepareState::Preparing => ("preparing", None),
            OutgoingPrepareState::Failed(error) => ("failed", Some(error.clone())),
        };
        Some(OutgoingPrepareView {
            transfer_id: transfer_id.to_string(),
            state: state.to_string(),
            error,
            hashed_bytes: prepare.hashed_bytes,
            total_bytes: prepare.total_bytes,
            items_ready: prepare.items_ready,
            total_items: prepare.sources.len(),
        })
    }

    pub fn cancel_outgoing_prepare(&self, transfer_id: &str) {
        self.outgoing_prepares.lock().remove(transfer_id);
    }

    fn prune_outgoing_prepares(&self) {
        let now = now_millis();
        self.outgoing_prepares.lock().retain(|_, prepare| {
            now.saturating_sub(prepare.created_at_ms) <= OUTGOING_PREPARE_RETENTION_MS
        });
    }

    fn outgoing_prepare_snapshot(
        &self,
        transfer_id: &str,
    ) -> Option<(String, Vec<PreparedSource>)> {
        let prepares = self.outgoing_prepares.lock();
        let prepare = prepares.get(transfer_id)?;
        Some((prepare.target_node_id.clone(), prepare.sources.clone()))
    }

    fn outgoing_prepare_alive(&self, transfer_id: &str) -> bool {
        self.outgoing_prepares
            .lock()
            .get(transfer_id)
            .is_some_and(|prepare| prepare.state == OutgoingPrepareState::Preparing)
    }

    fn add_outgoing_prepare_bytes(&self, transfer_id: &str, bytes: u64) {
        if let Some(prepare) = self.outgoing_prepares.lock().get_mut(transfer_id) {
            prepare.hashed_bytes = prepare.hashed_bytes.saturating_add(bytes);
        }
    }

    fn add_outgoing_prepare_item(&self, transfer_id: &str) {
        if let Some(prepare) = self.outgoing_prepares.lock().get_mut(transfer_id) {
            prepare.items_ready = prepare.items_ready.saturating_add(1);
        }
    }

    fn fail_outgoing_prepare(&self, transfer_id: &str, message: String) {
        if let Some(prepare) = self.outgoing_prepares.lock().get_mut(transfer_id) {
            prepare.state = OutgoingPrepareState::Failed(message);
        }
    }

    async fn prepare_outgoing_paths(&self, transfer_id: &str) {
        let Some((target_node_id, sources)) = self.outgoing_prepare_snapshot(transfer_id) else {
            return;
        };
        let mut items = Vec::with_capacity(sources.len());
        for (index, source) in sources.iter().enumerate() {
            if !self.outgoing_prepare_alive(transfer_id) {
                return;
            }
            let digest = match self.hash_outgoing_source(transfer_id, source).await {
                Ok(Some(digest)) => digest,
                Ok(None) => return,
                Err(error) => {
                    self.fail_outgoing_prepare(transfer_id, error);
                    return;
                }
            };
            items.push(source.item(index, digest));
            self.add_outgoing_prepare_item(transfer_id);
        }
        let offer = TransferOffer {
            transfer_id: transfer_id.to_string(),
            source_node_id: self.node_id(),
            source_name: self.node_info().node_name,
            target_node_id: target_node_id.clone(),
            total_bytes: sources.iter().map(|source| source.size).sum(),
            items,
            created_at_ms: now_millis(),
        };
        if let Err(error) = crate::server::validate_outgoing_offer(&offer) {
            self.fail_outgoing_prepare(transfer_id, error);
            return;
        }
        let sources_by_item = offer
            .items
            .iter()
            .zip(sources.iter())
            .map(|(item, source)| {
                (
                    item.item_id.clone(),
                    OutgoingItemSource::Original {
                        path: source.path.clone(),
                        size: source.size,
                        modified_ms: source.modified_ms,
                    },
                )
            })
            .collect::<HashMap<_, _>>();
        let offer = Arc::new(offer);
        if let Err(error) = self.install_outgoing_paths(offer.clone(), sources_by_item) {
            self.fail_outgoing_prepare(transfer_id, error);
            return;
        }
        let progress = self.add_outgoing_transfer(offer.clone());
        let source_roots = self.outgoing_prepare_source_roots(transfer_id);
        self.set_transfer_source_paths(transfer_id, source_roots);
        if progress.status == TransferStatus::Failed {
            let message = progress
                .error
                .clone()
                .unwrap_or_else(|| "无法创建传输".to_string());
            self.remove_outgoing_staging(transfer_id);
            self.fail_outgoing_prepare(transfer_id, message);
            return;
        }
        if !self
            .send_to_peer_with_retry(&target_node_id, WireMessage::TransferOffer { offer })
            .await
        {
            let message = "目标设备暂时无法接收传输请求".to_string();
            if let Some(failed) =
                self.fail_transfer(transfer_id, message.clone(), TransferStatus::Failed)
            {
                self.publish_event(WireMessage::TransferProgress { progress: failed });
            }
            self.remove_outgoing_staging(transfer_id);
            self.fail_outgoing_prepare(transfer_id, message);
            return;
        }
        self.outgoing_prepares.lock().remove(transfer_id);
    }

    /// 返回 `Ok(None)` 表示传输已被取消，调用方应静默退出。
    async fn hash_outgoing_source(
        &self,
        transfer_id: &str,
        source: &PreparedSource,
    ) -> Result<Option<String>, String> {
        let mut file = tokio::fs::File::open(&source.path)
            .await
            .map_err(|error| format!("无法读取 {}：{error}", source.relative_path))?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; OUTGOING_HASH_CHUNK_BYTES];
        let mut total = 0u64;
        loop {
            if !self.outgoing_prepare_alive(transfer_id) {
                return Ok(None);
            }
            let read = file
                .read(&mut buffer)
                .await
                .map_err(|error| format!("读取 {} 失败：{error}", source.relative_path))?;
            if read == 0 {
                break;
            }
            total = total.saturating_add(read as u64);
            hasher.update(&buffer[..read]);
            self.add_outgoing_prepare_bytes(transfer_id, read as u64);
        }
        if total != source.size {
            return Err(format!(
                "{} 在准备期间发生变化，请重新选择后再发送",
                source.relative_path
            ));
        }
        Ok(Some(format!("{:x}", hasher.finalize())))
    }

    fn install_outgoing_paths(
        &self,
        offer: Arc<TransferOffer>,
        sources: HashMap<String, OutgoingItemSource>,
    ) -> Result<(), String> {
        let directory = self.outgoing_staging_directory(&offer.transfer_id);
        std::fs::create_dir_all(&directory)
            .map_err(|error| format!("无法创建暂存目录：{error}"))?;
        self.staged_outgoing.lock().insert(
            offer.transfer_id.clone(),
            StagedOutgoing {
                offer,
                directory,
                sources,
                created_at_ms: now_millis(),
            },
        );
        Ok(())
    }

    pub fn outgoing_item(&self, transfer_id: &str, item_id: &str) -> Option<TransferItem> {
        self.staged_outgoing
            .lock()
            .get(transfer_id)?
            .offer
            .items
            .iter()
            .find(|item| item.item_id == item_id)
            .cloned()
    }

    pub fn outgoing_staged_target(&self, transfer_id: &str, item_id: &str) -> Option<PathBuf> {
        let staged = self.staged_outgoing.lock();
        let record = staged.get(transfer_id)?;
        let index = record
            .offer
            .items
            .iter()
            .position(|item| item.item_id == item_id)?;
        Some(record.directory.join(format!("{index}.bin")))
    }

    pub fn is_outgoing_item_ready(&self, transfer_id: &str, item_id: &str) -> bool {
        self.staged_outgoing
            .lock()
            .get(transfer_id)
            .is_some_and(|record| record.sources.contains_key(item_id))
    }

    /// digest 是暂存上传实际写入内容的摘要，回填后随清单发给接收端。
    pub fn mark_outgoing_item_staged(&self, transfer_id: &str, item_id: &str, digest: String) {
        if let Some(record) = self.staged_outgoing.lock().get_mut(transfer_id) {
            if let Some(item) = Arc::make_mut(&mut record.offer)
                .items
                .iter_mut()
                .find(|item| item.item_id == item_id)
            {
                item.sha256 = digest;
            }
            record
                .sources
                .insert(item_id.to_string(), OutgoingItemSource::Staged);
        }
    }

    pub fn outgoing_offer_if_ready(&self, transfer_id: &str) -> Result<Arc<TransferOffer>, String> {
        let staged = self.staged_outgoing.lock();
        let record = staged
            .get(transfer_id)
            .ok_or_else(|| "暂存任务不存在".to_string())?;
        if record.sources.len() < record.offer.items.len() {
            return Err("源文件尚未全部就绪".to_string());
        }
        Ok(record.offer.clone())
    }

    pub fn outgoing_staging_counts(&self, transfer_id: &str) -> Option<(usize, usize)> {
        let staged = self.staged_outgoing.lock();
        let record = staged.get(transfer_id)?;
        Some((record.sources.len(), record.offer.items.len()))
    }

    /// 取消或完成后删除暂存目录，避免发送端长期占用双份空间。
    pub fn remove_outgoing_staging(&self, transfer_id: &str) {
        self.outgoing_prepares.lock().remove(transfer_id);
        let removed = self.staged_outgoing.lock().remove(transfer_id);
        let Some(record) = removed else {
            return;
        };
        if let Err(error) = std::fs::remove_dir_all(&record.directory) {
            if error.kind() != std::io::ErrorKind::NotFound {
                eprintln!("清理发送暂存目录失败：{error}");
            }
        }
    }

    /// 出站上传由节点定时驱动：接收端确认后立即开始，不依赖页面定时器或窗口可见性。
    pub fn spawn_outgoing_upload_supervisor(self: &Arc<Self>) {
        let runtime = self.clone();
        tokio::spawn(async move {
            loop {
                if runtime.stopping.load(Ordering::SeqCst) {
                    break;
                }
                runtime.drive_outgoing_uploads();
                runtime.prune_outgoing_prepares();
                tokio::time::sleep(Duration::from_millis(OUTGOING_UPLOAD_IDLE_POLL_MS)).await;
            }
        });
    }

    fn drive_outgoing_uploads(self: &Arc<Self>) {
        let entries: Vec<_> = self
            .staged_outgoing
            .lock()
            .iter()
            .map(|(id, entry)| (id.clone(), entry.created_at_ms))
            .collect();
        let now = now_millis();
        for (id, created_at) in entries {
            let record = self
                .transfers
                .read()
                .get(&id)
                .map(|record| (record.progress.status.clone(), record.updated_at_ms));
            match record {
                Some((TransferStatus::Accepted | TransferStatus::Uploading, _)) => {
                    self.spawn_outgoing_upload(&id)
                }
                Some((
                    TransferStatus::Completed
                    | TransferStatus::Rejected
                    | TransferStatus::Cancelled,
                    _,
                )) => self.remove_outgoing_staging(&id),
                Some((TransferStatus::Failed, updated))
                    if now.saturating_sub(updated) > FAILED_TRANSFER_RETENTION_MS =>
                {
                    self.remove_outgoing_staging(&id)
                }
                None if now.saturating_sub(created_at) > OUTGOING_STAGING_RETENTION_MS => {
                    self.remove_outgoing_staging(&id)
                }
                _ => {}
            }
        }
    }

    fn spawn_outgoing_upload(self: &Arc<Self>, transfer_id: &str) {
        if !self.outgoing_uploads.lock().insert(transfer_id.to_string()) {
            return;
        }
        let runtime = self.clone();
        let id = transfer_id.to_string();
        tokio::spawn(async move {
            if let Err(error) = runtime.upload_outgoing_transfer(&id).await {
                eprintln!("向对端发送文件失败：{error}");
            }
            runtime.outgoing_uploads.lock().remove(&id);
        });
    }

    async fn resolve_outgoing_target(&self, node_id: &str) -> Option<String> {
        for base in self.peer_http_bases(node_id) {
            let result =
                tokio::time::timeout(Duration::from_secs(6), fetch_node_info(&self.http, &base))
                    .await;
            if matches!(result, Ok(Ok(info)) if info.node_id == node_id) {
                return Some(base);
            }
        }
        None
    }

    async fn put_outgoing_item(
        self: &Arc<Self>,
        base: &str,
        offer: &TransferOffer,
        item: &TransferItem,
        path: &Path,
    ) -> Result<TransferProgress, (String, bool)> {
        let file = tokio::fs::File::open(path)
            .await
            .map_err(|error| (format!("读取源文件失败：{error}"), false))?;
        let url = format!(
            "{base}/api/v1/transfers/{}/items/{}",
            crate::server::encode_path_segment(&offer.transfer_id),
            crate::server::encode_path_segment(&item.item_id)
        );
        let runtime = self.clone();
        let progress_id = offer.transfer_id.clone();
        let item_id = item.item_id.clone();
        let stream = ReaderStream::new(file).scan(
            (0u64, Instant::now()),
            move |(sent, last_report), chunk| {
                if let Ok(bytes) = &chunk {
                    *sent = sent.saturating_add(bytes.len() as u64);
                    if last_report.elapsed() >= TRANSFER_PROGRESS_INTERVAL {
                        if let Some(progress) =
                            runtime.report_transfer_progress(&progress_id, &item_id, *sent)
                        {
                            runtime.publish_event(WireMessage::TransferProgress { progress });
                        }
                        *last_report = Instant::now();
                    }
                }
                std::future::ready(Some(chunk))
            },
        );
        let response = self
            .upload_http
            .put(url)
            .header(crate::model::TARGET_HEADER, &offer.target_node_id)
            .header(reqwest::header::CONTENT_TYPE, item.media_type.clone())
            .body(reqwest::Body::wrap_stream(stream))
            .send()
            .await;
        match response {
            Ok(response) if response.status().is_success() => {
                let progress = response
                    .json::<TransferProgress>()
                    .await
                    .map_err(|error| (format!("接收端未返回传输进度：{error}"), true))?;
                if progress.transfer_id != offer.transfer_id {
                    return Err(("接收端返回了其他任务的进度".to_string(), false));
                }
                Ok(progress)
            }
            Ok(response) => {
                let status = response.status().as_u16();
                let retryable = status >= 500 || matches!(status, 408 | 425 | 429);
                let message = response
                    .json::<serde_json::Value>()
                    .await
                    .ok()
                    .and_then(|value| {
                        value
                            .get("error")
                            .and_then(|error| error.as_str())
                            .map(str::to_string)
                    })
                    .unwrap_or_else(|| format!("接收端返回 {status}"));
                Err((message, retryable))
            }
            Err(error) => Err((format!("传输中断：{error}"), true)),
        }
    }

    async fn fail_outgoing_transfer(
        &self,
        transfer_id: &str,
        generation: u32,
        message: String,
    ) -> String {
        let Some(progress) = self.fail_transfer_at_generation(
            transfer_id,
            message.clone(),
            TransferStatus::Failed,
            Some(generation),
        ) else {
            return message;
        };
        if progress.status != TransferStatus::Failed {
            return message;
        }
        self.publish_event(WireMessage::TransferProgress {
            progress: progress.clone(),
        });
        if let Some((source_node_id, target_node_id)) = self.transfer_peer_ids(transfer_id) {
            let peer = if source_node_id == self.node_id() {
                target_node_id
            } else {
                source_node_id
            };
            if peer != self.node_id() {
                let _ = self
                    .send_to_peer_with_retry(
                        &peer,
                        WireMessage::TransferAction {
                            transfer_id: transfer_id.to_string(),
                            action: crate::model::TransferAction::Fail,
                            reason: Some(message.clone()),
                            retry_count: progress.retry_count,
                        },
                    )
                    .await;
            }
        }
        message
    }

    pub(crate) async fn upload_outgoing_transfer(
        self: &Arc<Self>,
        transfer_id: &str,
    ) -> Result<(), String> {
        let Some(entry) = self.staged_outgoing.lock().get(transfer_id).cloned() else {
            return Ok(());
        };
        let Some(progress) = self.begin_transfer_upload(transfer_id) else {
            return Ok(());
        };
        let generation = progress.retry_count;
        self.publish_event(WireMessage::TransferProgress { progress });
        let Some(base) = self
            .resolve_outgoing_target(&entry.offer.target_node_id)
            .await
        else {
            return Err(self
                .fail_outgoing_transfer(
                    transfer_id,
                    generation,
                    "无法连接接收端节点，文件尚未发送".to_string(),
                )
                .await);
        };
        for (index, item) in entry.offer.items.iter().enumerate() {
            if !self.transfer_upload_active(transfer_id) {
                return Ok(());
            }
            if self
                .transfers
                .read()
                .get(transfer_id)
                .is_some_and(|record| record.received_items.contains(&item.item_id))
            {
                continue;
            }
            let (path, staged) = match entry.sources.get(&item.item_id) {
                Some(OutgoingItemSource::Staged) => {
                    (entry.directory.join(format!("{index}.bin")), true)
                }
                Some(OutgoingItemSource::Original {
                    path,
                    size,
                    modified_ms,
                }) => {
                    let metadata = match tokio::fs::metadata(path).await {
                        Ok(metadata) => metadata,
                        Err(error) => {
                            let message = format!("{}：无法读取源文件（{error}）", item.name);
                            return Err(self
                                .fail_outgoing_transfer(transfer_id, generation, message)
                                .await);
                        }
                    };
                    // 源文件在准备之后被修改或替换时明确失败，避免把不一致的字节发出去。
                    if metadata.len() != *size || file_modified_ms(&metadata) != *modified_ms {
                        let message = format!("{}：源文件已变化，请重新发送", item.name);
                        return Err(self
                            .fail_outgoing_transfer(transfer_id, generation, message)
                            .await);
                    }
                    (path.clone(), false)
                }
                None => {
                    let message = format!("{}：源文件尚未就绪", item.name);
                    return Err(self
                        .fail_outgoing_transfer(transfer_id, generation, message)
                        .await);
                }
            };
            let mut attempt = 0;
            let reported = loop {
                attempt += 1;
                match self
                    .put_outgoing_item(&base, &entry.offer, item, &path)
                    .await
                {
                    Ok(progress) => break progress,
                    Err((message, retryable)) => {
                        if !retryable || attempt >= OUTGOING_ITEM_UPLOAD_ATTEMPTS {
                            return Err(self
                                .fail_outgoing_transfer(
                                    transfer_id,
                                    generation,
                                    format!("{}：{message}", item.name),
                                )
                                .await);
                        }
                        let backoff = 400u64.saturating_mul(1u64 << (attempt as u32 - 1));
                        tokio::time::sleep(Duration::from_millis(backoff.min(4_000))).await;
                    }
                }
            };
            if reported.retry_count != generation
                || !self.apply_peer_progress(
                    &entry.offer.target_node_id,
                    reported,
                    Some(&item.item_id),
                )
            {
                return Err(self
                    .fail_outgoing_transfer(
                        transfer_id,
                        generation,
                        "接收端返回的确认与当前传输不一致".into(),
                    )
                    .await);
            }
            if staged {
                if let Err(error) = tokio::fs::remove_file(&path).await {
                    if error.kind() != std::io::ErrorKind::NotFound {
                        eprintln!("清理暂存文件失败：{error}");
                    }
                }
            }
        }
        if self
            .transfer(transfer_id)
            .is_some_and(|record| record.progress.status == TransferStatus::Completed)
        {
            self.remove_outgoing_staging(transfer_id);
        }
        Ok(())
    }
}
