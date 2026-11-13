use crate::clipboard;
use crate::discovery;
use crate::model::{
    ClipboardSnapshot, NodeInfo, PeerInfo, TransferOffer, TransferProgress, TransferStatus,
    WireMessage,
};
use crate::runtime::NodeRuntime;
use axum::body::Body;
use axum::extract::{Path as AxumPath, State, WebSocketUpgrade};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
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
pub struct TransferAcceptance {
    pub destination: String,
}

#[derive(Debug, Deserialize)]
pub struct PeerConnectionRequest {
    pub url: String,
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
        .route("/api/v1/peers/connect", post(connect_peer))
        .route(
            "/api/v1/clipboard",
            get(clipboard_state).post(post_clipboard),
        )
        .route("/api/v1/ws", get(websocket))
        .route("/api/v1/transfers", post(create_transfer))
        .route("/api/v1/transfers/:id/accept", post(accept_transfer))
        .route("/api/v1/transfers/:id/reject", post(reject_transfer))
        .route("/api/v1/transfers/:id/cancel", post(cancel_transfer))
        .route("/api/v1/transfers/:id/status", get(transfer_status))
        .route("/api/v1/transfers/:id/items/:item_id", put(upload_item))
        .route(
            "/api/{*path}",
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
    state
        .runtime
        .publish(WireMessage::ClipboardUpdate { event });
    Ok(Json(snapshot))
}

async fn websocket(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state.runtime))
}

