use crate::clipboard;
use crate::discovery;
use crate::model::{
    ClipboardSnapshot, HardwareMetrics, MobileSession, MobileTransferView, NodeInfo, PeerInfo,
    TransferOffer, TransferProgress, TransferStatus, WireMessage, METRICS_SYSTEM_CAPABILITY,
};
use crate::runtime::NodeRuntime;
use axum::body::Body;
use axum::extract::{Path as AxumPath, Query, State, WebSocketUpgrade};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use tokio_util::io::ReaderStream;
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};

#[derive(Clone)]
pub struct AppState {
    pub runtime: Arc<NodeRuntime>,
    pub static_root: PathBuf,
}

#[derive(Debug, Deserialize)]
pub struct ClipboardRequest {
    pub text: Option<String>,
    pub image_png: Option<String>,
    pub source_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PeerConnectionRequest {
    pub url: String,
}

#[derive(Debug, Deserialize)]
pub struct TransferListQuery {
    pub status: Option<TransferStatus>,
}

#[derive(Debug, Deserialize)]
pub struct MobileSessionCreateRequest {
    pub label: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MobileSessionQuery {
    pub session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MobileSessionRequest {
    pub session_id: String,
}

#[derive(Debug, Deserialize)]
pub struct MobileTransferFailureRequest {
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MobileTransferCreateRequest {
    pub session_id: String,
    pub offer: TransferOffer,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: String,
}

pub fn router(runtime: Arc<NodeRuntime>, static_root: PathBuf) -> Router {
    let index = static_root.join("index.html");
    let state = AppState {
        runtime,
        static_root,
    };
    Router::new()
        .route("/api/v1/info", get(info))
        .route("/api/v1/peers", get(peers))
        .route("/api/v1/metrics", get(metrics))
        .route("/api/v1/peers/connect", post(connect_peer))
        .route(
            "/api/v1/clipboard",
            get(clipboard_state).post(post_clipboard),
        )
        .route("/api/v1/ws", get(websocket))
        .route(
            "/api/v1/mobile/sessions",
            get(list_mobile_sessions).post(create_mobile_session),
        )
        .route(
            "/api/v1/mobile/transfers",
            get(list_mobile_transfers).post(create_mobile_transfer),
        )
        .route(
            "/api/v1/mobile/transfers/:id/accept",
            post(accept_mobile_transfer),
        )
        .route(
            "/api/v1/mobile/transfers/:id/reject",
            post(reject_mobile_transfer),
        )
        .route(
            "/api/v1/mobile/transfers/:id/fail",
            post(fail_mobile_transfer),
        )
        .route(
            "/api/v1/mobile/transfers/:id/items/:item_id",
            put(upload_mobile_item),
        )
        .route(
            "/api/v1/mobile/transfers/:id/items/:item_id/download",
            get(download_mobile_item),
        )
        .route(
            "/api/v1/transfers",
            get(list_transfers).post(create_transfer),
        )
        .route("/api/v1/transfers/:id/reject", post(reject_transfer))
        .route("/api/v1/transfers/:id/cancel", post(cancel_transfer))
        .route("/api/v1/transfers/:id/status", get(transfer_status))
        .route("/api/v1/transfers/:id/items/:item_id", put(upload_item))
        .route(
            "/api/*path",
            get(api_not_found)
                .post(api_not_found)
                .put(api_not_found)
                .delete(api_not_found),
        )
        .fallback_service(ServeDir::new(state.static_root.clone()).fallback(ServeFile::new(index)))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

async fn info(State(state): State<AppState>) -> Json<NodeInfo> {
    state.runtime.update_addresses();
    Json(state.runtime.node_info())
}

async fn peers(State(state): State<AppState>) -> Json<Vec<PeerInfo>> {
    Json(state.runtime.peers_snapshot())
}

async fn metrics(State(state): State<AppState>) -> Json<Option<HardwareMetrics>> {
    Json(state.runtime.local_metrics())
}

async fn connect_peer(
    State(state): State<AppState>,
    Json(payload): Json<PeerConnectionRequest>,
) -> Result<Json<PeerInfo>, ApiError> {
    discovery::connect_manual(state.runtime, payload.url)
        .await
        .map(Json)
        .map_err(ApiError::bad_request)
}

async fn clipboard_state(State(state): State<AppState>) -> Json<Option<ClipboardSnapshot>> {
    Json(state.runtime.clipboard_snapshot())
}

async fn api_not_found() -> ApiError {
    ApiError::not_found("接口不存在")
}

async fn post_clipboard(
    State(state): State<AppState>,
    Json(payload): Json<ClipboardRequest>,
) -> Result<Json<ClipboardSnapshot>, ApiError> {
    let image_bytes = payload
        .image_png
        .as_deref()
        .map(|value| {
            BASE64
                .decode(value)
                .map_err(|_| ApiError::bad_request("image_png 无效"))
        })
        .transpose()?;
    if image_bytes
        .as_ref()
        .is_some_and(|value| value.len() > crate::model::CLIPBOARD_IMAGE_LIMIT)
    {
        return Err(ApiError::payload_too_large("图片超过 16 MiB 限制"));
    }
    let event = state
        .runtime
        .create_local_clipboard_named(
            payload.text,
            image_bytes,
            payload
                .source_name
                .unwrap_or_else(|| "手机浏览器".to_string()),
        )
        .ok_or_else(|| ApiError::bad_request("剪贴板内容为空或同步已暂停"))?;
    let snapshot = state
        .runtime
        .clipboard_snapshot()
        .ok_or_else(|| ApiError::internal("剪贴板状态不可用"))?;
    clipboard::apply_snapshot(&snapshot, &state.runtime).map_err(ApiError::internal)?;
    state.runtime.publish_event(WireMessage::ClipboardUpdate {
        event: event.clone(),
    });
    state
        .runtime
        .send_to_peers_except(None, WireMessage::ClipboardUpdate { event });
    Ok(Json(snapshot))
}

async fn websocket(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.max_message_size(32 * 1024 * 1024)
        .max_frame_size(32 * 1024 * 1024)
        .on_upgrade(move |socket| handle_socket(socket, state.runtime))
}

async fn handle_socket(socket: axum::extract::ws::WebSocket, runtime: Arc<NodeRuntime>) {
    let (mut sender, mut receiver) = socket.split();
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel::<WireMessage>(64);
    let mut events = runtime.subscribe();
    let hello = WireMessage::Hello {
        node: runtime.node_info(),
    };
    let _ = sender
        .send(axum::extract::ws::Message::Text(
            serde_json::to_string(&hello).unwrap_or_default(),
        ))
        .await;
    let mut peer_id: Option<String> = None;
    let mut owns_peer_channel = false;
    let mut clipboard_resync = tokio::time::interval(std::time::Duration::from_secs(1));

    loop {
        tokio::select! {
            incoming = receiver.next() => {
                match incoming {
                    Some(Ok(axum::extract::ws::Message::Text(text))) => {
                        match serde_json::from_str::<WireMessage>(&text) {
                            Ok(message) => {
                                if let WireMessage::Hello { node } = &message {
                                    if peer_id
                                        .as_deref()
                                        .is_some_and(|current| current != node.node_id)
                                    {
                                        break;
                                    }
                                }
                                if let Some(identified) = handle_wire_message(message, &runtime, peer_id.as_deref()).await {
                                    match peer_id.as_deref() {
                                        None => {
                                            if !runtime.add_peer_channel(identified.clone(), outgoing_tx.clone()) {
                                                break;
                                            }
                                            peer_id = Some(identified);
                                            owns_peer_channel = true;
                                            if let Some(snapshot) = runtime.clipboard_snapshot() {
                                                let _ = outgoing_tx.try_send(WireMessage::ClipboardSnapshot { snapshot });
                                            }
                                        }
                                        Some(current) if current == identified => {}
                                        Some(_) => break,
                                    }
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    Some(Ok(axum::extract::ws::Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
            outbound = outgoing_rx.recv() => {
                if let Some(message) = outbound {
                    if sender.send(axum::extract::ws::Message::Text(serde_json::to_string(&message).unwrap_or_default())).await.is_err() {
                        break;
                    }
                }
            }
            _ = clipboard_resync.tick() => {
                if let Some(peer_id) = peer_id.as_deref() {
                    let _ = runtime.flush_clipboard_resync(peer_id);
                }
            }
            event = events.recv() => {
                match event {
                    Ok(message) => {
                        if peer_id.is_some() {
                            continue;
                        }
                        if sender.send(axum::extract::ws::Message::Text(serde_json::to_string(&message).unwrap_or_default())).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
    if owns_peer_channel {
        if let Some(peer_id) = peer_id {
            runtime.remove_peer(&peer_id);
        }
    }
}

pub(crate) async fn handle_wire_message(
    message: WireMessage,
    runtime: &Arc<NodeRuntime>,
    current_peer: Option<&str>,
) -> Option<String> {
    match message {
        WireMessage::Hello { node } => {
            if node.node_id == runtime.node_id() {
                return None;
            }
            if node.protocol_version != crate::model::PROTOCOL_VERSION {
                return None;
            }
            let endpoint = node
                .addresses
                .first()
                .map(|address| format!("{}/api/v1/ws", address.url));
            runtime.set_peer(PeerInfo {
                node_id: node.node_id.clone(),
                node_name: node.node_name,
                platform: node.platform,
                addresses: node.addresses,
                capabilities: node.capabilities,
                online: true,
                last_seen_ms: crate::clock::now_millis(),
                endpoint,
                source: "websocket".to_string(),
                metrics: None,
                metrics_received_at_ms: None,
                metrics_stale: false,
            });
            Some(node.node_id)
        }
        WireMessage::Ping { at_ms } => {
            if let Some(peer_id) = current_peer {
                runtime.touch_peer(peer_id);
                let _ = runtime.send_to_peer(peer_id, WireMessage::Pong { at_ms });
            }
            current_peer.map(str::to_string)
        }
        WireMessage::Pong { .. } => {
            if let Some(peer_id) = current_peer {
                runtime.touch_peer(peer_id);
            }
            current_peer.map(str::to_string)
        }
        WireMessage::ClipboardUpdate { event } => {
            let peer_id = current_peer?;
            let source_name = runtime
                .peer_name(peer_id)
                .unwrap_or_else(|| "远端设备".to_string());
            if runtime.accept_clipboard(event.clone(), source_name) {
                if let Some(snapshot) = runtime.clipboard_snapshot() {
                    if let Err(error) = clipboard::apply_snapshot(&snapshot, runtime) {
                        eprintln!("无法写入远端剪贴板：{error}");
                    }
                }
                runtime.publish_event(WireMessage::ClipboardUpdate {
                    event: event.clone(),
                });
                runtime.send_to_peers_except(Some(peer_id), WireMessage::ClipboardUpdate { event });
            }
            Some(peer_id.to_string())
        }
        WireMessage::ClipboardSnapshot { snapshot } => {
            let peer_id = current_peer?;
            let source_name = if snapshot.source_name.trim().is_empty() {
                runtime
                    .peer_name(peer_id)
                    .unwrap_or_else(|| "远端设备".to_string())
            } else {
                snapshot.source_name.clone()
            };
            let event = crate::model::ClipboardEvent {
                event_id: snapshot.event_id,
                origin_node_id: snapshot.source_node_id,
                clock: snapshot.clock,
                text: snapshot.text,
                image_png: snapshot.image_png,
                updated_at_ms: snapshot.updated_at_ms,
            };
            if runtime.accept_clipboard(event.clone(), source_name) {
                if let Some(snapshot) = runtime.clipboard_snapshot() {
                    if let Err(error) = clipboard::apply_snapshot(&snapshot, runtime) {
                        eprintln!("无法写入远端剪贴板：{error}");
                    }
                }
                runtime.publish_event(WireMessage::ClipboardUpdate {
                    event: event.clone(),
                });
                runtime.send_to_peers_except(Some(peer_id), WireMessage::ClipboardUpdate { event });
            }
            Some(peer_id.to_string())
        }
        WireMessage::TransferOffer { offer } => {
            let peer_id = current_peer?;
            if offer.source_node_id != peer_id || offer.target_node_id != runtime.node_id() {
                return Some(peer_id.to_string());
            }
            runtime.add_transfer(offer);
            Some(peer_id.to_string())
        }
        WireMessage::TransferAction {
            transfer_id,
            action,
            destination,
            reason,
        } => {
            let _ = apply_transfer_action(
                runtime,
                &transfer_id,
                &action,
                destination,
                reason,
                current_peer,
            )
            .await;
            current_peer.map(str::to_string)
        }
        WireMessage::TransferProgress { progress } => {
            let transfer_id = progress.transfer_id.clone();
            if let Some((source_node_id, target_node_id)) = runtime.transfer_peer_ids(&transfer_id)
            {
                if current_peer == Some(source_node_id.as_str())
                    || current_peer == Some(target_node_id.as_str())
                {
                    runtime.update_transfer(&transfer_id, progress);
                }
            } else {
                return current_peer.map(str::to_string);
            }
            current_peer.map(str::to_string)
        }
        WireMessage::MetricsUpdate { metrics } => {
            let peer_id = current_peer?;
            if runtime.peer_supports(peer_id, METRICS_SYSTEM_CAPABILITY)
                && runtime.update_peer_metrics(peer_id, metrics.clone())
            {
                runtime.publish_event(WireMessage::MetricsUpdate { metrics });
            }
            Some(peer_id.to_string())
        }
    }
}

async fn create_transfer(
    State(state): State<AppState>,
    Json(mut offer): Json<TransferOffer>,
) -> Result<Json<TransferProgress>, ApiError> {
    validate_offer(&offer)?;
    offer.target_node_id = state.runtime.node_id();
    offer.created_at_ms = crate::clock::now_millis();
    state.runtime.add_transfer(offer.clone());
    let progress = state
        .runtime
        .transfer(&offer.transfer_id)
        .map(|record| record.progress)
        .ok_or_else(|| ApiError::internal("无法创建传输"))?;
    Ok(Json(progress))
}

async fn list_transfers(
    Query(query): Query<TransferListQuery>,
    State(state): State<AppState>,
) -> Json<Vec<TransferProgress>> {
    Json(state.runtime.transfers_snapshot(query.status))
}

fn validate_offer(offer: &TransferOffer) -> Result<(), ApiError> {
    let listed_bytes = offer
        .items
        .iter()
        .try_fold(0u64, |total, item| total.checked_add(item.size));
    if offer.items.is_empty()
        || listed_bytes != Some(offer.total_bytes)
        || offer.total_bytes > crate::model::MAX_TRANSFER_BYTES
    {
        return Err(ApiError::payload_too_large("传输清单为空或超过大小限制"));
    }
    let mut item_ids = HashSet::new();
    for item in &offer.items {
        if item.directory
            || item.size > crate::model::MAX_TRANSFER_BYTES
            || has_parent_component(&item.relative_path)
            || item.relative_path.is_empty()
            || !item_ids.insert(item.item_id.clone())
        {
            return Err(ApiError::bad_request("传输清单包含无效项目"));
        }
    }
    Ok(())
}

async fn create_mobile_session(
    State(state): State<AppState>,
    Json(payload): Json<MobileSessionCreateRequest>,
) -> Json<MobileSession> {
    cleanup_mobile_staging(&state).await;
    let label = payload
        .label
        .filter(|label| !label.trim().is_empty())
        .unwrap_or_else(|| "手机浏览器".to_string());
    Json(state.runtime.create_mobile_session(label))
}

async fn list_mobile_sessions(State(state): State<AppState>) -> Json<Vec<MobileSession>> {
    let sessions = state.runtime.mobile_sessions_snapshot();
    cleanup_mobile_staging(&state).await;
    Json(sessions)
}

async fn list_mobile_transfers(
    Query(query): Query<MobileSessionQuery>,
    State(state): State<AppState>,
) -> Result<Json<Vec<MobileTransferView>>, ApiError> {
    if let Some(session_id) = query.session_id.as_deref() {
        state
            .runtime
            .touch_mobile_session(session_id)
            .ok_or_else(|| ApiError::not_found("手机会话已过期"))?;
    }
    cleanup_mobile_staging(&state).await;
    Ok(Json(
        state
            .runtime
            .mobile_transfers_snapshot(query.session_id.as_deref()),
    ))
}

async fn cleanup_mobile_staging(state: &AppState) {
    state.runtime.mobile_sessions_snapshot();
    for directory in state.runtime.prune_mobile_transfers() {
        let _ = fs::remove_dir_all(directory).await;
    }
}

async fn cleanup_mobile_partial_files(directory: &Path) {
    let Ok(metadata) = fs::symlink_metadata(directory).await else {
        return;
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return;
    }
    let mut directories = vec![directory.to_path_buf()];
    while let Some(current) = directories.pop() {
        let Ok(mut entries) = fs::read_dir(current).await else {
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            let Ok(file_type) = entry.file_type().await else {
                continue;
            };
            if file_type.is_dir() {
                directories.push(path);
            } else if file_type.is_file()
                && path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().ends_with(".part"))
            {
                let _ = fs::remove_file(path).await;
            }
        }
    }
}

async fn create_mobile_transfer(
    State(state): State<AppState>,
    Json(mut payload): Json<MobileTransferCreateRequest>,
) -> Result<Json<MobileTransferView>, ApiError> {
    validate_offer(&payload.offer)?;
    if payload.offer.source_node_id != state.runtime.node_id() {
        return Err(ApiError::bad_request("手机下载暂只支持扫码节点上的文件"));
    }
    payload.offer.target_node_id = state.runtime.node_id();
    payload.offer.created_at_ms = crate::clock::now_millis();
    let view = state
        .runtime
        .create_mobile_transfer(&payload.session_id, payload.offer)
        .map_err(ApiError::bad_request)?;
    Ok(Json(view))
}

async fn accept_mobile_transfer(
    AxumPath(id): AxumPath<String>,
    State(state): State<AppState>,
    Json(payload): Json<MobileSessionRequest>,
) -> Result<Json<MobileTransferView>, ApiError> {
    state
        .runtime
        .touch_mobile_session(&payload.session_id)
        .ok_or_else(|| ApiError::not_found("手机会话已过期"))?;
    state
        .runtime
        .set_mobile_transfer_status(&id, &payload.session_id, TransferStatus::Accepted)
        .map(Json)
        .ok_or_else(|| ApiError::not_found("手机传输不存在"))
}

async fn reject_mobile_transfer(
    AxumPath(id): AxumPath<String>,
    State(state): State<AppState>,
    Json(payload): Json<MobileSessionRequest>,
) -> Result<Json<MobileTransferView>, ApiError> {
    state
        .runtime
        .touch_mobile_session(&payload.session_id)
        .ok_or_else(|| ApiError::not_found("手机会话已过期"))?;
    state
        .runtime
        .set_mobile_transfer_status(&id, &payload.session_id, TransferStatus::Rejected)
        .map(Json)
        .ok_or_else(|| ApiError::not_found("手机传输不存在"))
}

async fn fail_mobile_transfer(
    AxumPath(id): AxumPath<String>,
    State(state): State<AppState>,
    Json(payload): Json<MobileTransferFailureRequest>,
) -> Result<Json<MobileTransferView>, ApiError> {
    let record = state
        .runtime
        .mobile_transfer(&id)
        .ok_or_else(|| ApiError::not_found("手机传输不存在"))?;
    if record.offer.source_node_id != state.runtime.node_id() {
        return Err(ApiError::forbidden("手机传输来源不匹配"));
    }
    let message = payload
        .error
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "手机文件上传失败".to_string());
    let progress = state
        .runtime
        .fail_mobile_transfer(&id, message)
        .ok_or_else(|| ApiError::not_found("手机传输不存在"))?;
    cleanup_mobile_partial_files(&record.staged_directory).await;
    Ok(Json(progress))
}

async fn upload_mobile_item(
    AxumPath((transfer_id, item_id)): AxumPath<(String, String)>,
    State(state): State<AppState>,
    body: Body,
) -> Result<Json<MobileTransferView>, ApiError> {
    let record = state
        .runtime
        .mobile_transfer(&transfer_id)
        .ok_or_else(|| ApiError::not_found("手机传输不存在"))?;
    if record.offer.source_node_id != state.runtime.node_id()
        || !matches!(
            record.progress.status,
            TransferStatus::Accepted | TransferStatus::Uploading
        )
    {
        return Err(ApiError::conflict("手机传输尚未被接受"));
    }
    let item = record
        .offer
        .items
        .iter()
        .find(|item| item.item_id == item_id)
        .ok_or_else(|| ApiError::not_found("传输项目不存在"))?;
    if item.directory || has_parent_component(&item.relative_path) || item.relative_path.is_empty()
    {
        return Err(ApiError::bad_request("传输路径无效"));
    }
    let _item_lock = TransferItemLock::new(&state.runtime, &transfer_id, &item_id)?;
    if let Some(progress) = state
        .runtime
        .mobile_item_already_staged(&transfer_id, &item_id)
    {
        return Ok(Json(progress));
    }
    fs::create_dir_all(&record.staged_directory)
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?;
    let target = prepare_target(&record.staged_directory, &item.relative_path).await?;
    let temp = target.with_extension(format!("meshshare-{}.part", uuid::Uuid::new_v4()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?;
    let mut stream = body.into_data_stream();
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let stream_result: Result<(u64, String), ApiError> = async {
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| ApiError::internal(error.to_string()))?;
            total = total.saturating_add(chunk.len() as u64);
            if total > item.size || total > crate::model::MAX_TRANSFER_BYTES {
                return Err(ApiError::payload_too_large("传输内容超过清单大小"));
            }
            hasher.update(&chunk);
            file.write_all(&chunk)
                .await
                .map_err(|error| ApiError::internal(error.to_string()))?;
        }
        file.flush()
            .await
            .map_err(|error| ApiError::internal(error.to_string()))?;
        Ok((total, format!("{:x}", hasher.finalize())))
    }
    .await;
    drop(file);
    let (total, digest) = match stream_result {
        Ok(value) => value,
        Err(error) => {
            let _ = fs::remove_file(&temp).await;
            return Err(error);
        }
    };
    if total != item.size || digest != item.sha256 {
        let _ = fs::remove_file(&temp).await;
        let _ = state
            .runtime
            .fail_mobile_transfer(&transfer_id, "文件校验失败".to_string());
        return Err(ApiError::bad_request("文件校验失败"));
    }
    let target = match prepare_target(&record.staged_directory, &item.relative_path).await {
        Ok(target) => target,
        Err(error) => {
            let _ = fs::remove_file(&temp).await;
            return Err(error);
        }
    };
    if let Err(error) = fs::hard_link(&temp, &target).await {
        let _ = fs::remove_file(&temp).await;
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            return Err(ApiError::conflict(format!(
                "目标已存在：{}",
                target.display()
            )));
        }
        return Err(ApiError::internal(error.to_string()));
    }
    let _ = fs::remove_file(&temp).await;
    let Some(progress) = state
        .runtime
        .stage_mobile_item(&transfer_id, &item_id, total)
    else {
        let _ = fs::remove_file(&target).await;
        return Err(ApiError::conflict("手机传输已取消或完成"));
    };
    Ok(Json(progress))
}

async fn download_mobile_item(
    AxumPath((transfer_id, item_id)): AxumPath<(String, String)>,
    Query(query): Query<MobileSessionQuery>,
    State(state): State<AppState>,
) -> Result<Response, ApiError> {
    let session_id = query
        .session_id
        .ok_or_else(|| ApiError::bad_request("缺少手机会话"))?;
    state
        .runtime
        .touch_mobile_session(&session_id)
        .ok_or_else(|| ApiError::not_found("手机会话已过期"))?;
    let record = state
        .runtime
        .mobile_transfer(&transfer_id)
        .ok_or_else(|| ApiError::not_found("手机传输不存在"))?;
    if record.session_id != session_id {
        return Err(ApiError::forbidden("手机会话不匹配"));
    }
    let item = record
        .offer
        .items
        .iter()
        .find(|item| item.item_id == item_id)
        .ok_or_else(|| ApiError::not_found("传输项目不存在"))?;
    let path = state
        .runtime
        .mobile_item_path(&transfer_id, &item_id)
        .ok_or_else(|| ApiError::conflict("文件尚未准备好下载"))?;
    let file = fs::File::open(&path)
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?;
    let size = file
        .metadata()
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?
        .len();
    let filename = item
        .name
        .chars()
        .map(|value| {
            if value.is_ascii_alphanumeric() || matches!(value, '.' | '-' | '_' | ' ') {
                value
            } else {
                '_'
            }
        })
        .collect::<String>();
    let content_type = HeaderValue::from_str(&item.media_type)
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
    let content_disposition = HeaderValue::from_str(&format!(
        "attachment; filename=\"{}\"",
        if filename.is_empty() {
            "download"
        } else {
            &filename
        }
    ))
    .map_err(|error| ApiError::internal(error.to_string()))?;
    let mut response = (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CONTENT_LENGTH, HeaderValue::from(size)),
            (header::CONTENT_DISPOSITION, content_disposition),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        Body::from_stream(ReaderStream::new(file)),
    )
        .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

async fn reject_transfer(
    AxumPath(id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<TransferProgress>, ApiError> {
    let progress = state
        .runtime
        .fail_transfer(
            &id,
            "接收端拒绝了传输".to_string(),
            TransferStatus::Rejected,
        )
        .ok_or_else(|| ApiError::not_found("传输不存在"))?;
    if let Some(source_node_id) = state.runtime.transfer_source_id(&id) {
        let _ = state.runtime.send_to_peer(
            &source_node_id,
            WireMessage::TransferAction {
                transfer_id: id.clone(),
                action: "reject".to_string(),
                destination: None,
                reason: progress.error.clone(),
            },
        );
    }
    state.runtime.publish_event(WireMessage::TransferAction {
        transfer_id: id,
        action: "reject".to_string(),
        destination: None,
        reason: progress.error.clone(),
    });
    state.runtime.publish_event(WireMessage::TransferProgress {
        progress: progress.clone(),
    });
    Ok(Json(progress))
}

async fn cancel_transfer(
    AxumPath(id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<TransferProgress>, ApiError> {
    let progress = state
        .runtime
        .fail_transfer(&id, "传输已取消".to_string(), TransferStatus::Cancelled)
        .ok_or_else(|| ApiError::not_found("传输不存在"))?;
    if let Some(source_node_id) = state.runtime.transfer_source_id(&id) {
        let _ = state.runtime.send_to_peer(
            &source_node_id,
            WireMessage::TransferAction {
                transfer_id: id.clone(),
                action: "cancel".to_string(),
                destination: None,
                reason: progress.error.clone(),
            },
        );
    }
    state.runtime.publish_event(WireMessage::TransferAction {
        transfer_id: id,
        action: "cancel".to_string(),
        destination: None,
        reason: progress.error.clone(),
    });
    state.runtime.publish_event(WireMessage::TransferProgress {
        progress: progress.clone(),
    });
    Ok(Json(progress))
}

async fn transfer_status(
    AxumPath(id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<TransferProgress>, ApiError> {
    state
        .runtime
        .transfer(&id)
        .map(|record| Json(record.progress))
        .ok_or_else(|| ApiError::not_found("传输不存在"))
}

async fn upload_item(
    AxumPath((transfer_id, item_id)): AxumPath<(String, String)>,
    State(state): State<AppState>,
    body: Body,
) -> Result<Json<TransferProgress>, ApiError> {
    let record = state
        .runtime
        .transfer(&transfer_id)
        .ok_or_else(|| ApiError::not_found("传输不存在"))?;
    if record.progress.status != TransferStatus::Accepted
        && record.progress.status != TransferStatus::Uploading
    {
        return Err(ApiError::conflict("传输尚未被接收端接受"));
    }
    let destination = record
        .accepted_destination
        .clone()
        .ok_or_else(|| ApiError::conflict("接收目录不可用"))?;
    let item = record
        .offer
        .items
        .iter()
        .find(|item| item.item_id == item_id)
        .ok_or_else(|| ApiError::not_found("传输项目不存在"))?;
    if item.directory || has_parent_component(&item.relative_path) || item.relative_path.is_empty()
    {
        return Err(ApiError::bad_request("传输路径无效"));
    }
    let _item_lock = TransferItemLock::new(&state.runtime, &transfer_id, &item_id)?;
    let target = prepare_target(&destination, &item.relative_path).await?;
    let temp = target.with_extension(format!("meshshare-{}.part", uuid::Uuid::new_v4()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?;
    let mut stream = body.into_data_stream();
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let stream_result: Result<(u64, String), ApiError> = async {
        while let Some(chunk) = stream.next().await {
            if !state.runtime.transfer_upload_active(&transfer_id) {
                return Err(ApiError::conflict("传输已取消"));
            }
            let chunk = chunk.map_err(|error| ApiError::internal(error.to_string()))?;
            total = total.saturating_add(chunk.len() as u64);
            if total > item.size || total > crate::model::MAX_TRANSFER_BYTES {
                return Err(ApiError::payload_too_large("传输内容超过清单大小"));
            }
            hasher.update(&chunk);
            file.write_all(&chunk)
                .await
                .map_err(|error| ApiError::internal(error.to_string()))?;
        }
        file.flush()
            .await
            .map_err(|error| ApiError::internal(error.to_string()))?;
        Ok((total, format!("{:x}", hasher.finalize())))
    }
    .await;
    drop(file);
    let (total, digest) = match stream_result {
        Ok(value) => value,
        Err(error) => {
            let _ = fs::remove_file(&temp).await;
            return Err(error);
        }
    };
    if total != item.size || digest != item.sha256 {
        let _ = fs::remove_file(&temp).await;
        let message = "文件校验失败".to_string();
        let progress = state
            .runtime
            .fail_transfer(&transfer_id, message.clone(), TransferStatus::Failed)
            .ok_or_else(|| ApiError::not_found("传输不存在"))?;
        state
            .runtime
            .publish_event(WireMessage::TransferProgress { progress });
        return Err(ApiError::bad_request(message));
    }
    let target = match prepare_target(&destination, &item.relative_path).await {
        Ok(target) => target,
        Err(error) => {
            let _ = fs::remove_file(&temp).await;
            return Err(error);
        }
    };
    if !state.runtime.transfer_upload_active(&transfer_id) {
        let _ = fs::remove_file(&temp).await;
        return Err(ApiError::conflict("传输已取消"));
    }
    if let Err(error) = fs::hard_link(&temp, &target).await {
        let _ = fs::remove_file(&temp).await;
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            return Err(ApiError::conflict(format!(
                "目标已存在：{}",
                target.display()
            )));
        }
        return Err(ApiError::internal(error.to_string()));
    }
    let _ = fs::remove_file(&temp).await;
    let Some(progress) = state
        .runtime
        .mark_item_received(&transfer_id, &item_id, total)
    else {
        let _ = fs::remove_file(&target).await;
        return Err(ApiError::conflict("传输已取消或已完成"));
    };
    state.runtime.publish_event(WireMessage::TransferProgress {
        progress: progress.clone(),
    });
    Ok(Json(progress))
}

struct TransferItemLock {
    runtime: Arc<NodeRuntime>,
    transfer_id: String,
    item_id: String,
}

impl TransferItemLock {
    fn new(runtime: &Arc<NodeRuntime>, transfer_id: &str, item_id: &str) -> Result<Self, ApiError> {
        if !runtime.try_lock_transfer_item(transfer_id, item_id) {
            return Err(ApiError::conflict("文件项目正在上传"));
        }
        Ok(Self {
            runtime: runtime.clone(),
            transfer_id: transfer_id.to_string(),
            item_id: item_id.to_string(),
        })
    }
}

impl Drop for TransferItemLock {
    fn drop(&mut self) {
        self.runtime
            .unlock_transfer_item(&self.transfer_id, &self.item_id);
    }
}

async fn prepare_target(destination: &Path, relative_path: &str) -> Result<PathBuf, ApiError> {
    let mut parent = destination.to_path_buf();
    let components = Path::new(relative_path).components().collect::<Vec<_>>();
    let Some((file_component, directory_components)) = components.split_last() else {
        return Err(ApiError::bad_request("传输路径为空"));
    };
    let Component::Normal(file_name) = file_component else {
        return Err(ApiError::bad_request("传输文件名无效"));
    };
    for component in directory_components {
        let Component::Normal(name) = component else {
            return Err(ApiError::bad_request("传输路径无效"));
        };
        parent.push(name);
        match fs::symlink_metadata(&parent).await {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(ApiError::bad_request("传输目录包含符号链接或非目录"));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match fs::create_dir(&parent).await {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        let metadata = fs::symlink_metadata(&parent)
                            .await
                            .map_err(|error| ApiError::internal(error.to_string()))?;
                        if metadata.file_type().is_symlink() || !metadata.is_dir() {
                            return Err(ApiError::bad_request("传输目录包含符号链接或非目录"));
                        }
                    }
                    Err(error) => return Err(ApiError::internal(error.to_string())),
                }
            }
            Err(error) => return Err(ApiError::internal(error.to_string())),
        }
    }
    let target = parent.join(file_name);
    match fs::symlink_metadata(&target).await {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(ApiError::conflict("目标文件是符号链接"));
            }
            return Err(ApiError::conflict(format!(
                "目标已存在：{}",
                target.display()
            )));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(ApiError::internal(error.to_string())),
    }
    Ok(target)
}

async fn apply_transfer_action(
    runtime: &Arc<NodeRuntime>,
    transfer_id: &str,
    action: &str,
    _destination: Option<String>,
    reason: Option<String>,
    current_peer: Option<&str>,
) -> Result<(), ApiError> {
    let Some((source_node_id, target_node_id)) = runtime.transfer_peer_ids(transfer_id) else {
        return Err(ApiError::not_found("传输不存在"));
    };
    if current_peer != Some(source_node_id.as_str())
        && current_peer != Some(target_node_id.as_str())
    {
        return Err(ApiError::forbidden("传输操作来源不匹配"));
    }
    match action {
        "accept" => {
            // The destination is selected locally on the receiving node and is
            // never accepted from a peer message.
            if current_peer == Some(target_node_id.as_str()) {
                if let Some(progress) = runtime.mark_transfer_accepted(transfer_id) {
                    runtime.publish_event(WireMessage::TransferProgress { progress });
                }
            }
        }
        "reject" => {
            if let Some(progress) = runtime.fail_transfer(
                transfer_id,
                reason.unwrap_or_else(|| "接收端拒绝了传输".to_string()),
                TransferStatus::Rejected,
            ) {
                runtime.publish_event(WireMessage::TransferProgress { progress });
            }
        }
        "cancel" => {
            if let Some(progress) = runtime.fail_transfer(
                transfer_id,
                reason.unwrap_or_else(|| "传输已取消".to_string()),
                TransferStatus::Cancelled,
            ) {
                runtime.publish_event(WireMessage::TransferProgress { progress });
            }
        }
        _ => {}
    }
    Ok(())
}

fn has_parent_component(value: &str) -> bool {
    value.contains('\\')
        || Path::new(value).components().any(|component| {
            matches!(
                component,
                Component::ParentDir
                    | Component::RootDir
                    | Component::Prefix(_)
                    | Component::CurDir
            )
        })
}

#[cfg(test)]
mod tests {
    use super::{cleanup_mobile_partial_files, handle_wire_message, has_parent_component};
    use crate::model::{
        ClipboardEvent, ClipboardSnapshot, HardwareMetrics, Hlc, NodeInfo, PeerInfo, WireMessage,
        METRICS_SYSTEM_CAPABILITY, PROTOCOL_VERSION,
    };
    use crate::runtime::NodeRuntime;
    use futures_util::{SinkExt, StreamExt};
    use std::path::PathBuf;
    use tokio::sync::mpsc;
    use tokio_tungstenite::connect_async;
    use uuid::Uuid;

    #[test]
    fn rejects_parent_and_absolute_paths() {
        assert!(has_parent_component("../secret.txt"));
        assert!(has_parent_component("/tmp/secret.txt"));
        assert!(!has_parent_component("folder/secret.txt"));
    }

    #[tokio::test]
    async fn cleanup_mobile_partial_files_preserves_completed_files() {
        let directory =
            std::env::temp_dir().join(format!("meshshare-partial-test-{}", Uuid::new_v4()));
        let nested = directory.join("nested");
        std::fs::create_dir_all(&nested).expect("create partial test directory");
        std::fs::write(directory.join("complete.bin"), b"complete").expect("write complete file");
        std::fs::write(nested.join("upload.meshshare-id.part"), b"partial")
            .expect("write partial file");

        cleanup_mobile_partial_files(&directory).await;

        assert!(directory.join("complete.bin").exists());
        assert!(!nested.join("upload.meshshare-id.part").exists());
        let _ = std::fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn remote_metrics_update_is_not_rebroadcast() {
        let data_dir =
            std::env::temp_dir().join(format!("meshshare-server-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&data_dir).expect("create test directory");
        let runtime = NodeRuntime::new(data_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        runtime.set_peer(PeerInfo {
            node_id: "peer".to_string(),
            node_name: "peer".to_string(),
            platform: "test".to_string(),
            addresses: Vec::new(),
            capabilities: vec![METRICS_SYSTEM_CAPABILITY.to_string()],
            online: true,
            last_seen_ms: crate::clock::now_millis(),
            endpoint: None,
            source: "test".to_string(),
            metrics: None,
            metrics_received_at_ms: None,
            metrics_stale: false,
        });
        let (tx, mut rx) = mpsc::channel(8);
        runtime.add_peer_channel("peer".to_string(), tx);
        let metrics = HardwareMetrics {
            node_id: "peer".to_string(),
            sampled_at_ms: 1,
            cpu_usage_percent: Some(2.0),
            memory_used_bytes: None,
            memory_total_bytes: None,
            network_down_bytes_per_sec: None,
            network_up_bytes_per_sec: None,
            gpus: Vec::new(),
        };
        let result = handle_wire_message(
            WireMessage::MetricsUpdate { metrics },
            &runtime,
            Some("peer"),
        )
        .await;
        assert_eq!(result.as_deref(), Some("peer"));
        assert!(rx.try_recv().is_err());
        assert!(runtime.peers_snapshot()[0].metrics.is_some());
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn forwarded_clipboard_events_keep_their_original_source() {
        let data_dir =
            std::env::temp_dir().join(format!("meshshare-forward-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&data_dir).expect("create test directory");
        let runtime = NodeRuntime::new(data_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        runtime.set_peer(PeerInfo {
            node_id: "relay".to_string(),
            node_name: "Relay".to_string(),
            platform: "test".to_string(),
            addresses: Vec::new(),
            capabilities: Vec::new(),
            online: true,
            last_seen_ms: crate::clock::now_millis(),
            endpoint: None,
            source: "test".to_string(),
            metrics: None,
            metrics_received_at_ms: None,
            metrics_stale: false,
        });
        let event = ClipboardEvent {
            event_id: "event-from-origin".to_string(),
            origin_node_id: "origin".to_string(),
            clock: Hlc {
                physical_ms: 10,
                logical: 0,
                node_id: "origin".to_string(),
            },
            text: Some("forwarded".to_string()),
            image_png: None,
            updated_at_ms: 10,
        };
        let result = handle_wire_message(
            WireMessage::ClipboardUpdate {
                event: event.clone(),
            },
            &runtime,
            Some("relay"),
        )
        .await;
        assert_eq!(result.as_deref(), Some("relay"));
        let snapshot = runtime.clipboard_snapshot().expect("clipboard snapshot");
        assert_eq!(snapshot.source_node_id, "origin");
        assert_eq!(snapshot.text.as_deref(), Some("forwarded"));

        let snapshot_message = WireMessage::ClipboardSnapshot {
            snapshot: ClipboardSnapshot {
                event_id: "snapshot-from-origin".to_string(),
                source_node_id: "origin".to_string(),
                source_name: "Origin".to_string(),
                clock: Hlc {
                    physical_ms: 20,
                    logical: 0,
                    node_id: "origin".to_string(),
                },
                text: Some("snapshot-forwarded".to_string()),
                image_png: None,
                updated_at_ms: 20,
            },
        };
        let _ = handle_wire_message(snapshot_message, &runtime, Some("relay")).await;
        let snapshot = runtime
            .clipboard_snapshot()
            .expect("newer clipboard snapshot");
        assert_eq!(snapshot.source_node_id, "origin");
        assert_eq!(snapshot.source_name, "Origin");
        assert_eq!(snapshot.text.as_deref(), Some("snapshot-forwarded"));
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn websocket_keeps_one_channel_for_normal_peer_messages() {
        let data_dir =
            std::env::temp_dir().join(format!("meshshare-websocket-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&data_dir).expect("create test directory");
        let static_root = PathBuf::from(&data_dir).join("web");
        std::fs::create_dir_all(&static_root).expect("create static directory");
        std::fs::write(static_root.join("index.html"), "<html></html>")
            .expect("write static index");
        let runtime = NodeRuntime::new(data_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let app = super::router(runtime.clone(), static_root);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let address = listener.local_addr().expect("listener address");
        let server_task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test router");
        });
        let (mut socket, _) = connect_async(format!("ws://{address}/api/v1/ws"))
            .await
            .expect("connect websocket");
        let _ = socket
            .next()
            .await
            .expect("server hello")
            .expect("hello message");
        let peer = NodeInfo {
            node_id: "peer".to_string(),
            node_name: "Peer".to_string(),
            version: "test".to_string(),
            protocol_version: PROTOCOL_VERSION,
            platform: "test".to_string(),
            port: 0,
            addresses: Vec::new(),
            capabilities: Vec::new(),
        };
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                serde_json::to_string(&WireMessage::Hello { node: peer }).expect("serialize hello"),
            ))
            .await
            .expect("send hello");
        for index in 0..20 {
            socket
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    serde_json::to_string(&WireMessage::Ping { at_ms: index }).expect("ping"),
                ))
                .await
                .expect("send ping");
            loop {
                let message = socket
                    .next()
                    .await
                    .expect("pong response")
                    .expect("valid websocket message");
                let tokio_tungstenite::tungstenite::Message::Text(body) = message else {
                    continue;
                };
                if matches!(
                    serde_json::from_str::<WireMessage>(&body).expect("decode response"),
                    WireMessage::Pong { .. }
                ) {
                    break;
                }
            }
        }
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                serde_json::to_string(&WireMessage::ClipboardUpdate {
                    event: ClipboardEvent {
                        event_id: "websocket-event".to_string(),
                        origin_node_id: "peer".to_string(),
                        clock: Hlc {
                            physical_ms: 30,
                            logical: 0,
                            node_id: "peer".to_string(),
                        },
                        text: Some("hello".to_string()),
                        image_png: None,
                        updated_at_ms: 30,
                    },
                })
                .expect("clipboard update"),
            ))
            .await
            .expect("send clipboard update");
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                serde_json::to_string(&WireMessage::Ping { at_ms: 21 }).expect("ping"),
            ))
            .await
            .expect("send final ping");
        let mut received_pong = false;
        while let Some(message) = socket.next().await {
            let message = message.expect("final websocket message");
            if let tokio_tungstenite::tungstenite::Message::Text(body) = message {
                if matches!(
                    serde_json::from_str::<WireMessage>(&body).expect("decode final response"),
                    WireMessage::Pong { .. }
                ) {
                    received_pong = true;
                    break;
                }
            }
        }
        assert!(
            received_pong,
            "connection should remain usable after clipboard update"
        );
        let _ = socket.close(None).await;
        runtime
            .stopping
            .store(true, std::sync::atomic::Ordering::SeqCst);
        server_task.abort();
        let _ = std::fs::remove_dir_all(data_dir);
    }
}

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }
    fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: message.into(),
        }
    }
    fn forbidden(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: message.into(),
        }
    }
    fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }
    fn payload_too_large(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            message: message.into(),
        }
    }
    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut response = (
            self.status,
            Json(ErrorBody {
                error: self.message,
            }),
        )
            .into_response();
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        response
    }
}