async fn handle_socket(socket: axum::extract::ws::WebSocket, runtime: Arc<NodeRuntime>) {
    let (mut sender, mut receiver) = socket.split();
    let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel::<WireMessage>();
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

    loop {
        tokio::select! {
            incoming = receiver.next() => {
                match incoming {
                    Some(Ok(axum::extract::ws::Message::Text(text))) => {
                        match serde_json::from_str::<WireMessage>(&text) {
                            Ok(message) => {
                                if let Some(identified) = handle_wire_message(message, &runtime, peer_id.as_deref()).await {
                                    peer_id = Some(identified.clone());
                                    runtime.add_peer_channel(identified, outgoing_tx.clone());
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
            event = events.recv() => {
                match event {
                    Ok(message) => {
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
    if let Some(peer_id) = peer_id {
        runtime.remove_peer(&peer_id);
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
            });
            Some(node.node_id)
        }
        WireMessage::Ping { at_ms } => {
            if let Some(peer_id) = current_peer {
                runtime.touch_peer(peer_id);
            }
            runtime.publish(WireMessage::Pong { at_ms });
            current_peer.map(str::to_string)
        }
        WireMessage::Pong { .. } => {
            if let Some(peer_id) = current_peer {
                runtime.touch_peer(peer_id);
            }
            current_peer.map(str::to_string)
        }
        WireMessage::ClipboardUpdate { event } => {
            if runtime.accept_clipboard(event.clone(), "远端设备".to_string()) {
                if let Some(snapshot) = runtime.clipboard_snapshot() {
                    let _ = clipboard::apply_snapshot(&snapshot, runtime);
                }
                runtime.publish(WireMessage::ClipboardUpdate { event });
            }
            current_peer.map(str::to_string)
        }
        WireMessage::TransferOffer { offer } => {
            runtime.add_transfer(offer);
            current_peer.map(str::to_string)
        }
        WireMessage::TransferAction {
            transfer_id,
            action,
            destination,
            reason,
        } => {
            let _ =
                apply_transfer_action(runtime, &transfer_id, &action, destination, reason).await;
            current_peer.map(str::to_string)
        }
        WireMessage::TransferProgress { progress } => {
            let transfer_id = progress.transfer_id.clone();
            runtime.update_transfer(&transfer_id, progress);
            current_peer.map(str::to_string)
        }
    }
}

async fn create_transfer(
    State(state): State<AppState>,
    Json(mut offer): Json<TransferOffer>,
) -> Result<Json<TransferProgress>, ApiError> {
    let listed_bytes = offer.items.iter().map(|item| item.size).sum::<u64>();
    if offer.items.is_empty()
        || listed_bytes != offer.total_bytes
        || offer.total_bytes > crate::model::MAX_TRANSFER_BYTES
    {
        return Err(ApiError::payload_too_large("传输清单为空或超过大小限制"));
    }
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

async fn accept_transfer(
    AxumPath(id): AxumPath<String>,
    State(state): State<AppState>,
    Json(payload): Json<TransferAcceptance>,
) -> Result<Json<TransferProgress>, ApiError> {
    let destination = PathBuf::from(payload.destination);
    if !destination.is_absolute() {
        return Err(ApiError::bad_request("接收目录必须是绝对路径"));
    }
    fs::create_dir_all(&destination)
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?;
    let progress = state
        .runtime
        .set_transfer_acceptance(&id, destination)
        .ok_or_else(|| ApiError::not_found("传输不存在"))?;
    state.runtime.publish(WireMessage::TransferAction {
        transfer_id: id,
        action: "accept".to_string(),
        destination: progress.destination.clone(),
        reason: None,
    });
    state.runtime.publish(WireMessage::TransferProgress {
        progress: progress.clone(),
    });
    Ok(Json(progress))
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
    state.runtime.publish(WireMessage::TransferAction {
        transfer_id: id,
        action: "reject".to_string(),
        destination: None,
        reason: progress.error.clone(),
    });
    state.runtime.publish(WireMessage::TransferProgress {
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
    state.runtime.publish(WireMessage::TransferAction {
        transfer_id: id,
        action: "cancel".to_string(),
        destination: None,
        reason: progress.error.clone(),
    });
    state.runtime.publish(WireMessage::TransferProgress {
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
    if item.directory || has_parent_component(&item.relative_path) {
        return Err(ApiError::bad_request("传输路径无效"));
    }
    let target = destination.join(&item.relative_path);
    if fs::try_exists(&target).await.unwrap_or(false) {
        return Err(ApiError::conflict(format!(
            "目标已存在：{}",
            target.display()
        )));
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .await
            .map_err(|error| ApiError::internal(error.to_string()))?;
    }
    let temp = target.with_extension(format!("meshshare-{}.part", uuid::Uuid::new_v4()));
    let mut file = fs::File::create(&temp)
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
            .publish(WireMessage::TransferProgress { progress });
        return Err(ApiError::bad_request(message));
    }
    if let Err(error) = fs::rename(&temp, &target).await {
        let _ = fs::remove_file(&temp).await;
        return Err(ApiError::internal(error.to_string()));
    }
    let progress = state
        .runtime
        .mark_item_received(&transfer_id, &item_id, total)
        .ok_or_else(|| ApiError::not_found("传输不存在"))?;
    state.runtime.publish(WireMessage::TransferProgress {
        progress: progress.clone(),
    });
    Ok(Json(progress))
}

async fn apply_transfer_action(
    runtime: &Arc<NodeRuntime>,
    transfer_id: &str,
    action: &str,
    destination: Option<String>,
    reason: Option<String>,
) -> Result<(), ApiError> {
    match action {
        "accept" => {
            if let Some(destination) = destination {
                let progress = runtime
                    .set_transfer_acceptance(transfer_id, PathBuf::from(destination))
                    .ok_or_else(|| ApiError::not_found("传输不存在"))?;
                runtime.publish(WireMessage::TransferProgress { progress });
            }
        }
        "reject" => {
            if let Some(progress) = runtime.fail_transfer(
                transfer_id,
                reason.unwrap_or_else(|| "接收端拒绝了传输".to_string()),
                TransferStatus::Rejected,
            ) {
                runtime.publish(WireMessage::TransferProgress { progress });
            }
        }
        "cancel" => {
            if let Some(progress) = runtime.fail_transfer(
                transfer_id,
                reason.unwrap_or_else(|| "传输已取消".to_string()),
                TransferStatus::Cancelled,
            ) {
                runtime.publish(WireMessage::TransferProgress { progress });
            }
        }
        _ => {}
    }
    Ok(())
}

fn has_parent_component(value: &str) -> bool {
    Path::new(value).components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    })
}

#[cfg(test)]
mod tests {
    use super::has_parent_component;

    #[test]
    fn rejects_parent_and_absolute_paths() {
        assert!(has_parent_component("../secret.txt"));
        assert!(has_parent_component("/tmp/secret.txt"));
        assert!(!has_parent_component("folder/secret.txt"));
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
