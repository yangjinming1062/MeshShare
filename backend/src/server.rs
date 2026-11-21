use crate::clipboard;
use crate::control::{accept_transfer_at, ControlPlane, SettingsPatch, SettingsSnapshot};
use crate::discovery;
use crate::model::{
    AddressInfo, ClipboardEvent, ClipboardHistoryDetail, ClipboardHistoryList, ClipboardSnapshot,
    HardwareMetrics, MobileSession, MobileTransferView, NodeInfo, PeerInfo, TransferHistoryEntry,
    TransferOffer, TransferProgress, TransferStatus, WireMessage, CLIPBOARD_IMAGE_LIMIT,
    CLIPBOARD_TEXT_LIMIT, CLIPBOARD_VIEW_CAPABILITY, METRICS_NETWORK_CAPABILITY,
    METRICS_SYSTEM_CAPABILITY,
};
use crate::runtime::{
    valid_history_delete_ids, validate_receive_directory, NodeRuntime, OutgoingPrepareView,
    PeerSession, TransferRetryError, TRANSFER_PROGRESS_INTERVAL, UPDATE_INSTALLING_MESSAGE,
};
use axum::body::Body;
use axum::extract::{
    ConnectInfo, DefaultBodyLimit, Path as AxumPath, Query, State, WebSocketUpgrade,
};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::{from_fn, Next};
use axum::response::Response as AxumResponse;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use futures_util::{SinkExt, StreamExt};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::io::ReaderStream;
use tower_http::cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};

const PEER_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_WIRE_ERROR_LENGTH: usize = 4_096;
const MAX_METRIC_GPU_COUNT: usize = 64;
const MAX_METRIC_GPU_NAME_LENGTH: usize = 256;
const MAX_METRIC_REASON_LENGTH: usize = 512;
const MAX_METRIC_CPU_MODEL_LENGTH: usize = 256;
const MAX_METRIC_DISK_COUNT: usize = 128;
const MAX_METRIC_DISK_NAME_LENGTH: usize = 256;
const MAX_METRIC_DISK_PATH_LENGTH: usize = 1_024;
const MAX_METRIC_FILESYSTEM_LENGTH: usize = 128;
const MAX_MEDIA_TYPE_LENGTH: usize = 256;
const MIN_FREE_DISK_BYTES: u64 = 64 * 1024 * 1024;

struct DiskSpaceGuard {
    path: PathBuf,
    checked_at: Instant,
    available_bytes: Option<u64>,
}

impl DiskSpaceGuard {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            checked_at: Instant::now() - Duration::from_secs(2),
            available_bytes: None,
        }
    }

    fn has_headroom(&mut self, required_bytes: u64) -> bool {
        if self.checked_at.elapsed() >= Duration::from_secs(1) {
            self.available_bytes = available_disk_space(&self.path);
            self.checked_at = Instant::now();
        }
        self.available_bytes.is_some_and(|available| {
            available >= required_bytes.saturating_add(MIN_FREE_DISK_BYTES)
        })
    }
}

fn available_disk_space(path: &Path) -> Option<u64> {
    let path = std::fs::canonicalize(path).ok()?;
    sysinfo::Disks::new_with_refreshed_list()
        .list()
        .iter()
        .filter_map(|disk| {
            // `canonicalize` 在 Windows 上返回扩展长度路径，
            // 形如 `\\?\C:\...`；而 sysinfo 报告的挂载点是盘符形式。
            // 因此挂载点也要一并规范化，
            // 两者才能按组件逐个比较。
            let mount_point = std::fs::canonicalize(disk.mount_point()).ok()?;
            path.starts_with(&mount_point)
                .then_some((mount_point.components().count(), disk.available_space()))
        })
        .max_by_key(|(component_count, _)| *component_count)
        .map(|(_, available_space)| available_space)
}

/// 把请求体写入已打开的临时文件并计算 SHA-256。
///
/// 每块数据都受 60 秒空闲超时、清单大小上限和磁盘余量约束；调用方通过
/// `on_chunk` 的块长度检查传输或会话是否仍然有效，并更新在途进度。
async fn stream_body_to_file(
    body: Body,
    mut file: fs::File,
    limit: u64,
    disk_root: &Path,
    mut on_chunk: impl FnMut(u64) -> Result<(), ApiError>,
) -> Result<(u64, String), ApiError> {
    let mut stream = body.into_data_stream();
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut disk_guard = DiskSpaceGuard::new(disk_root.to_path_buf());
    loop {
        let chunk = tokio::time::timeout(Duration::from_secs(60), stream.next())
            .await
            .map_err(|_| ApiError::request_timeout("上传长时间没有数据"))?;
        let Some(chunk) = chunk else {
            break;
        };
        let chunk = chunk.map_err(|error| ApiError::internal(error.to_string()))?;
        on_chunk(chunk.len() as u64)?;
        total = total
            .checked_add(chunk.len() as u64)
            .ok_or_else(|| ApiError::payload_too_large("传输内容大小溢出"))?;
        if total > limit {
            return Err(ApiError::payload_too_large("传输内容超过清单大小"));
        }
        if !disk_guard.has_headroom(chunk.len() as u64) {
            return Err(ApiError::conflict("磁盘可用空间不足"));
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

#[derive(Clone)]
pub struct AppState {
    pub runtime: Arc<NodeRuntime>,
    pub control: Arc<ControlPlane>,
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
pub struct TransferHistoryQuery {
    pub session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MobileSessionCreateRequest {
    pub label: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MobileSessionQuery {
    pub session_id: Option<String>,
    #[serde(default)]
    pub auto_complete: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MobileSessionRequest {
    pub session_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MobileTransferFailureRequest {
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MobileTransferCreateRequest {
    pub session_id: String,
    pub offer: TransferOffer,
}

#[derive(Debug, Deserialize)]
pub struct TransferAcceptRequest {
    pub destination: String,
}

#[derive(Debug, Serialize)]
pub struct OutgoingStagingView {
    pub transfer_id: String,
    pub staged_items: usize,
    pub total_items: usize,
}

#[derive(Debug, Serialize)]
pub struct OutgoingStagingCancelled {
    pub transfer_id: String,
}

#[derive(Debug, Deserialize)]
pub struct OutgoingPathRequest {
    pub target_node_id: String,
    pub paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct BrowseQuery {
    pub path: Option<String>,
    pub offset: Option<usize>,
    pub limit: Option<usize>,
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    error: String,
}

#[derive(Debug, Serialize)]
struct ClipboardHistoryCleared {
    cleared: usize,
}

#[allow(dead_code)]
pub fn router(runtime: Arc<NodeRuntime>, static_root: PathBuf) -> Router {
    let control = ControlPlane::runtime_only(runtime.clone());
    router_with_control(runtime, static_root, control)
}

pub fn router_with_control(
    runtime: Arc<NodeRuntime>,
    static_root: PathBuf,
    control: Arc<ControlPlane>,
) -> Router {
    let index = static_root.join("index.html");
    let state = AppState {
        runtime,
        control,
        static_root,
    };
    Router::new()
        .route("/api/v1/info", get(info))
        .route("/api/v1/peers", get(peers))
        .route("/api/v1/metrics", get(metrics))
        .route("/api/v1/settings", get(settings).patch(patch_settings))
        .route("/api/v1/settings/update-check", post(request_update_check))
        .route("/api/v1/peers/connect", post(connect_peer))
        .route(
            "/api/v1/clipboard",
            get(clipboard_state).post(post_clipboard),
        )
        .route("/api/v1/clipboard/status", get(clipboard_status))
        .route(
            "/api/v1/clipboard/history",
            get(clipboard_history).delete(clear_clipboard_history),
        )
        .route(
            "/api/v1/clipboard/history/:id",
            get(clipboard_history_detail),
        )
        .route(
            "/api/v1/clipboard/history/:id/restore",
            post(restore_clipboard_history),
        )
        .route(
            "/api/v1/clipboard/history/:id/thumbnail",
            get(clipboard_history_thumbnail),
        )
        .route("/api/v1/ws", get(websocket))
        .route("/api/v1/mobile/access", get(mobile_access))
        .route("/api/v1/mobile/ca", get(download_mobile_ca))
        .route(
            "/api/v1/mobile/peers/:peer_id/api/v1/info",
            get(proxy_peer_info),
        )
        .route(
            "/api/v1/mobile/peers/:peer_id/api/v1/transfers",
            post(proxy_create_transfer),
        )
        .route(
            "/api/v1/mobile/peers/:peer_id/api/v1/transfers/:id/status",
            get(proxy_transfer_status),
        )
        .route(
            "/api/v1/mobile/peers/:peer_id/api/v1/transfers/:id/reject",
            post(proxy_reject_transfer),
        )
        .route(
            "/api/v1/mobile/peers/:peer_id/api/v1/transfers/:id/cancel",
            post(proxy_cancel_transfer),
        )
        .route(
            "/api/v1/mobile/peers/:peer_id/api/v1/transfers/:id/fail",
            post(proxy_fail_transfer),
        )
        .route(
            "/api/v1/mobile/peers/:peer_id/api/v1/transfers/:id/retry",
            post(proxy_retry_transfer),
        )
        .route(
            "/api/v1/mobile/peers/:peer_id/api/v1/transfers/:id/items/:item_id",
            put(proxy_upload_item),
        )
        .route(
            "/api/v1/mobile/sessions",
            get(list_mobile_sessions).post(create_mobile_session),
        )
        .route(
            "/api/v1/mobile/transfers",
            get(list_mobile_transfers).post(create_mobile_transfer),
        )
        .route(
            "/api/v1/mobile/transfers/:id",
            delete(dismiss_mobile_transfer),
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
            "/api/v1/mobile/transfers/:id/cancel",
            post(cancel_mobile_transfer),
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
            "/api/v1/mobile/transfers/:id/items/:item_id/complete",
            post(complete_mobile_item),
        )
        .route(
            "/api/v1/transfers",
            get(list_transfers).post(create_transfer),
        )
        .route(
            "/api/v1/transfers/history",
            get(list_transfer_history).delete(clear_transfer_history),
        )
        .route("/api/v1/transfers/:id", delete(dismiss_transfer))
        .route("/api/v1/transfers/:id/reject", post(reject_transfer))
        .route("/api/v1/transfers/:id/cancel", post(cancel_transfer))
        .route("/api/v1/transfers/:id/fail", post(fail_transfer))
        .route("/api/v1/transfers/:id/retry", post(retry_transfer))
        .route("/api/v1/transfers/:id/accept", post(accept_transfer))
        .route("/api/v1/transfers/:id/status", get(transfer_status))
        .route("/api/v1/transfers/:id/items/:item_id", put(upload_item))
        .route("/api/v1/outgoing", post(create_outgoing_staging))
        .route(
            "/api/v1/outgoing/:id/items/:item_id",
            put(upload_outgoing_item),
        )
        .route("/api/v1/outgoing/paths", post(create_outgoing_paths))
        .route("/api/v1/outgoing/:id/start", post(start_outgoing_staging))
        .route("/api/v1/outgoing/:id/cancel", post(cancel_outgoing_staging))
        .route("/api/v1/outgoing/:id", get(outgoing_prepare_status))
        .route("/api/v1/management/files/roots", get(management_file_roots))
        .route("/api/v1/management/files", get(management_files))
        .route(
            "/api/v1/management/outgoing/paths",
            post(create_managed_outgoing_paths),
        )
        .route(
            "/api/v1/management/outgoing/:id",
            get(managed_outgoing_prepare),
        )
        .route(
            "/api/v1/management/outgoing/:id/cancel",
            post(cancel_managed_outgoing_prepare),
        )
        .route(
            "/api/*path",
            get(api_not_found)
                .post(api_not_found)
                .put(api_not_found)
                .delete(api_not_found),
        )
        .fallback_service(ServeDir::new(state.static_root.clone()).fallback(ServeFile::new(index)))
        .layer(DefaultBodyLimit::max(32 * 1024 * 1024))
        .layer(
            CorsLayer::new()
                .allow_origin(AllowOrigin::predicate(|origin, _| {
                    is_allowed_origin(origin)
                }))
                .allow_methods(AllowMethods::any())
                .allow_headers(AllowHeaders::any()),
        )
        .layer(from_fn(origin_guard))
        .with_state(state)
}

async fn origin_guard(request: axum::http::Request<Body>, next: Next) -> AxumResponse {
    let is_mutation = !matches!(
        request.method(),
        &Method::GET | &Method::HEAD | &Method::OPTIONS
    );
    let is_websocket_upgrade = request.uri().path() == "/api/v1/ws"
        && request
            .headers()
            .get(header::UPGRADE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.eq_ignore_ascii_case("websocket"));
    let is_api = request.uri().path().starts_with("/api/");
    let allowed = request
        .headers()
        .get(header::ORIGIN)
        .is_none_or(is_allowed_origin);
    if is_api && (is_mutation || is_websocket_upgrade) && !allowed {
        return (StatusCode::FORBIDDEN, "跨来源管理请求被拒绝").into_response();
    }
    next.run(request).await
}

fn is_allowed_origin(origin: &HeaderValue) -> bool {
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    let Ok(url) = reqwest::Url::parse(origin) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https" | "tauri") {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    if matches!(host, "localhost" | "tauri.localhost") {
        return true;
    }
    let Ok(address) = host.parse::<std::net::Ipv4Addr>() else {
        return false;
    };
    address.is_loopback()
        || address.is_private()
        || address.is_link_local()
        || (u32::from(address) & 0xffc00000) == 0x64400000
}

async fn info(State(state): State<AppState>) -> Json<NodeInfo> {
    Json(state.runtime.node_info())
}

async fn peers(State(state): State<AppState>) -> Json<Vec<PeerInfo>> {
    Json(state.runtime.peers_snapshot())
}

async fn metrics(State(state): State<AppState>) -> Json<Option<HardwareMetrics>> {
    Json(state.runtime.local_metrics())
}

async fn settings(State(state): State<AppState>) -> Json<SettingsSnapshot> {
    Json(state.control.snapshot())
}

async fn patch_settings(
    State(state): State<AppState>,
    Json(patch): Json<SettingsPatch>,
) -> Result<Json<SettingsSnapshot>, ApiError> {
    state
        .control
        .apply_patch(patch)
        .map(Json)
        .map_err(ApiError::bad_request)
}

async fn request_update_check(
    State(state): State<AppState>,
) -> Result<Json<SettingsSnapshot>, ApiError> {
    state
        .control
        .request_update_check()
        .map(Json)
        .map_err(ApiError::conflict)
}

async fn clipboard_status(State(state): State<AppState>) -> Json<crate::model::ClipboardStatus> {
    Json(state.runtime.clipboard_status())
}

async fn mobile_access(State(state): State<AppState>) -> Json<crate::model::MobileAccessInfo> {
    Json(state.runtime.mobile_access_info())
}

async fn download_mobile_ca(State(state): State<AppState>) -> Result<Response, ApiError> {
    let path = state
        .runtime
        .mobile_ca_path()
        .ok_or_else(|| ApiError::not_found("手机 CA 尚未生成"))?;
    let bytes = fs::read(&path)
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?;
    let mut response = bytes.into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/x-x509-ca-cert"),
    );
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=meshshare-ca.crt"),
    );
    Ok(response)
}

fn proxy_peer_base(state: &AppState, peer_id: &str) -> Result<String, ApiError> {
    if peer_id == state.runtime.node_id() {
        return Err(ApiError::bad_request("本机传输不需要代理"));
    }
    state
        .runtime
        .peer_http_base(peer_id)
        .ok_or_else(|| ApiError::not_found("目标设备当前不可用"))
}

async fn proxy_json<T: DeserializeOwned>(response: reqwest::Response) -> Result<Json<T>, ApiError> {
    let status = response.status();
    let payload = response
        .json::<serde_json::Value>()
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?;
    if !status.is_success() {
        let message = payload
            .get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("目标设备请求失败");
        let error = match status {
            StatusCode::BAD_REQUEST => ApiError::bad_request(message),
            StatusCode::FORBIDDEN => ApiError::forbidden(message),
            StatusCode::NOT_FOUND => ApiError::not_found(message),
            StatusCode::CONFLICT => ApiError::conflict(message),
            StatusCode::MISDIRECTED_REQUEST => ApiError::misdirected(message),
            StatusCode::TOO_EARLY => ApiError::too_early(message),
            StatusCode::REQUEST_TIMEOUT => ApiError::request_timeout(message),
            StatusCode::TOO_MANY_REQUESTS => ApiError::too_many_requests(message),
            StatusCode::PAYLOAD_TOO_LARGE => ApiError::payload_too_large(message),
            _ if status.is_server_error() => ApiError::internal(message),
            _ => ApiError::conflict(message),
        };
        return Err(error);
    }
    serde_json::from_value(payload)
        .map(Json)
        .map_err(|error| ApiError::internal(error.to_string()))
}

async fn read_proxy_peer_info(
    state: &AppState,
    base: &str,
    peer_id: &str,
) -> Result<NodeInfo, ApiError> {
    let response = state
        .runtime
        .http
        .get(format!("{base}/api/v1/info"))
        .send()
        .await
        .map_err(|error| ApiError::internal(format!("无法连接目标设备：{error}")))?;
    let Json(info) = proxy_json::<NodeInfo>(response).await?;
    if info.node_id != peer_id {
        return Err(ApiError::misdirected("地址上的设备与传输目标不匹配"));
    }
    Ok(info)
}

async fn proxy_peer_info(
    AxumPath(peer_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<NodeInfo>, ApiError> {
    let base = proxy_peer_base(&state, &peer_id)?;
    read_proxy_peer_info(&state, &base, &peer_id)
        .await
        .map(Json)
}

async fn proxy_create_transfer(
    AxumPath(peer_id): AxumPath<String>,
    State(state): State<AppState>,
    Json(offer): Json<TransferOffer>,
) -> Result<Json<TransferProgress>, ApiError> {
    if offer.target_node_id != peer_id {
        return Err(ApiError::bad_request("目标设备标识不匹配"));
    }
    let base = proxy_peer_base(&state, &peer_id)?;
    read_proxy_peer_info(&state, &base, &peer_id).await?;
    let response = state
        .runtime
        .proxy_http
        .post(format!("{base}/api/v1/transfers"))
        .json(&offer)
        .send()
        .await
        .map_err(|error| ApiError::internal(format!("无法连接目标设备：{error}")))?;
    proxy_json(response).await
}

async fn proxy_transfer_status(
    AxumPath((peer_id, transfer_id)): AxumPath<(String, String)>,
    State(state): State<AppState>,
) -> Result<Json<TransferProgress>, ApiError> {
    let base = proxy_peer_base(&state, &peer_id)?;
    let response = state
        .runtime
        .proxy_http
        .get(format!("{base}/api/v1/transfers/{transfer_id}/status"))
        .send()
        .await
        .map_err(|error| ApiError::internal(format!("无法连接目标设备：{error}")))?;
    proxy_json(response).await
}

async fn proxy_transfer_action(
    peer_id: String,
    transfer_id: String,
    action: &str,
    state: &AppState,
) -> Result<Json<TransferProgress>, ApiError> {
    let base = proxy_peer_base(state, &peer_id)?;
    let response = state
        .runtime
        .proxy_http
        .post(format!("{base}/api/v1/transfers/{transfer_id}/{action}"))
        .send()
        .await
        .map_err(|error| ApiError::internal(format!("无法连接目标设备：{error}")))?;
    proxy_json(response).await
}

async fn proxy_reject_transfer(
    AxumPath((peer_id, transfer_id)): AxumPath<(String, String)>,
    State(state): State<AppState>,
) -> Result<Json<TransferProgress>, ApiError> {
    proxy_transfer_action(peer_id, transfer_id, "reject", &state).await
}

async fn proxy_cancel_transfer(
    AxumPath((peer_id, transfer_id)): AxumPath<(String, String)>,
    State(state): State<AppState>,
) -> Result<Json<TransferProgress>, ApiError> {
    proxy_transfer_action(peer_id, transfer_id, "cancel", &state).await
}

async fn proxy_fail_transfer(
    AxumPath((peer_id, transfer_id)): AxumPath<(String, String)>,
    State(state): State<AppState>,
    Json(payload): Json<MobileTransferFailureRequest>,
) -> Result<Json<TransferProgress>, ApiError> {
    let base = proxy_peer_base(&state, &peer_id)?;
    let response = state
        .runtime
        .proxy_http
        .post(format!("{base}/api/v1/transfers/{transfer_id}/fail"))
        .json(&payload)
        .send()
        .await
        .map_err(|error| ApiError::internal(format!("无法连接目标设备：{error}")))?;
    proxy_json(response).await
}

async fn proxy_retry_transfer(
    AxumPath((peer_id, transfer_id)): AxumPath<(String, String)>,
    State(state): State<AppState>,
) -> Result<Json<TransferProgress>, ApiError> {
    proxy_transfer_action(peer_id, transfer_id, "retry", &state).await
}

async fn proxy_upload_item(
    AxumPath((peer_id, transfer_id, item_id)): AxumPath<(String, String, String)>,
    State(state): State<AppState>,
    body: Body,
) -> Result<Json<TransferProgress>, ApiError> {
    let base = proxy_peer_base(&state, &peer_id)?;
    let stream = body
        .into_data_stream()
        .map(|chunk| chunk.map_err(|error| std::io::Error::other(error.to_string())));
    let response = state
        .runtime
        .proxy_http
        .put(format!(
            "{base}/api/v1/transfers/{transfer_id}/items/{item_id}"
        ))
        .body(reqwest::Body::wrap_stream(stream))
        .send()
        .await
        .map_err(|error| ApiError::internal(format!("无法连接目标设备：{error}")))?;
    proxy_json(response).await
}

async fn connect_peer(
    State(state): State<AppState>,
    Json(payload): Json<PeerConnectionRequest>,
) -> Result<Json<PeerInfo>, ApiError> {
    if payload.url.len() > 4_096 {
        return Err(ApiError::bad_request("节点地址过长"));
    }
    discovery::connect_manual(state.runtime, payload.url)
        .await
        .map(Json)
        .map_err(ApiError::bad_request)
}

async fn clipboard_state(State(state): State<AppState>) -> Json<Option<ClipboardSnapshot>> {
    Json(state.runtime.clipboard_snapshot())
}

async fn clipboard_history(State(state): State<AppState>) -> Json<ClipboardHistoryList> {
    Json(state.runtime.clipboard_history_list())
}

async fn clipboard_history_detail(
    State(state): State<AppState>,
    AxumPath(event_id): AxumPath<String>,
) -> Result<Json<ClipboardHistoryDetail>, ApiError> {
    if event_id.is_empty() || event_id.len() > 256 {
        return Err(ApiError::bad_request("剪贴板历史 ID 无效"));
    }
    state
        .runtime
        .clipboard_history_detail(&event_id)
        .map(Json)
        .ok_or_else(|| ApiError::not_found("剪贴板历史不存在"))
}

async fn clipboard_history_thumbnail(
    State(state): State<AppState>,
    AxumPath(event_id): AxumPath<String>,
) -> Result<Response, ApiError> {
    if event_id.is_empty() || event_id.len() > 256 {
        return Err(ApiError::bad_request("剪贴板历史 ID 无效"));
    }
    let bytes = state
        .runtime
        .clipboard_history_thumbnail(&event_id)
        .ok_or_else(|| ApiError::not_found("剪贴板历史缩略图不可用"))?;
    let mut response = bytes.into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("image/png"));
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}

async fn restore_clipboard_history(
    State(state): State<AppState>,
    AxumPath(event_id): AxumPath<String>,
) -> Result<Json<ClipboardSnapshot>, ApiError> {
    if event_id.is_empty() || event_id.len() > 256 {
        return Err(ApiError::bad_request("剪贴板历史 ID 无效"));
    }
    let event = state
        .runtime
        .restore_clipboard_history(&event_id)
        .ok_or_else(|| ApiError::not_found("剪贴板历史不存在或内容无效"))?;
    publish_local_clipboard(&state.runtime, event).map(Json)
}

async fn clear_clipboard_history(
    State(state): State<AppState>,
) -> Result<Json<ClipboardHistoryCleared>, ApiError> {
    state
        .runtime
        .clear_clipboard_history()
        .map(|cleared| Json(ClipboardHistoryCleared { cleared }))
        .map_err(|error| ApiError::internal(error.to_string()))
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
    if image_bytes
        .as_ref()
        .is_some_and(|value| !clipboard::is_valid_png(value))
    {
        return Err(ApiError::bad_request("image_png 不是有效的 PNG 图片"));
    }
    if payload
        .text
        .as_ref()
        .is_some_and(|value| value.len() > CLIPBOARD_TEXT_LIMIT)
    {
        return Err(ApiError::payload_too_large("文字超过 16 MiB 限制"));
    }
    if payload
        .source_name
        .as_ref()
        .is_some_and(|value| value.len() > 256)
    {
        return Err(ApiError::bad_request("剪贴板来源名称过长"));
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
    publish_local_clipboard(&state.runtime, event).map(Json)
}

/// 把已接受的本机剪贴板事件发给界面与其他节点。
fn publish_local_clipboard(
    runtime: &NodeRuntime,
    event: ClipboardEvent,
) -> Result<ClipboardSnapshot, ApiError> {
    let snapshot = runtime
        .clipboard_snapshot()
        .ok_or_else(|| ApiError::internal("剪贴板状态不可用"))?;
    runtime.publish_event(WireMessage::ClipboardUpdate {
        event: event.clone(),
    });
    runtime.send_to_peers_except(None, WireMessage::ClipboardUpdate { event });
    Ok(snapshot)
}

async fn websocket(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    connect_info: Option<ConnectInfo<SocketAddr>>,
) -> impl IntoResponse {
    ws.max_message_size(32 * 1024 * 1024)
        .max_frame_size(32 * 1024 * 1024)
        .on_upgrade(move |socket| {
            handle_socket(socket, state.runtime, connect_info.map(|info| info.0))
        })
}

async fn handle_socket(
    socket: axum::extract::ws::WebSocket,
    runtime: Arc<NodeRuntime>,
    remote_address: Option<SocketAddr>,
) {
    let (mut sender, mut receiver) = socket.split();
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel::<WireMessage>(64);
    let mut events = runtime.subscribe();
    let hello = WireMessage::Hello {
        node: runtime.node_info(),
    };
    if !matches!(
        tokio::time::timeout(
            Duration::from_secs(10),
            sender.send(axum::extract::ws::Message::Text(
                serde_json::to_string(&hello).unwrap_or_default(),
            )),
        )
        .await,
        Ok(Ok(()))
    ) {
        return;
    }
    let mut peer_session = None;
    let mut ui_client = false;
    let mut clipboard_resync = tokio::time::interval(std::time::Duration::from_secs(1));
    // 客户端必须尽快表明身份。未认证的连接一旦保留，
    // 接收任务与广播订阅就会一直挂着，
    // 而该连接永远不会成为可用的对端通道。
    let handshake_deadline = tokio::time::sleep(PEER_HANDSHAKE_TIMEOUT);
    tokio::pin!(handshake_deadline);

    loop {
        let peer_id = peer_session.as_ref().map(PeerSession::node_id);
        tokio::select! {
            _ = &mut handshake_deadline, if peer_id.is_none() && !ui_client => break,
            incoming = receiver.next() => {
                match incoming {
                    Some(Ok(axum::extract::ws::Message::Text(text))) => {
                        let Ok(message) = serde_json::from_str::<WireMessage>(&text) else {
                            break;
                        };
                        match message {
                            WireMessage::Ping { at_ms } if peer_id.is_none() => {
                                // UI 通道用 Ping 表明身份，并继续接收本地事件。
                                ui_client = true;
                                let pong = serde_json::to_string(&WireMessage::Pong { at_ms })
                                    .unwrap_or_default();
                                if !matches!(
                                    tokio::time::timeout(
                                        Duration::from_secs(10),
                                        sender.send(axum::extract::ws::Message::Text(pong)),
                                    )
                                    .await,
                                    Ok(Ok(()))
                                ) {
                                    break;
                                }
                            }
                            WireMessage::Hello { node } if peer_id.is_none() => {
                                let Some(peer) = incoming_peer_from_hello(node, &runtime, remote_address) else {
                                    break;
                                };
                                let Some(session) = runtime.attach_peer_session(peer, outgoing_tx.clone()) else {
                                    break;
                                };
                                sync_peer_state(&runtime, session.node_id()).await;
                                peer_session = Some(session);
                            }
                            message => {
                                let is_hello = matches!(&message, WireMessage::Hello { .. });
                                if handle_wire_message(message, &runtime, peer_id).await.is_none() && is_hello {
                                    break;
                                }
                            }
                        }
                    }
                    Some(Ok(axum::extract::ws::Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
            outbound = outgoing_rx.recv() => {
                if let Some(message) = outbound {
                    if !matches!(
                        tokio::time::timeout(
                            Duration::from_secs(10),
                            sender.send(axum::extract::ws::Message::Text(serde_json::to_string(&message).unwrap_or_default())),
                        )
                        .await,
                        Ok(Ok(()))
                    ) {
                        break;
                    }
                }
            }
            _ = clipboard_resync.tick() => {
                if runtime.stopping.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
                if let Some(peer_id) = peer_id {
                    if runtime
                        .peers
                        .read()
                        .get(peer_id)
                        .is_some_and(|peer| {
                            crate::clock::now_millis().saturating_sub(peer.last_seen_ms) > 30_000
                        })
                    {
                        break;
                    }
                    let _ = runtime.flush_clipboard_resync(peer_id);
                    let _ = runtime.flush_history_deletions(peer_id);
                }
            }
            event = events.recv() => {
                match event {
                    Ok(message) => {
                        if peer_id.is_some() {
                            continue;
                        }
                        if !matches!(
                            tokio::time::timeout(
                                Duration::from_secs(10),
                                sender.send(axum::extract::ws::Message::Text(serde_json::to_string(&message).unwrap_or_default())),
                            )
                            .await,
                            Ok(Ok(()))
                        ) {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}

/// 接受远端剪贴板内容。能写系统剪贴板的节点同时向其他对端转发，只读节点仅更新本地快照。
fn accept_remote_clipboard(
    runtime: &Arc<NodeRuntime>,
    peer_id: &str,
    event: ClipboardEvent,
    source_name: String,
) {
    let can_write_system_clipboard = runtime.supports_capability("clipboard.text");
    if !can_write_system_clipboard && !runtime.supports_capability(CLIPBOARD_VIEW_CAPABILITY) {
        return;
    }
    if !runtime.accept_clipboard(event.clone(), source_name) {
        return;
    }
    runtime.publish_event(WireMessage::ClipboardUpdate {
        event: event.clone(),
    });
    if can_write_system_clipboard {
        runtime.send_to_peers_except(Some(peer_id), WireMessage::ClipboardUpdate { event });
    }
}

fn incoming_peer_from_hello(
    node: NodeInfo,
    runtime: &NodeRuntime,
    remote_address: Option<SocketAddr>,
) -> Option<PeerInfo> {
    let endpoint = remote_address
        .filter(|address| is_allowed_peer_address(address.ip()))
        .map(|address| format!("http://{}:{}/api/v1/ws", address.ip(), node.port));
    peer_from_hello(node, runtime, endpoint)
}

pub(crate) fn peer_from_hello(
    node: NodeInfo,
    runtime: &NodeRuntime,
    endpoint: Option<String>,
) -> Option<PeerInfo> {
    if !validate_node_info(&node)
        || node.node_id == runtime.node_id()
        || node.protocol_version != crate::model::PROTOCOL_VERSION
    {
        return None;
    }
    let mobile_addresses = node.mobile_addresses;
    let addresses = node
        .addresses
        .into_iter()
        .filter_map(|mut address| {
            is_allowed_advertised_address(&address).then(|| {
                address.url = format!("http://{}:{}", address.address, node.port);
                address
            })
        })
        .collect::<Vec<_>>();
    let endpoint = endpoint.or_else(|| {
        addresses
            .first()
            .map(|address| format!("{}/api/v1/ws", address.url))
    });
    Some(PeerInfo {
        node_id: node.node_id,
        node_name: node.node_name,
        platform: node.platform,
        addresses,
        mobile_addresses,
        capabilities: node.capabilities,
        online: true,
        last_seen_ms: crate::clock::now_millis(),
        endpoint,
        source: "websocket".to_string(),
        metrics: None,
        metrics_received_at_ms: None,
        metrics_stale: false,
    })
}

pub(crate) async fn sync_peer_state(runtime: &Arc<NodeRuntime>, node_id: &str) {
    if let Some(snapshot) = runtime.clipboard_snapshot() {
        let _ = runtime
            .send_to_peer_with_retry(node_id, WireMessage::ClipboardSnapshot { snapshot })
            .await;
    }
    replay_active_offers(runtime, node_id).await;
    let _ = runtime.flush_history_deletions(node_id);
}

pub(crate) async fn handle_wire_message(
    message: WireMessage,
    runtime: &Arc<NodeRuntime>,
    current_peer: Option<&str>,
) -> Option<String> {
    if let Some(peer_id) = current_peer {
        runtime.touch_peer(peer_id);
    }
    match message {
        WireMessage::Hello { node } => {
            if current_peer.map_or_else(
                || runtime.has_peer_channel(&node.node_id),
                |peer_id| peer_id != node.node_id,
            ) {
                return None;
            }
            let peer = peer_from_hello(node, runtime, None)?;
            let node_id = peer.node_id.clone();
            runtime.set_peer(peer);
            Some(node_id)
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
            if validate_clipboard_event(&event) {
                let source_name = runtime
                    .peer_name(peer_id)
                    .unwrap_or_else(|| "远端设备".to_string());
                accept_remote_clipboard(runtime, peer_id, event, source_name);
            }
            Some(peer_id.to_string())
        }
        WireMessage::ClipboardSnapshot { snapshot } => {
            let peer_id = current_peer?;
            if snapshot.source_name.len() > 256 {
                return Some(peer_id.to_string());
            }
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
            if validate_clipboard_event(&event) {
                accept_remote_clipboard(runtime, peer_id, event, source_name);
            }
            Some(peer_id.to_string())
        }
        WireMessage::TransferOffer { offer } => {
            let peer_id = current_peer?;
            if offer.source_node_id != peer_id
                || offer.target_node_id != runtime.node_id()
                || validate_offer(&offer, true).is_err()
            {
                return Some(peer_id.to_string());
            }
            let transfer_id = offer.transfer_id.clone();
            let added = runtime.add_transfer(offer);
            if let Some(record) = runtime.transfer(&transfer_id) {
                let progress = record.progress.clone();
                let _ = runtime
                    .send_to_peer_with_retry(
                        peer_id,
                        WireMessage::TransferProgress {
                            progress: progress.clone(),
                        },
                    )
                    .await;
                if added
                    && matches!(
                        progress.status,
                        TransferStatus::Accepted | TransferStatus::Uploading
                    )
                {
                    let _ = runtime
                        .send_to_peer_with_retry(
                            peer_id,
                            WireMessage::TransferAction {
                                transfer_id,
                                action: "accept".to_string(),
                                reason: None,
                                retry_count: None,
                            },
                        )
                        .await;
                }
            }
            Some(peer_id.to_string())
        }
        WireMessage::TransferAction {
            transfer_id,
            action,
            reason,
            retry_count,
        } => {
            if reason
                .as_ref()
                .is_some_and(|value| value.len() > MAX_WIRE_ERROR_LENGTH)
            {
                return current_peer.map(str::to_string);
            }
            let _ = apply_transfer_action(
                runtime,
                &transfer_id,
                &action,
                reason,
                retry_count,
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
                    let valid = runtime.transfer(&transfer_id).is_some_and(|record| {
                        validate_transfer_progress_payload(&progress)
                            && progress.total_bytes == record.offer.total_bytes
                            && progress.total_items == record.offer.items.len()
                            && progress.completed_bytes <= progress.total_bytes
                            && progress.completed_items <= progress.total_items
                            && progress_counts_match_status(&progress)
                            && progress_is_monotonic(&record.progress, &progress)
                    });
                    if valid {
                        runtime.update_transfer(&transfer_id, progress);
                    }
                }
            } else {
                return current_peer.map(str::to_string);
            }
            current_peer.map(str::to_string)
        }
        WireMessage::TransferHistoryDeleted { transfer_ids } => {
            let peer_id = current_peer?;
            if valid_history_delete_ids(&transfer_ids) {
                let mut seen = HashSet::new();
                let ids = transfer_ids
                    .into_iter()
                    .filter(|id| seen.insert(id.clone()))
                    .collect::<Vec<_>>();
                if runtime.forget_transfer_history_from_peer(peer_id, &ids) > 0 {
                    // 界面连接会收到这条事件并立即重新读取历史。
                    runtime
                        .publish_event(WireMessage::TransferHistoryDeleted { transfer_ids: ids });
                }
            }
            Some(peer_id.to_string())
        }
        WireMessage::MetricsUpdate { metrics } => {
            let peer_id = current_peer?;
            if validate_metrics(&metrics)
                && (runtime.peer_supports(peer_id, METRICS_SYSTEM_CAPABILITY)
                    || runtime.peer_supports(peer_id, METRICS_NETWORK_CAPABILITY))
                && runtime.update_peer_metrics(peer_id, metrics.clone())
            {
                runtime.publish_event(WireMessage::MetricsUpdate { metrics });
            }
            Some(peer_id.to_string())
        }
    }
}

pub(crate) async fn replay_active_offers(runtime: &Arc<NodeRuntime>, peer_id: &str) {
    for progress in runtime.transfers_snapshot() {
        let Some(offer) = progress.offer else {
            continue;
        };
        if offer.source_node_id != runtime.node_id()
            || offer.target_node_id != peer_id
            || !matches!(
                progress.status,
                TransferStatus::Waiting | TransferStatus::Accepted | TransferStatus::Uploading
            )
        {
            continue;
        }
        let _ = runtime
            .send_to_peer_with_retry(peer_id, WireMessage::TransferOffer { offer })
            .await;
    }
}

async fn create_transfer(
    State(state): State<AppState>,
    Json(mut offer): Json<TransferOffer>,
) -> Result<Json<TransferProgress>, ApiError> {
    validate_offer(&offer, false)?;
    if offer.target_node_id != state.runtime.node_id() {
        return Err(ApiError::misdirected("地址上的设备与传输目标不匹配"));
    }
    offer.created_at_ms = crate::clock::now_millis();
    state.runtime.add_transfer(offer.clone());
    let progress = state
        .runtime
        .transfer(&offer.transfer_id)
        .map(|record| record.progress)
        .ok_or_else(|| ApiError::internal("无法创建传输"))?;
    Ok(Json(progress))
}

async fn list_transfers(State(state): State<AppState>) -> Json<Vec<TransferProgress>> {
    Json(state.runtime.transfers_snapshot())
}

async fn list_transfer_history(
    Query(query): Query<TransferHistoryQuery>,
    State(state): State<AppState>,
) -> Json<Vec<TransferHistoryEntry>> {
    Json(
        state
            .runtime
            .transfer_history_snapshot(query.session_id.as_deref()),
    )
}

#[derive(Debug, Serialize)]
struct TransferHistoryClearedBody {
    cleared: usize,
}

async fn clear_transfer_history(
    Query(query): Query<TransferHistoryQuery>,
    State(state): State<AppState>,
) -> Result<Json<TransferHistoryClearedBody>, ApiError> {
    let cleared = state
        .runtime
        .clear_transfer_history(query.session_id.as_deref())
        .map_err(|error| ApiError::internal(format!("清空传输历史失败：{error}")))?;
    for (peer_id, ids) in cleared.peer_batches {
        state.runtime.queue_history_deletions(&peer_id, ids);
    }
    Ok(Json(TransferHistoryClearedBody {
        cleared: cleared.cleared,
    }))
}

/// 清单携带摘要时必须与写入内容一致；留空时以写入内容为准。
fn digest_matches(claimed: &str, computed: &str) -> bool {
    claimed.is_empty() || claimed.eq_ignore_ascii_case(computed)
}

/// 摘要校验规则：非空必须合法；require_digest=false 允许留空，由节点写入文件体时计算回填。
fn validate_offer(offer: &TransferOffer, require_digest: bool) -> Result<(), ApiError> {
    if offer.transfer_id.trim().is_empty()
        || offer.source_node_id.trim().is_empty()
        || offer.target_node_id.trim().is_empty()
        || offer.transfer_id.len() > 256
        || offer.source_node_id.len() > 256
        || offer.target_node_id.len() > 256
    {
        return Err(ApiError::bad_request("传输清单缺少节点或传输标识"));
    }
    if offer.source_name.len() > 1_024 {
        return Err(ApiError::bad_request("传输来源名称过长"));
    }
    let listed_bytes = offer
        .items
        .iter()
        .try_fold(0u64, |total, item| total.checked_add(item.size));
    if offer.items.is_empty() || listed_bytes != Some(offer.total_bytes) {
        return Err(ApiError::bad_request("传输清单为空或总大小不匹配"));
    }
    let mut item_ids = HashSet::new();
    let mut relative_paths = HashSet::new();
    for item in &offer.items {
        let digest_ok = if item.sha256.is_empty() {
            !require_digest
        } else {
            is_sha256(&item.sha256)
        };
        if !digest_ok
            || item.item_id.trim().is_empty()
            || item.item_id.len() > 256
            || item.name.trim().is_empty()
            || item.name.len() > 1_024
            || item.media_type.len() > MAX_MEDIA_TYPE_LENGTH
            || item.directory
            || has_parent_component(&item.relative_path)
            || item.relative_path.is_empty()
            || item.relative_path.len() > 4_096
            || item.relative_path.contains('\0')
            || item.name.contains('\0')
            || !item_ids.insert(item.item_id.clone())
            || !relative_paths.insert(normalize_relative_path(&item.relative_path))
        {
            return Err(ApiError::bad_request("传输清单包含无效项目"));
        }
    }
    Ok(())
}

fn validate_clipboard_event(event: &ClipboardEvent) -> bool {
    if event.event_id.trim().is_empty()
        || event.origin_node_id.trim().is_empty()
        || event.event_id.len() > 256
        || event.origin_node_id.len() > 256
        || event.clock.node_id != event.origin_node_id
        || (event.text.is_none() && event.image_png.is_none())
        || event
            .text
            .as_ref()
            .is_some_and(|text| text.len() > CLIPBOARD_TEXT_LIMIT)
    {
        return false;
    }
    let Some(image_png) = event.image_png.as_deref() else {
        return true;
    };
    let Ok(bytes) = BASE64.decode(image_png) else {
        return false;
    };
    bytes.len() <= CLIPBOARD_IMAGE_LIMIT && clipboard::is_valid_png(&bytes)
}

fn validate_transfer_progress_payload(progress: &TransferProgress) -> bool {
    progress
        .error
        .as_ref()
        .is_none_or(|value| value.len() <= MAX_WIRE_ERROR_LENGTH)
        && progress
            .destination
            .as_ref()
            .is_none_or(|value| value.len() <= 4_096)
        && progress
            .direction
            .as_deref()
            .is_none_or(|value| matches!(value, "send" | "receive"))
}

fn validate_metrics(metrics: &HardwareMetrics) -> bool {
    !metrics.node_id.trim().is_empty()
        && metrics.node_id.len() <= 256
        && metrics.cpu_model.as_ref().is_none_or(|value| {
            !value.trim().is_empty() && value.len() <= MAX_METRIC_CPU_MODEL_LENGTH
        })
        && metrics
            .cpu_logical_cores
            .is_none_or(|value| (1..=4096).contains(&value))
        && metrics
            .cpu_physical_cores
            .is_none_or(|value| (1..=4096).contains(&value))
        && metrics
            .cpu_frequency_mhz
            .is_none_or(|value| (1..=100_000).contains(&value))
        && metrics.memory_available_bytes.is_none_or(|value| {
            metrics
                .memory_total_bytes
                .is_none_or(|total| value <= total)
        })
        && metrics
            .swap_used_bytes
            .is_none_or(|value| metrics.swap_total_bytes.is_none_or(|total| value <= total))
        && metrics.load_average.as_ref().is_none_or(|value| {
            [value.one, value.five, value.fifteen]
                .iter()
                .all(|item| item.is_finite() && *item >= 0.0)
        })
        && metrics.gpus.len() <= MAX_METRIC_GPU_COUNT
        && metrics.gpus.iter().all(|gpu| {
            !gpu.name.trim().is_empty()
                && gpu.name.len() <= MAX_METRIC_GPU_NAME_LENGTH
                && gpu
                    .unavailable_reason
                    .as_ref()
                    .is_none_or(|value| value.len() <= MAX_METRIC_REASON_LENGTH)
                && gpu
                    .utilization_percent
                    .is_none_or(|value| value.is_finite() && (0.0..=100.0).contains(&value))
        })
        && metrics.disks.len() <= MAX_METRIC_DISK_COUNT
        && metrics.disks.iter().all(|disk| {
            !disk.name.trim().is_empty()
                && disk.name.len() <= MAX_METRIC_DISK_NAME_LENGTH
                && disk.mount_point.len() <= MAX_METRIC_DISK_PATH_LENGTH
                && disk.file_system.len() <= MAX_METRIC_FILESYSTEM_LENGTH
                && disk.available_bytes <= disk.total_bytes
        })
}

pub(crate) fn validate_node_info(node: &NodeInfo) -> bool {
    !node.node_id.trim().is_empty()
        && node.node_id.len() <= 256
        && node.node_name.len() <= 256
        && node.version.len() <= 64
        && node.platform.len() <= 64
        && node.addresses.len() <= 32
        && node.mobile_addresses.len() <= 32
        && node.capabilities.len() <= 64
        && node.addresses.iter().all(|address| {
            address.address.len() <= 64
                && address.url.len() <= 512
                && address.label.len() <= 256
                && address.kind.len() <= 64
        })
        && node.mobile_addresses.iter().all(|address| {
            address.address.len() <= 64
                && address.url.len() <= 512
                && address.label.len() <= 256
                && address.kind.len() <= 64
                && is_allowed_advertised_address(address)
                && address
                    .url
                    .starts_with(&format!("https://{}:", address.address))
        })
        && node
            .capabilities
            .iter()
            .all(|capability| !capability.is_empty() && capability.len() <= 128)
}

fn is_allowed_advertised_address(address: &AddressInfo) -> bool {
    let Ok(ip) = address.address.parse::<std::net::IpAddr>() else {
        return false;
    };
    match ip {
        std::net::IpAddr::V4(ip) => {
            ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || (u32::from(ip) & 0xffc0_0000) == 0x6440_0000
        }
        std::net::IpAddr::V6(_) => false,
    }
}

fn is_allowed_peer_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => address.is_loopback() || crate::runtime::is_network_ipv4(address),
        IpAddr::V6(_) => false,
    }
}

fn normalize_relative_path(value: &str) -> String {
    Path::new(value)
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

pub(crate) fn encode_path_segment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push('%');
            encoded.push_str(&format!("{byte:02X}"));
        }
    }
    encoded
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn progress_is_monotonic(current: &TransferProgress, next: &TransferProgress) -> bool {
    if next.completed_bytes < current.completed_bytes
        || next.completed_items < current.completed_items
    {
        return false;
    }
    if next.retry_count < current.retry_count {
        return false;
    }
    if next.retry_count > current.retry_count {
        return current.status == TransferStatus::Failed
            && next.retry_count == current.retry_count.saturating_add(1)
            && matches!(
                next.status,
                TransferStatus::Waiting | TransferStatus::Accepted | TransferStatus::Uploading
            );
    }
    if matches!(
        current.status,
        TransferStatus::Completed
            | TransferStatus::Rejected
            | TransferStatus::Failed
            | TransferStatus::Cancelled
    ) {
        return next.status == current.status;
    }
    match current.status {
        TransferStatus::Waiting => true,
        TransferStatus::Accepted => matches!(
            next.status,
            TransferStatus::Accepted
                | TransferStatus::Uploading
                | TransferStatus::Completed
                | TransferStatus::Ready
                | TransferStatus::Rejected
                | TransferStatus::Failed
                | TransferStatus::Cancelled
        ),
        TransferStatus::Uploading => matches!(
            next.status,
            TransferStatus::Uploading
                | TransferStatus::Completed
                | TransferStatus::Ready
                | TransferStatus::Failed
                | TransferStatus::Cancelled
        ),
        TransferStatus::Ready => matches!(
            next.status,
            TransferStatus::Ready | TransferStatus::Completed | TransferStatus::Cancelled
        ),
        TransferStatus::Rejected
        | TransferStatus::Failed
        | TransferStatus::Cancelled
        | TransferStatus::Completed => false,
    }
}

fn progress_counts_match_status(progress: &TransferProgress) -> bool {
    !matches!(
        progress.status,
        TransferStatus::Completed | TransferStatus::Ready
    ) || (progress.completed_bytes == progress.total_bytes
        && progress.completed_items == progress.total_items)
}

async fn create_mobile_session(
    State(state): State<AppState>,
    Json(payload): Json<MobileSessionCreateRequest>,
) -> Result<Json<MobileSession>, ApiError> {
    cleanup_mobile_staging(&state).await;
    let label = payload
        .label
        .filter(|label| !label.trim().is_empty())
        .unwrap_or_else(|| "手机浏览器".to_string());
    if label.len() > 256 {
        return Err(ApiError::bad_request("手机会话名称过长"));
    }
    state
        .runtime
        .create_mobile_session(label)
        .map(Json)
        .map_err(ApiError::conflict)
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
        state.runtime.touch_mobile_session_transfers(session_id);
    }
    cleanup_mobile_staging(&state).await;
    Ok(Json(
        state
            .runtime
            .mobile_transfers_snapshot(query.session_id.as_deref()),
    ))
}

async fn cleanup_mobile_staging(state: &AppState) {
    state.runtime.prune_expired_mobile_sessions();
    for directory in state.runtime.prune_mobile_transfers() {
        if let Err(error) = fs::remove_dir_all(directory).await {
            if error.kind() != std::io::ErrorKind::NotFound {
                eprintln!("清理手机临时目录失败：{error}");
            }
        }
    }
}

async fn cleanup_mobile_staging_directory(directory: &Path) {
    let Ok(metadata) = fs::symlink_metadata(directory).await else {
        return;
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return;
    }
    if let Err(error) = fs::remove_dir_all(directory).await {
        if error.kind() != std::io::ErrorKind::NotFound {
            eprintln!("清理手机临时目录失败：{error}");
        }
    }
}

async fn remove_file_with_diagnostics(path: &Path) -> bool {
    match fs::remove_file(path).await {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => {
            eprintln!("清理临时文件失败 {}：{error}", path.display());
            false
        }
    }
}

/// 持有一个已登记的桌面端上传临时文件，直到提交或被显式清理。
/// 同步的 drop 路径覆盖请求 future
/// 在请求体仍在流式传输时被取消的情况。
struct DesktopTempFileGuard {
    runtime: Arc<NodeRuntime>,
    path: PathBuf,
    linked_target: Option<PathBuf>,
    committed: bool,
    armed: bool,
}

impl DesktopTempFileGuard {
    fn new(runtime: Arc<NodeRuntime>, path: PathBuf) -> Self {
        Self {
            runtime,
            path,
            linked_target: None,
            committed: false,
            armed: true,
        }
    }

    fn mark_linked(&mut self, target: PathBuf) {
        self.linked_target = Some(target);
    }

    fn mark_committed(&mut self) {
        self.committed = true;
    }

    async fn cleanup(&mut self) {
        if !self.armed {
            return;
        }
        let temp_removed = remove_file_with_diagnostics(&self.path).await;
        let target_removed = if self.committed {
            true
        } else if let Some(target) = self.linked_target.as_deref() {
            remove_file_with_diagnostics(target).await
        } else {
            true
        };
        if temp_removed && target_removed {
            self.runtime.unregister_desktop_temp_file(&self.path);
            self.armed = false;
        }
    }
}

impl Drop for DesktopTempFileGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let temp_removed = match std::fs::remove_file(&self.path) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
            Err(error) => {
                eprintln!("清理临时文件失败 {}：{error}", self.path.display());
                false
            }
        };
        let target_removed = if self.committed {
            true
        } else if let Some(target) = self.linked_target.as_deref() {
            match std::fs::remove_file(target) {
                Ok(()) => true,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
                Err(error) => {
                    eprintln!("清理已提交目标文件失败 {}：{error}", target.display());
                    false
                }
            }
        } else {
            true
        };
        if temp_removed && target_removed {
            self.runtime.unregister_desktop_temp_file(&self.path);
        }
    }
}

async fn create_mobile_transfer(
    State(state): State<AppState>,
    Json(mut payload): Json<MobileTransferCreateRequest>,
) -> Result<Json<MobileTransferView>, ApiError> {
    validate_offer(&payload.offer, false)?;
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
    let record = state
        .runtime
        .mobile_transfer(&id)
        .ok_or_else(|| ApiError::not_found("手机传输不存在"))?;
    let should_cleanup = matches!(
        record.progress.status,
        TransferStatus::Waiting
            | TransferStatus::Accepted
            | TransferStatus::Uploading
            | TransferStatus::Ready
    );
    let progress = state
        .runtime
        .set_mobile_transfer_status(&id, &payload.session_id, TransferStatus::Rejected)
        .ok_or_else(|| ApiError::not_found("手机传输不存在"))?;
    if should_cleanup {
        cleanup_mobile_staging_directory(&record.staged_directory).await;
    }
    Ok(Json(progress))
}

async fn cancel_mobile_transfer(
    AxumPath(id): AxumPath<String>,
    State(state): State<AppState>,
    Json(payload): Json<MobileSessionRequest>,
) -> Result<Json<MobileTransferView>, ApiError> {
    state
        .runtime
        .touch_mobile_session(&payload.session_id)
        .ok_or_else(|| ApiError::not_found("手机会话已过期"))?;
    let record = state
        .runtime
        .mobile_transfer(&id)
        .ok_or_else(|| ApiError::not_found("手机传输不存在"))?;
    let should_cleanup = matches!(
        record.progress.status,
        TransferStatus::Waiting
            | TransferStatus::Accepted
            | TransferStatus::Uploading
            | TransferStatus::Ready
    );
    let progress = state
        .runtime
        .cancel_mobile_transfer(&id, &payload.session_id)
        .ok_or_else(|| ApiError::not_found("手机传输不存在"))?;
    if should_cleanup {
        cleanup_mobile_staging_directory(&record.staged_directory).await;
    }
    Ok(Json(progress))
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
    if message.len() > MAX_WIRE_ERROR_LENGTH {
        return Err(ApiError::bad_request("失败原因过长"));
    }
    let progress = state
        .runtime
        .fail_mobile_transfer(&id, message)
        .ok_or_else(|| ApiError::not_found("手机传输不存在"))?;
    if matches!(
        record.progress.status,
        TransferStatus::Waiting | TransferStatus::Accepted | TransferStatus::Uploading
    ) {
        cleanup_mobile_staging_directory(&record.staged_directory).await;
    }
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
    if state
        .runtime
        .touch_mobile_session(&record.session_id)
        .is_none()
    {
        return Err(ApiError::not_found("手机会话已过期"));
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
    ensure_directory_without_symlink(&record.staged_directory).await?;
    let target = prepare_target(&record.staged_directory, &item.relative_path).await?;
    state
        .runtime
        .begin_mobile_upload(&transfer_id)
        .ok_or_else(|| ApiError::conflict("手机传输已结束"))?;
    let temp = target.with_extension(format!("meshshare-{}.part", uuid::Uuid::new_v4()));
    let file = match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .await
    {
        Ok(file) => file,
        Err(error) => {
            let message = format!("无法创建手机临时文件：{error}");
            let _ = state
                .runtime
                .fail_mobile_transfer(&transfer_id, message.clone());
            cleanup_mobile_staging_directory(&record.staged_directory).await;
            return Err(ApiError::internal(message));
        }
    };
    let stream_result = stream_body_to_file(
        body,
        file,
        item.size,
        &record.staged_directory,
        |_chunk_bytes| {
            if state
                .runtime
                .touch_mobile_session(&record.session_id)
                .is_none()
            {
                return Err(ApiError::not_found("手机会话已过期"));
            }
            state.runtime.touch_mobile_transfer_activity(&transfer_id);
            Ok(())
        },
    )
    .await;
    let (total, digest) = match stream_result {
        Ok(value) => value,
        Err(error) => {
            remove_file_with_diagnostics(&temp).await;
            return Err(error);
        }
    };
    if total != item.size || !digest_matches(&item.sha256, &digest) {
        remove_file_with_diagnostics(&temp).await;
        let _ = state
            .runtime
            .fail_mobile_transfer(&transfer_id, "文件校验失败".to_string());
        cleanup_mobile_staging_directory(&record.staged_directory).await;
        return Err(ApiError::bad_request("文件校验失败"));
    }
    let target = match prepare_target(&record.staged_directory, &item.relative_path).await {
        Ok(target) => target,
        Err(error) => {
            remove_file_with_diagnostics(&temp).await;
            return Err(error);
        }
    };
    if let Err(error) = fs::hard_link(&temp, &target).await {
        remove_file_with_diagnostics(&temp).await;
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            return Err(ApiError::conflict(format!(
                "目标已存在：{}",
                target.display()
            )));
        }
        return Err(ApiError::internal(error.to_string()));
    }
    remove_file_with_diagnostics(&temp).await;
    let Some(progress) = state
        .runtime
        .stage_mobile_item(&transfer_id, &item_id, total, digest)
    else {
        remove_file_with_diagnostics(&target).await;
        return Err(ApiError::conflict("手机传输已取消或完成"));
    };
    Ok(Json(progress))
}

/// Windows 上，显式完成请求可能先于下载响应关闭文件句柄。
async fn remove_mobile_staged_file(path: &Path) {
    for attempt in 0..10 {
        match fs::remove_file(path).await {
            Ok(()) => return,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) if attempt == 9 => {
                eprintln!("清理网页暂存文件失败 {}：{error}", path.display());
                return;
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
}

/// 项目确认完成后的收尾：删除暂存文件，整单完成时清理暂存目录。
async fn cleanup_mobile_item_artifacts(runtime: &NodeRuntime, transfer_id: &str, item_id: &str) {
    if let Some(path) = runtime.mobile_item_path(transfer_id, item_id) {
        remove_mobile_staged_file(&path).await;
    }
    if let Some(record) = runtime.mobile_transfer(transfer_id) {
        if record.progress.status == TransferStatus::Completed
            && record.retryable_downloads.is_empty()
        {
            cleanup_mobile_staging_directory(&record.staged_directory).await;
        }
    }
}

async fn finish_mobile_item_download(
    runtime: &NodeRuntime,
    transfer_id: &str,
    item_id: &str,
) -> Option<MobileTransferView> {
    let view = runtime.mark_mobile_item_downloaded(transfer_id, item_id, false)?;
    cleanup_mobile_item_artifacts(runtime, transfer_id, item_id).await;
    Some(view)
}

async fn download_mobile_item(
    AxumPath((transfer_id, item_id)): AxumPath<(String, String)>,
    Query(query): Query<MobileSessionQuery>,
    method: Method,
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
    if !matches!(
        record.progress.status,
        TransferStatus::Ready | TransferStatus::Completed
    ) {
        return Err(ApiError::conflict("文件尚未准备好下载"));
    }
    let item = record
        .offer
        .items
        .iter()
        .find(|item| item.item_id == item_id)
        .ok_or_else(|| ApiError::not_found("传输项目不存在"))?;
    if record.downloaded_items.contains(&item_id) && !record.retryable_downloads.contains(&item_id)
    {
        return Err(ApiError::not_found("暂存文件已清理"));
    }
    let path = state
        .runtime
        .mobile_item_path(&transfer_id, &item_id)
        .ok_or_else(|| ApiError::conflict("文件尚未准备好下载"))?;
    let metadata = fs::symlink_metadata(&path)
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ApiError::conflict("暂存文件不可用"));
    }
    let canonical_path = fs::canonicalize(&path)
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?;
    let canonical_directory = fs::canonicalize(&record.staged_directory)
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?;
    if !canonical_path.starts_with(&canonical_directory) {
        return Err(ApiError::conflict("暂存文件路径越界"));
    }
    let file = fs::File::open(&canonical_path)
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?;
    let size = file
        .metadata()
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?
        .len();
    if size != item.size {
        return Err(ApiError::conflict("暂存文件大小不匹配"));
    }
    let (fallback_filename, encoded_filename) = content_disposition_filename(&item.name);
    let content_type = HeaderValue::from_str(&item.media_type)
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
    let content_disposition = HeaderValue::from_str(&format!(
        "attachment; filename=\"{}\"; filename*=UTF-8''{}",
        fallback_filename, encoded_filename
    ))
    .map_err(|error| ApiError::internal(error.to_string()))?;
    let runtime = state.runtime;
    let auto_complete = query.auto_complete && method == Method::GET;
    // Hyper 可能不去轮询空请求体，或在 Content-Length 处停止。
    // HTTP 下载没有浏览器侧确认，因此暂存数据要保留以便重试。
    if auto_complete && size == 0 {
        runtime.mark_mobile_item_downloaded(&transfer_id, &item_id, true);
    }
    let stream = ReaderStream::new(file.take(size)).scan(0u64, move |sent, chunk| {
        if let Ok(bytes) = &chunk {
            *sent += bytes.len() as u64;
            runtime.touch_mobile_session(&session_id);
            runtime.touch_mobile_transfer_activity(&transfer_id);
            if auto_complete && *sent == size {
                runtime.mark_mobile_item_downloaded(&transfer_id, &item_id, true);
            }
        }
        futures_util::future::ready(Some(chunk))
    });
    let response = (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CONTENT_LENGTH, HeaderValue::from(size)),
            (header::CONTENT_DISPOSITION, content_disposition),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        Body::from_stream(stream),
    )
        .into_response();
    Ok(response)
}

async fn complete_mobile_item(
    AxumPath((transfer_id, item_id)): AxumPath<(String, String)>,
    State(state): State<AppState>,
    Json(payload): Json<MobileSessionRequest>,
) -> Result<Json<MobileTransferView>, ApiError> {
    state
        .runtime
        .touch_mobile_session(&payload.session_id)
        .ok_or_else(|| ApiError::not_found("手机会话已过期"))?;
    let record = state
        .runtime
        .mobile_transfer(&transfer_id)
        .ok_or_else(|| ApiError::not_found("手机传输不存在"))?;
    if record.session_id != payload.session_id {
        return Err(ApiError::forbidden("手机会话不匹配"));
    }
    if !record
        .offer
        .items
        .iter()
        .any(|item| item.item_id == item_id)
    {
        return Err(ApiError::not_found("传输项目不存在"));
    }
    let view = finish_mobile_item_download(state.runtime.as_ref(), &transfer_id, &item_id)
        .await
        .ok_or_else(|| ApiError::conflict("文件尚未准备好下载"))?;
    Ok(Json(view))
}

async fn reject_transfer(
    AxumPath(id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<TransferProgress>, ApiError> {
    finish_transfer(
        &state,
        id,
        TransferStatus::Rejected,
        "reject",
        "接收端拒绝了传输",
    )
    .await
}

async fn accept_transfer(
    AxumPath(id): AxumPath<String>,
    State(state): State<AppState>,
    Json(payload): Json<TransferAcceptRequest>,
) -> Result<Json<TransferProgress>, ApiError> {
    if payload.destination.trim().is_empty() || payload.destination.len() > 4_096 {
        return Err(ApiError::bad_request("接收目录路径无效"));
    }
    accept_transfer_at(&state.runtime, id, PathBuf::from(payload.destination))
        .map(Json)
        .map_err(ApiError::bad_request)
}

async fn cancel_transfer(
    AxumPath(id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<TransferProgress>, ApiError> {
    finish_transfer(
        &state,
        id,
        TransferStatus::Cancelled,
        "cancel",
        "传输已取消",
    )
    .await
}

async fn fail_transfer(
    AxumPath(id): AxumPath<String>,
    State(state): State<AppState>,
    Json(payload): Json<MobileTransferFailureRequest>,
) -> Result<Json<TransferProgress>, ApiError> {
    let message = payload
        .error
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "文件上传失败，已停止重试".to_string());
    if message.len() > MAX_WIRE_ERROR_LENGTH {
        return Err(ApiError::bad_request("失败原因过长"));
    }
    finish_transfer(&state, id, TransferStatus::Failed, "fail", &message).await
}

/// 结束本机发起的传输并把结果通知对端；拒绝、取消与上传失败共用同一收尾路径。
async fn finish_transfer(
    state: &AppState,
    transfer_id: String,
    status: TransferStatus,
    action: &str,
    reason: &str,
) -> Result<Json<TransferProgress>, ApiError> {
    let progress = state
        .runtime
        .fail_transfer(&transfer_id, reason.to_string(), status)
        .ok_or_else(|| ApiError::not_found("传输不存在"))?;
    if let Some(source_node_id) = state.runtime.transfer_source_id(&transfer_id) {
        let _ = state
            .runtime
            .send_to_peer_with_retry(
                &source_node_id,
                WireMessage::TransferAction {
                    transfer_id,
                    action: action.to_string(),
                    reason: progress.error.clone(),
                    retry_count: Some(progress.retry_count),
                },
            )
            .await;
    }
    state.runtime.publish_event(WireMessage::TransferProgress {
        progress: progress.clone(),
    });
    Ok(Json(progress))
}

async fn dismiss_transfer(
    AxumPath(id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<TransferProgress>, ApiError> {
    let record = state
        .runtime
        .transfer(&id)
        .ok_or_else(|| ApiError::not_found("传输不存在"))?;
    if record.progress.status != TransferStatus::Failed {
        return Err(ApiError::conflict("只能移除失败的任务记录"));
    }
    let progress = state
        .runtime
        .remove_failed_transfer(&id)
        .ok_or_else(|| ApiError::conflict("传输任务已被移除"))?;
    state.runtime.remove_outgoing_staging(&id);
    Ok(Json(progress))
}

async fn dismiss_mobile_transfer(
    AxumPath(id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<TransferProgress>, ApiError> {
    let record = state
        .runtime
        .mobile_transfer(&id)
        .ok_or_else(|| ApiError::not_found("手机传输不存在"))?;
    if record.progress.status != TransferStatus::Failed {
        return Err(ApiError::conflict("只能移除失败的任务记录"));
    }
    let removed = state
        .runtime
        .remove_failed_mobile_transfer(&id)
        .ok_or_else(|| ApiError::conflict("手机传输已被移除"))?;
    cleanup_mobile_staging_directory(&removed.staged_directory).await;
    Ok(Json(removed.progress))
}

async fn retry_transfer(
    AxumPath(id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<TransferProgress>, ApiError> {
    let record = state
        .runtime
        .transfer(&id)
        .ok_or_else(|| ApiError::not_found("传输不存在"))?;
    if record.offer.target_node_id != state.runtime.node_id() {
        return Err(ApiError::bad_request("只有目标节点可以重试该传输"));
    }
    let status = if record
        .accepted_destination
        .as_deref()
        .is_some_and(|destination| validate_receive_directory(destination).is_ok())
    {
        TransferStatus::Accepted
    } else {
        TransferStatus::Waiting
    };
    let progress = state.runtime.retry_transfer(&id, status, None)?;
    if let Some(source_node_id) = state.runtime.transfer_source_id(&id) {
        let _ = state
            .runtime
            .send_to_peer_with_retry(
                &source_node_id,
                WireMessage::TransferProgress {
                    progress: progress.clone(),
                },
            )
            .await;
    }
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

fn is_loopback_request(connect_info: Option<&ConnectInfo<SocketAddr>>) -> bool {
    connect_info.is_some_and(|ConnectInfo(address)| address.ip().is_loopback())
}

/// 运行时在后台准备清单时复用与 HTTP 建单一致的清单校验。
pub(crate) fn validate_outgoing_offer(offer: &TransferOffer) -> Result<(), String> {
    validate_offer(offer, true).map_err(|error| error.message)
}

/// 桌面端按原路径直传：节点后台枚举、算摘要后自己发送清单，不再复制源文件。
async fn create_outgoing_paths(
    State(state): State<AppState>,
    connect_info: Option<ConnectInfo<SocketAddr>>,
    Json(payload): Json<OutgoingPathRequest>,
) -> Result<Json<OutgoingPrepareView>, ApiError> {
    if !is_loopback_request(connect_info.as_ref()) {
        return Err(ApiError::forbidden("暂存接口只允许本机调用"));
    }
    start_outgoing_paths(&state, payload)
}

/// 持有源文件的节点按本机绝对路径建立发送任务；本机直传与远程管理共用。
fn start_outgoing_paths(
    state: &AppState,
    payload: OutgoingPathRequest,
) -> Result<Json<OutgoingPrepareView>, ApiError> {
    if payload.target_node_id == state.runtime.node_id()
        || state
            .runtime
            .peer_http_base(&payload.target_node_id)
            .is_none()
    {
        return Err(ApiError::not_found("目标设备当前不可用"));
    }
    state
        .runtime
        .begin_outgoing_paths(payload.target_node_id, payload.paths)
        .map(Json)
        .map_err(ApiError::bad_request)
}

/// 远程管理页的文件浏览与路径发送只在本机开启「远程文件访问」后可用。
fn ensure_remote_file_access(state: &AppState) -> Result<(), ApiError> {
    if state.runtime.remote_file_access() {
        Ok(())
    } else {
        Err(ApiError::forbidden(
            "远程文件访问未启用，请在目标机器的设置中开启",
        ))
    }
}

fn browse_api_error(error: crate::browse::BrowseError) -> ApiError {
    match error {
        crate::browse::BrowseError::NotFound => ApiError::not_found("目录不存在"),
        crate::browse::BrowseError::NotDirectory => {
            ApiError::bad_request("需要目标机器上的绝对目录路径")
        }
        crate::browse::BrowseError::Denied(message) => {
            ApiError::forbidden(format!("无法读取该目录：{message}"))
        }
        crate::browse::BrowseError::Io(message) => {
            ApiError::internal(format!("无法读取该目录：{message}"))
        }
    }
}

async fn management_file_roots(
    State(state): State<AppState>,
) -> Result<Json<crate::browse::BrowseRoots>, ApiError> {
    ensure_remote_file_access(&state)?;
    Ok(Json(crate::browse::roots()))
}

async fn management_files(
    State(state): State<AppState>,
    Query(query): Query<BrowseQuery>,
) -> Result<Json<crate::browse::BrowseListing>, ApiError> {
    ensure_remote_file_access(&state)?;
    let path = query
        .path
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| ApiError::bad_request("需要目标机器上的绝对目录路径"))?;
    let offset = query.offset.unwrap_or(0);
    let limit = query.limit.unwrap_or(crate::browse::DEFAULT_BROWSE_ENTRIES);
    // 目录可能很大，读元数据放到阻塞线程，避免拖住 HTTP 运行时。
    let requested = PathBuf::from(path);
    let listing =
        tokio::task::spawn_blocking(move || crate::browse::list(&requested, offset, limit))
            .await
            .map_err(|error| ApiError::internal(format!("无法读取该目录：{error}")))?
            .map_err(browse_api_error)?;
    Ok(Json(listing))
}

/// 远程管理页在目标机器上按路径建立发送任务，文件由目标机器自己读取与上传。
async fn create_managed_outgoing_paths(
    State(state): State<AppState>,
    Json(payload): Json<OutgoingPathRequest>,
) -> Result<Json<OutgoingPrepareView>, ApiError> {
    ensure_remote_file_access(&state)?;
    start_outgoing_paths(&state, payload)
}

async fn managed_outgoing_prepare(
    AxumPath(transfer_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<OutgoingPrepareView>, ApiError> {
    ensure_remote_file_access(&state)?;
    prepare_view(&state, &transfer_id)
}

async fn cancel_managed_outgoing_prepare(
    AxumPath(transfer_id): AxumPath<String>,
    State(state): State<AppState>,
) -> Result<Json<OutgoingStagingCancelled>, ApiError> {
    ensure_remote_file_access(&state)?;
    Ok(cancel_prepare(&state, transfer_id))
}

async fn outgoing_prepare_status(
    AxumPath(transfer_id): AxumPath<String>,
    State(state): State<AppState>,
    connect_info: Option<ConnectInfo<SocketAddr>>,
) -> Result<Json<OutgoingPrepareView>, ApiError> {
    if !is_loopback_request(connect_info.as_ref()) {
        return Err(ApiError::forbidden("暂存接口只允许本机调用"));
    }
    prepare_view(&state, &transfer_id)
}

fn prepare_view(
    state: &AppState,
    transfer_id: &str,
) -> Result<Json<OutgoingPrepareView>, ApiError> {
    state
        .runtime
        .outgoing_prepare_view(transfer_id)
        .map(Json)
        .ok_or_else(|| ApiError::not_found("准备任务已结束"))
}

fn cancel_prepare(state: &AppState, transfer_id: String) -> Json<OutgoingStagingCancelled> {
    state.runtime.cancel_outgoing_prepare(&transfer_id);
    Json(OutgoingStagingCancelled { transfer_id })
}

/// 原生界面把待发送文件交给本机节点，此后的上传不再依赖页面状态。
async fn create_outgoing_staging(
    State(state): State<AppState>,
    connect_info: Option<ConnectInfo<SocketAddr>>,
    Json(mut offer): Json<TransferOffer>,
) -> Result<Json<OutgoingStagingView>, ApiError> {
    if !is_loopback_request(connect_info.as_ref()) {
        return Err(ApiError::forbidden("暂存接口只允许本机调用"));
    }
    validate_offer(&offer, false)?;
    if offer.source_node_id != state.runtime.node_id() {
        return Err(ApiError::bad_request("暂存来源必须是本机节点"));
    }
    if offer.target_node_id == state.runtime.node_id()
        || state
            .runtime
            .peer_http_base(&offer.target_node_id)
            .is_none()
    {
        return Err(ApiError::not_found("目标设备当前不可用"));
    }
    offer.created_at_ms = crate::clock::now_millis();
    let total_items = offer.items.len();
    let transfer_id = offer.transfer_id.clone();
    state
        .runtime
        .begin_outgoing_staging(offer)
        .map_err(ApiError::conflict)?;
    Ok(Json(OutgoingStagingView {
        transfer_id,
        staged_items: 0,
        total_items,
    }))
}

async fn upload_outgoing_item(
    AxumPath((transfer_id, item_id)): AxumPath<(String, String)>,
    State(state): State<AppState>,
    connect_info: Option<ConnectInfo<SocketAddr>>,
    body: Body,
) -> Result<Json<OutgoingStagingView>, ApiError> {
    if !is_loopback_request(connect_info.as_ref()) {
        return Err(ApiError::forbidden("暂存接口只允许本机调用"));
    }
    let item = state
        .runtime
        .outgoing_item(&transfer_id, &item_id)
        .ok_or_else(|| ApiError::not_found("暂存项目不存在"))?;
    if !state.runtime.is_outgoing_item_ready(&transfer_id, &item_id) {
        let target = state
            .runtime
            .outgoing_staged_target(&transfer_id, &item_id)
            .ok_or_else(|| ApiError::not_found("暂存项目不存在"))?;
        let directory = target
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| ApiError::internal("暂存路径无效"))?;
        let temp = target.with_extension("part");
        let file = match fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temp)
            .await
        {
            Ok(file) => file,
            Err(error) => {
                return Err(ApiError::internal(format!("无法创建暂存文件：{error}")));
            }
        };
        let streamed =
            stream_body_to_file(body, file, item.size, &directory, |_chunk_bytes| Ok(())).await;
        let (total, digest) = match streamed {
            Ok(value) => value,
            Err(error) => {
                let _ = fs::remove_file(&temp).await;
                return Err(error);
            }
        };
        if total != item.size || !digest_matches(&item.sha256, &digest) {
            let _ = fs::remove_file(&temp).await;
            return Err(ApiError::bad_request("源文件校验失败"));
        }
        fs::rename(&temp, &target)
            .await
            .map_err(|error| ApiError::internal(format!("提交暂存文件失败：{error}")))?;
        state
            .runtime
            .mark_outgoing_item_staged(&transfer_id, &item_id, digest);
    }
    let (staged_items, total_items) = state
        .runtime
        .outgoing_staging_counts(&transfer_id)
        .ok_or_else(|| ApiError::not_found("暂存任务不存在"))?;
    Ok(Json(OutgoingStagingView {
        transfer_id,
        staged_items,
        total_items,
    }))
}

async fn start_outgoing_staging(
    AxumPath(transfer_id): AxumPath<String>,
    State(state): State<AppState>,
    connect_info: Option<ConnectInfo<SocketAddr>>,
) -> Result<Json<TransferProgress>, ApiError> {
    if !is_loopback_request(connect_info.as_ref()) {
        return Err(ApiError::forbidden("暂存接口只允许本机调用"));
    }
    let offer = state
        .runtime
        .outgoing_offer_if_ready(&transfer_id)
        .map_err(ApiError::conflict)?;
    let target_node_id = offer.target_node_id.clone();
    let progress = state.runtime.add_outgoing_transfer(offer.clone());
    if progress.status == TransferStatus::Failed {
        return Err(ApiError::conflict(
            progress
                .error
                .clone()
                .unwrap_or_else(|| "无法创建传输".to_string()),
        ));
    }
    if !state.runtime.has_peer_channel(&target_node_id)
        || !state
            .runtime
            .send_to_peer_with_retry(&target_node_id, WireMessage::TransferOffer { offer })
            .await
    {
        if let Some(failed) = state.runtime.fail_transfer(
            &transfer_id,
            "目标设备暂时无法接收传输请求".to_string(),
            TransferStatus::Failed,
        ) {
            state
                .runtime
                .publish_event(WireMessage::TransferProgress { progress: failed });
        }
        return Err(ApiError::conflict("目标设备暂时无法接收传输请求"));
    }
    Ok(Json(progress))
}

async fn cancel_outgoing_staging(
    AxumPath(transfer_id): AxumPath<String>,
    State(state): State<AppState>,
    connect_info: Option<ConnectInfo<SocketAddr>>,
) -> Result<Json<OutgoingStagingCancelled>, ApiError> {
    if !is_loopback_request(connect_info.as_ref()) {
        return Err(ApiError::forbidden("暂存接口只允许本机调用"));
    }
    state.runtime.remove_outgoing_staging(&transfer_id);
    Ok(cancel_prepare(&state, transfer_id))
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
    let already_received = record.received_items.contains(&item_id);
    if !matches!(
        record.progress.status,
        TransferStatus::Accepted | TransferStatus::Uploading
    ) && !(record.progress.status == TransferStatus::Completed && already_received)
    {
        return Err(ApiError::conflict("传输尚未被接收端接受"));
    }
    let _item_lock = TransferItemLock::new(&state.runtime, &transfer_id, &item_id)?;
    // 接收方可能已在发送方丢失 HTTP 响应时提交了文件。
    // 当已提交条目及其最终文件仍然存在时，
    // 重复的 PUT 视为幂等确认。
    if let Some(progress) = state
        .runtime
        .transfer_item_already_received(&transfer_id, &item_id)
    {
        let target = resolve_target(&destination, &item.relative_path).await?;
        if existing_file_matches(&target, item.size, &item.sha256).await? {
            return Ok(Json(progress));
        }
        return Err(ApiError::conflict("已完成的传输项目与目标文件校验不一致"));
    }
    let target = prepare_target(&destination, &item.relative_path).await?;
    let progress = state
        .runtime
        .begin_transfer_upload(&transfer_id)
        .ok_or_else(|| ApiError::conflict("传输已结束"))?;
    state
        .runtime
        .publish_event(WireMessage::TransferProgress { progress });
    let temp = target.with_extension(format!("meshshare-{}.part", uuid::Uuid::new_v4()));
    if let Err(error) = state.runtime.register_desktop_temp_file(&temp) {
        let message = format!("无法登记桌面临时文件：{error}");
        if let Some(progress) =
            state
                .runtime
                .fail_transfer(&transfer_id, message.clone(), TransferStatus::Failed)
        {
            state
                .runtime
                .publish_event(WireMessage::TransferProgress { progress });
        }
        return Err(ApiError::internal(message));
    }
    let mut temp_guard = DesktopTempFileGuard::new(state.runtime.clone(), temp.clone());
    let file = match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .await
    {
        Ok(file) => file,
        Err(error) => {
            let message = format!("无法创建桌面临时文件：{error}");
            if let Some(progress) =
                state
                    .runtime
                    .fail_transfer(&transfer_id, message.clone(), TransferStatus::Failed)
            {
                state
                    .runtime
                    .publish_event(WireMessage::TransferProgress { progress });
            }
            return Err(ApiError::internal(message));
        }
    };
    let mut received = 0u64;
    let mut last_report = Instant::now();
    let mut reported = false;
    let stream_result = stream_body_to_file(body, file, item.size, &destination, |chunk_bytes| {
        if !state.runtime.transfer_upload_active(&transfer_id) {
            return Err(ApiError::conflict("传输已取消"));
        }
        state.runtime.touch_transfer_activity(&transfer_id);
        received = received.saturating_add(chunk_bytes);
        // 首个数据块立即上报，其余按节流窗口刷新，
        // 进度不必等窗口到期才离开起点。
        if !reported || last_report.elapsed() >= TRANSFER_PROGRESS_INTERVAL {
            if let Some(progress) =
                state
                    .runtime
                    .report_transfer_progress(&transfer_id, &item_id, received)
            {
                state
                    .runtime
                    .publish_event(WireMessage::TransferProgress { progress });
            }
            reported = true;
            last_report = Instant::now();
        }
        Ok(())
    })
    .await;
    let (total, digest) = match stream_result {
        Ok(value) => value,
        Err(error) => {
            temp_guard.cleanup().await;
            return Err(error);
        }
    };
    if total != item.size || !digest_matches(&item.sha256, &digest) {
        temp_guard.cleanup().await;
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
            temp_guard.cleanup().await;
            return Err(error);
        }
    };
    if !state.runtime.transfer_upload_active(&transfer_id) {
        temp_guard.cleanup().await;
        return Err(ApiError::conflict("传输已取消"));
    }
    // 提交后立即记录守卫的目标。
    // 若请求在传输标记完成之前被取消，
    // 守卫会删掉不完整的最终文件以及残留的临时文件。
    if let Err(error) = commit_desktop_temp_file(&temp, &target).await {
        temp_guard.cleanup().await;
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            let message = format!("目标已存在：{}", target.display());
            if let Some(progress) =
                state
                    .runtime
                    .fail_transfer(&transfer_id, message.clone(), TransferStatus::Failed)
            {
                state
                    .runtime
                    .publish_event(WireMessage::TransferProgress { progress });
            }
            return Err(ApiError::conflict(format!(
                "目标已存在：{}",
                target.display()
            )));
        }
        let message = format!("无法提交接收文件：{error}");
        if let Some(progress) =
            state
                .runtime
                .fail_transfer(&transfer_id, message.clone(), TransferStatus::Failed)
        {
            state
                .runtime
                .publish_event(WireMessage::TransferProgress { progress });
        }
        return Err(ApiError::internal(message));
    }
    temp_guard.mark_linked(target.clone());
    let Some(progress) = state
        .runtime
        .mark_item_received(&transfer_id, &item_id, total, digest)
    else {
        temp_guard.cleanup().await;
        return Err(ApiError::conflict("传输已取消或已完成"));
    };
    temp_guard.mark_committed();
    temp_guard.cleanup().await;
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
            // 瞬时断连后，重复请求可能与原始上传并发。
            // 因此在在途请求释放条目锁之后，
            // 才告知发送方重试是安全的。
            return Err(ApiError::too_early("文件项目正在上传"));
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
    let target = resolve_target(destination, relative_path).await?;
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

/// 提交一份校验通过的上传，且不覆盖已存在的文件。
///
/// 桌面端实现用硬链接，使最终文件名原子出现，
/// 临时文件守卫也能清掉未完成的提交。
/// Android 的应用专属 Download 存储通常由模拟/FUSE 存储支撑，
/// 这类存储会拒绝硬链接，尽管普通文件写入可以成功。
/// 因此 Android 退化为不覆盖的 rename 原语
/// （在较旧文件系统上是串行化的检查后 rename 回退）。
async fn commit_desktop_temp_file(temp: &Path, target: &Path) -> Result<(), std::io::Error> {
    match fs::hard_link(temp, target).await {
        Ok(()) => Ok(()),
        Err(error)
            if cfg!(target_os = "android")
                && matches!(
                    error.kind(),
                    std::io::ErrorKind::Unsupported
                        | std::io::ErrorKind::PermissionDenied
                        | std::io::ErrorKind::CrossesDevices
                ) =>
        {
            commit_android_temp_file(temp, target).await
        }
        Err(error) => Err(error),
    }
}

/// Android 的应用专属 Download 存储可能拒绝硬链接。
/// 回退提交保持串行化，避免两个传输都看到目标不存在、
/// 随后并发执行覆盖式 `rename`。
/// 对提供 `renameat2(RENAME_NOREPLACE)` 的内核/文件系统，
/// 改用该原语，即使其他进程触碰目标也能维持不覆盖保证。
static ANDROID_COMMIT_LOCK: OnceLock<AsyncMutex<()>> = OnceLock::new();

async fn commit_android_temp_file(temp: &Path, target: &Path) -> Result<(), std::io::Error> {
    let _guard = ANDROID_COMMIT_LOCK
        .get_or_init(|| AsyncMutex::new(()))
        .lock()
        .await;

    #[cfg(target_os = "android")]
    {
        match rename_noreplace(temp, target) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Err(error),
            Err(error)
                if matches!(
                    error.raw_os_error(),
                    Some(libc::ENOSYS) | Some(libc::EINVAL) | Some(libc::EOPNOTSUPP)
                ) => {}
            Err(error) => return Err(error),
        }
    }

    // 较旧的 Android 内核与部分模拟/FUSE 文件系统没有 renameat2。
    // 进程级锁仍能为本节点所有 MeshShare 传输闭合
    // 存在性检查与 rename 之间的竞态。
    match fs::symlink_metadata(target).await {
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "目标文件已存在",
        )),
        Err(check) if check.kind() == std::io::ErrorKind::NotFound => {
            fs::rename(temp, target).await
        }
        Err(check) => Err(check),
    }
}

#[cfg(target_os = "android")]
fn rename_noreplace(temp: &Path, target: &Path) -> Result<(), std::io::Error> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let temp = CString::new(temp.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "临时路径包含 NUL 字节")
    })?;
    let target = CString::new(target.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "目标路径包含 NUL 字节")
    })?;
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            temp.as_ptr(),
            libc::AT_FDCWD,
            target.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

async fn resolve_target(destination: &Path, relative_path: &str) -> Result<PathBuf, ApiError> {
    let metadata = fs::symlink_metadata(destination)
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ApiError::bad_request("接收目录包含符号链接或非目录"));
    }
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
    Ok(parent.join(file_name))
}

async fn existing_file_matches(
    path: &Path,
    expected_size: u64,
    expected_sha256: &str,
) -> Result<bool, ApiError> {
    let metadata = match fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(ApiError::internal(error.to_string())),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() != expected_size {
        return Ok(false);
    }
    let mut file = fs::File::open(path)
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|error| ApiError::internal(error.to_string()))?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(read as u64)
            .ok_or_else(|| ApiError::payload_too_large("文件大小溢出"))?;
        hasher.update(&buffer[..read]);
    }
    Ok(total == expected_size
        && format!("{:x}", hasher.finalize()).eq_ignore_ascii_case(expected_sha256))
}

async fn ensure_directory_without_symlink(path: &Path) -> Result<(), ApiError> {
    let mut missing = Vec::new();
    let mut current = path;
    loop {
        match fs::symlink_metadata(current).await {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(ApiError::bad_request("暂存目录包含符号链接或非目录"));
                }
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(current.to_path_buf());
                current = current
                    .parent()
                    .ok_or_else(|| ApiError::internal("暂存目录缺少父目录"))?;
            }
            Err(error) => return Err(ApiError::internal(error.to_string())),
        }
    }
    while let Some(directory) = missing.pop() {
        match fs::create_dir(&directory).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let metadata = fs::symlink_metadata(&directory)
                    .await
                    .map_err(|error| ApiError::internal(error.to_string()))?;
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(ApiError::bad_request("暂存目录包含符号链接或非目录"));
                }
            }
            Err(error) => return Err(ApiError::internal(error.to_string())),
        }
    }
    Ok(())
}

async fn apply_transfer_action(
    runtime: &Arc<NodeRuntime>,
    transfer_id: &str,
    action: &str,
    reason: Option<String>,
    retry_count: Option<u32>,
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
            // 目标目录由接收节点本地选择，
            // 绝不从对端消息中接受。
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
        "fail" => {
            let generation_matches = retry_count.is_none_or(|expected| {
                runtime
                    .transfer(transfer_id)
                    .is_some_and(|record| record.progress.retry_count == expected)
            });
            if generation_matches {
                if let Some(progress) = runtime.fail_transfer(
                    transfer_id,
                    reason.unwrap_or_else(|| "文件上传失败，已停止重试".to_string()),
                    TransferStatus::Failed,
                ) {
                    runtime.publish_event(WireMessage::TransferProgress { progress });
                }
            }
        }
        "retry" if current_peer == Some(source_node_id.as_str()) => {
            if let Ok(progress) =
                runtime.retry_transfer(transfer_id, TransferStatus::Waiting, retry_count)
            {
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

fn content_disposition_filename(raw: &str) -> (String, String) {
    let sanitized = raw
        .chars()
        .map(|value| {
            if value.is_control() || matches!(value, '/' | '\\') {
                '_'
            } else {
                value
            }
        })
        .collect::<String>();
    let sanitized = sanitized.trim();
    let unicode_name = if sanitized.is_empty() || matches!(sanitized, "." | "..") {
        "download"
    } else {
        sanitized
    };
    let fallback = unicode_name
        .chars()
        .map(|value| {
            if value.is_ascii_alphanumeric() || matches!(value, '.' | '-' | '_' | ' ') {
                value
            } else {
                '_'
            }
        })
        .collect::<String>();
    let fallback = if fallback.is_empty() {
        "download".to_string()
    } else {
        fallback
    };
    let mut encoded = String::with_capacity(unicode_name.len());
    for byte in unicode_name.as_bytes() {
        let allowed = byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'!' | b'#' | b'$' | b'&' | b'+' | b'-' | b'.' | b'^' | b'_' | b'`' | b'|' | b'~'
            );
        if allowed {
            encoded.push(*byte as char);
        } else {
            encoded.push('%');
            encoded.push(
                char::from_digit(u32::from(byte >> 4), 16)
                    .unwrap()
                    .to_ascii_uppercase(),
            );
            encoded.push(
                char::from_digit(u32::from(byte & 0x0f), 16)
                    .unwrap()
                    .to_ascii_uppercase(),
            );
        }
    }
    (fallback, encoded)
}

#[cfg(test)]
mod tests {
    use super::{
        content_disposition_filename, encode_path_segment, handle_wire_message,
        has_parent_component, incoming_peer_from_hello, is_allowed_origin, validate_metrics,
        validate_offer, MobileSessionRequest, PEER_HANDSHAKE_TIMEOUT,
    };
    use crate::model::{
        ClipboardEvent, ClipboardSnapshot, DiskMetrics, HardwareMetrics, Hlc, NodeInfo, PeerInfo,
        ReceiveMode, TransferHistoryEntry, TransferItem, TransferOffer, TransferProgress,
        TransferStatus, WireMessage, CLIPBOARD_TEXT_LIMIT, METRICS_SYSTEM_CAPABILITY,
        PROTOCOL_VERSION,
    };
    use crate::runtime::{NodeProfile, NodeRuntime};
    use axum::http::{HeaderValue, Request};
    use futures_util::{SinkExt, StreamExt};
    use sha2::{Digest, Sha256};
    use std::path::PathBuf;
    use std::time::Duration;
    use tokio::sync::mpsc;
    use tokio_tungstenite::connect_async;
    use uuid::Uuid;

    #[test]
    fn content_disposition_preserves_unicode_filename_with_ascii_fallback() {
        let (fallback, encoded) = content_disposition_filename("报告 2026\".txt");
        assert_eq!(fallback, "__ 2026_.txt");
        assert!(encoded.starts_with("%E6%8A%A5%E5%91%8A%202026%22.txt"));
        assert!(!encoded.contains('/'));
    }

    #[test]
    fn origin_policy_allows_lan_and_tauri_but_rejects_public_websites() {
        assert!(is_allowed_origin(&HeaderValue::from_static(
            "http://192.168.1.20:37880"
        )));
        assert!(is_allowed_origin(&HeaderValue::from_static(
            "tauri://localhost"
        )));
        assert!(!is_allowed_origin(&HeaderValue::from_static(
            "https://evil.example"
        )));
        assert!(!is_allowed_origin(&HeaderValue::from_static("null")));
    }

    #[tokio::test]
    async fn management_file_endpoints_require_the_local_switch() {
        let data_dir =
            std::env::temp_dir().join(format!("meshshare-browse-switch-{}", Uuid::new_v4()));
        let root = data_dir.join("root");
        std::fs::create_dir_all(&root).expect("create root");
        std::fs::write(root.join("note.txt"), b"data").expect("write file");
        let runtime = NodeRuntime::new(data_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let state = super::AppState {
            runtime: runtime.clone(),
            control: super::ControlPlane::runtime_only(runtime.clone()),
            static_root: data_dir.clone(),
        };
        let path = root.to_string_lossy().into_owned();
        let query = || super::BrowseQuery {
            path: Some(path.clone()),
            offset: None,
            limit: None,
        };
        let prepare = || super::OutgoingPathRequest {
            target_node_id: runtime.node_id(),
            paths: vec![path.clone()],
        };
        for status in [
            super::management_file_roots(super::State(state.clone()))
                .await
                .expect_err("roots require the switch")
                .status,
            super::management_files(super::State(state.clone()), super::Query(query()))
                .await
                .expect_err("listing requires the switch")
                .status,
            super::create_managed_outgoing_paths(
                super::State(state.clone()),
                super::Json(prepare()),
            )
            .await
            .expect_err("path sends require the switch")
            .status,
            super::managed_outgoing_prepare(
                super::AxumPath("missing".to_string()),
                super::State(state.clone()),
            )
            .await
            .expect_err("prepare status requires the switch")
            .status,
            super::cancel_managed_outgoing_prepare(
                super::AxumPath("missing".to_string()),
                super::State(state.clone()),
            )
            .await
            .expect_err("prepare cancel requires the switch")
            .status,
        ] {
            assert_eq!(status, super::StatusCode::FORBIDDEN);
        }
        assert!(!runtime.remote_file_access());
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn management_file_endpoints_read_directories_and_reject_bad_targets() {
        let data_dir =
            std::env::temp_dir().join(format!("meshshare-browse-list-{}", Uuid::new_v4()));
        let root = data_dir.join("root");
        let child = root.join("child");
        std::fs::create_dir_all(&child).expect("create directories");
        let file = root.join("note.txt");
        std::fs::write(&file, b"data").expect("write file");
        let runtime = NodeRuntime::new(data_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        runtime
            .set_remote_file_access(true)
            .expect("enable remote file access");
        let state = super::AppState {
            runtime: runtime.clone(),
            control: super::ControlPlane::runtime_only(runtime.clone()),
            static_root: data_dir.clone(),
        };
        let query = |value: PathBuf| super::BrowseQuery {
            path: Some(value.to_string_lossy().into_owned()),
            offset: None,
            limit: None,
        };

        let listing = super::management_files(
            super::State(state.clone()),
            super::Query(query(root.clone())),
        )
        .await
        .expect("list directory")
        .0;
        assert_eq!(
            listing.path,
            crate::fsutil::display_absolute_path(
                &std::fs::canonicalize(&root).expect("canonical root")
            )
        );
        assert!(listing
            .entries
            .iter()
            .any(|entry| entry.name == "child" && entry.directory));
        assert!(listing
            .entries
            .iter()
            .any(|entry| entry.name == "note.txt" && !entry.directory));

        let roots = super::management_file_roots(super::State(state.clone()))
            .await
            .expect("list roots")
            .0;
        assert!(!roots.roots.is_empty());

        assert_eq!(
            super::management_files(
                super::State(state.clone()),
                super::Query(query(file.clone())),
            )
            .await
            .expect_err("files are not directories")
            .status,
            super::StatusCode::BAD_REQUEST
        );
        assert_eq!(
            super::management_files(
                super::State(state.clone()),
                super::Query(query(root.join("missing"))),
            )
            .await
            .expect_err("missing directories are not listed")
            .status,
            super::StatusCode::NOT_FOUND
        );

        let prepare = |target: String| super::OutgoingPathRequest {
            target_node_id: target,
            paths: vec![file.to_string_lossy().into_owned()],
        };
        assert_eq!(
            super::create_managed_outgoing_paths(
                super::State(state.clone()),
                super::Json(prepare("unknown-peer".to_string())),
            )
            .await
            .expect_err("unknown peers cannot be targets")
            .status,
            super::StatusCode::NOT_FOUND
        );
        assert_eq!(
            super::create_managed_outgoing_paths(
                super::State(state.clone()),
                super::Json(prepare(runtime.node_id())),
            )
            .await
            .expect_err("the local node cannot be a target")
            .status,
            super::StatusCode::NOT_FOUND
        );
        assert_eq!(
            super::managed_outgoing_prepare(
                super::AxumPath("missing".to_string()),
                super::State(state.clone()),
            )
            .await
            .expect_err("finished prepares are gone")
            .status,
            super::StatusCode::NOT_FOUND
        );
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn settings_and_remote_accept_endpoints_update_runtime_state() {
        let data_dir =
            std::env::temp_dir().join(format!("meshshare-settings-api-{}", Uuid::new_v4()));
        let static_root = data_dir.join("web");
        let destination = data_dir.join("received");
        std::fs::create_dir_all(&static_root).expect("create static directory");
        std::fs::create_dir_all(&destination).expect("create destination");
        std::fs::write(static_root.join("index.html"), "<html></html>")
            .expect("write static index");
        let runtime = NodeRuntime::new(data_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        assert!(runtime
            .node_info()
            .capabilities
            .iter()
            .any(|value| value == "management.remote.v1"));
        assert!(runtime
            .node_info()
            .capabilities
            .iter()
            .any(|value| value == "management.files.v1"));
        let app = super::router(runtime.clone(), static_root);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let address = listener.local_addr().expect("listener address");
        let server_task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test router");
        });

        let settings = runtime
            .http
            .get(format!("http://{address}/api/v1/settings"))
            .send()
            .await
            .expect("get settings")
            .error_for_status()
            .expect("settings response")
            .json::<serde_json::Value>()
            .await
            .expect("decode settings");
        assert_eq!(settings["clipboard_sync_enabled"], true);
        assert_eq!(settings["update_frequency"], "weekly");
        assert_eq!(settings["monitor"]["capabilities"]["backend"], "unknown");

        let denied = runtime
            .http
            .patch(format!("http://{address}/api/v1/settings"))
            .header("Origin", "https://evil.example")
            .json(&serde_json::json!({ "clipboard_sync_enabled": false }))
            .send()
            .await
            .expect("send denied origin request");
        assert_eq!(denied.status(), reqwest::StatusCode::FORBIDDEN);

        let allowed = runtime
            .http
            .patch(format!("http://{address}/api/v1/settings"))
            .header("Origin", "http://192.168.1.10:37880")
            .json(&serde_json::json!({ "clipboard_sync_enabled": true }))
            .send()
            .await
            .expect("send allowed origin request");
        assert!(allowed.status().is_success());

        let patched = runtime
            .http
            .patch(format!("http://{address}/api/v1/settings"))
            .json(&serde_json::json!({
                "clipboard_sync_enabled": false,
                "update_frequency": "daily",
                "clipboard_history_settings": { "mode": "count", "value": 2 },
                "monitor": { "visible": false }
            }))
            .send()
            .await
            .expect("patch settings")
            .error_for_status()
            .expect("patched settings response")
            .json::<serde_json::Value>()
            .await
            .expect("decode patched settings");
        assert_eq!(patched["clipboard_sync_enabled"], false);
        assert_eq!(patched["update_frequency"], "daily");
        assert_eq!(patched["monitor"]["visible"], false);
        assert_eq!(runtime.clipboard_history_settings().count, 2);
        assert!(!runtime.monitor_visible());

        let offer = TransferOffer {
            transfer_id: "remote-accept".to_string(),
            source_node_id: "source".to_string(),
            source_name: "source".to_string(),
            target_node_id: runtime.node_id(),
            items: vec![TransferItem {
                item_id: "item".to_string(),
                name: "file.txt".to_string(),
                relative_path: "file.txt".to_string(),
                size: 0,
                sha256: "0".repeat(64),
                media_type: "text/plain".to_string(),
                directory: false,
            }],
            total_bytes: 0,
            created_at_ms: 0,
        };
        runtime.add_transfer(offer);
        let accepted = runtime
            .http
            .post(format!(
                "http://{address}/api/v1/transfers/remote-accept/accept"
            ))
            .json(&serde_json::json!({ "destination": destination.to_string_lossy() }))
            .send()
            .await
            .expect("accept remote transfer")
            .error_for_status()
            .expect("accepted transfer response")
            .json::<TransferProgress>()
            .await
            .expect("decode accepted transfer");
        assert_eq!(accepted.status, TransferStatus::Accepted);
        assert_eq!(
            runtime
                .transfer("remote-accept")
                .expect("transfer")
                .progress
                .status,
            TransferStatus::Accepted
        );

        let outgoing_offer = TransferOffer {
            transfer_id: "relay-source-accept".to_string(),
            source_node_id: runtime.node_id(),
            source_name: "source".to_string(),
            target_node_id: "peer".to_string(),
            items: vec![TransferItem {
                item_id: "item".to_string(),
                name: "file.txt".to_string(),
                relative_path: "file.txt".to_string(),
                size: 0,
                sha256: "0".repeat(64),
                media_type: "text/plain".to_string(),
                directory: false,
            }],
            total_bytes: 0,
            created_at_ms: 0,
        };
        runtime.add_outgoing_transfer(outgoing_offer);
        let denied = runtime
            .http
            .post(format!(
                "http://{address}/api/v1/transfers/relay-source-accept/accept"
            ))
            .json(&serde_json::json!({ "destination": destination.to_string_lossy() }))
            .send()
            .await
            .expect("reject source-side accept")
            .status();
        assert_eq!(denied, reqwest::StatusCode::BAD_REQUEST);

        runtime
            .stopping
            .store(true, std::sync::atomic::Ordering::SeqCst);
        server_task.abort();
        let _ = server_task.await;
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn incoming_websocket_uses_the_verified_remote_address_for_http_peer_endpoint() {
        let data_dir = std::env::temp_dir().join(format!(
            "meshshare-incoming-peer-endpoint-{}",
            Uuid::new_v4()
        ));
        let runtime = NodeRuntime::new(data_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let mut node = runtime.node_info();
        node.node_id = "incoming-peer".to_string();
        node.port = 37880;
        node.addresses.reverse();
        let peer = incoming_peer_from_hello(
            node,
            &runtime,
            Some("100.64.0.8:43122".parse().expect("remote socket address")),
        )
        .expect("valid incoming hello");
        let (tx, _rx) = mpsc::channel(1);
        let session = runtime
            .attach_peer_session(peer, tx)
            .expect("attach incoming peer");
        assert_eq!(session.node_id(), "incoming-peer");
        assert_eq!(
            runtime
                .peers_snapshot()
                .into_iter()
                .find(|peer| peer.node_id == "incoming-peer")
                .and_then(|peer| peer.endpoint),
            Some("http://100.64.0.8:37880/api/v1/ws".to_string())
        );
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[test]
    fn outgoing_staging_rejects_non_loopback_peers() {
        let loopback = "127.0.0.1:5000".parse().expect("loopback address");
        let lan = "192.168.1.5:5000".parse().expect("lan address");
        assert!(super::is_loopback_request(Some(&super::ConnectInfo(
            loopback
        ))));
        assert!(!super::is_loopback_request(Some(&super::ConnectInfo(lan))));
        assert!(!super::is_loopback_request(None));
    }

    #[tokio::test]
    async fn duplicate_transfer_offer_replays_current_progress_to_source() {
        let data_dir =
            std::env::temp_dir().join(format!("meshshare-offer-replay-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&data_dir).expect("create data directory");
        let runtime = NodeRuntime::new(data_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let (tx, mut rx) = mpsc::channel(8);
        assert!(runtime.add_peer_channel("source".to_string(), tx));
        let destination = data_dir.join("received");
        std::fs::create_dir_all(&destination).expect("create destination");
        let offer = TransferOffer {
            transfer_id: "duplicate-offer".to_string(),
            source_node_id: "source".to_string(),
            source_name: "source".to_string(),
            target_node_id: runtime.node_id(),
            items: vec![TransferItem {
                item_id: "item".to_string(),
                name: "file.txt".to_string(),
                relative_path: "file.txt".to_string(),
                size: 0,
                sha256: "0".repeat(64),
                media_type: "text/plain".to_string(),
                directory: false,
            }],
            total_bytes: 0,
            created_at_ms: 0,
        };
        let _ = handle_wire_message(
            WireMessage::TransferOffer {
                offer: offer.clone(),
            },
            &runtime,
            Some("source"),
        )
        .await;
        assert!(matches!(
            rx.recv().await,
            Some(WireMessage::TransferProgress { .. })
        ));
        runtime
            .set_transfer_acceptance("duplicate-offer", destination)
            .expect("accept transfer");
        let _ = handle_wire_message(
            WireMessage::TransferOffer { offer },
            &runtime,
            Some("source"),
        )
        .await;
        let Some(WireMessage::TransferProgress { progress }) = rx.recv().await else {
            panic!("duplicate offer should replay progress");
        };
        assert_eq!(progress.status, TransferStatus::Accepted);
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn outgoing_progress_tracks_the_receivers_decision_for_each_offer() {
        let data_dir =
            std::env::temp_dir().join(format!("meshshare-offer-reply-{}", Uuid::new_v4()));
        let sender = NodeRuntime::new(data_dir.join("sender"), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create sender");
        let receiver = NodeRuntime::new(
            data_dir.join("receiver"),
            0,
            env!("CARGO_PKG_VERSION").into(),
        )
        .expect("create receiver");
        let sender_id = sender.node_id();
        let receiver_id = receiver.node_id();
        let (tx, mut replies) = mpsc::channel(8);
        assert!(receiver.add_peer_channel(sender_id.clone(), tx));

        let new_offer = |id, expected_status| {
            let offer = TransferOffer {
                target_node_id: receiver_id.clone(),
                ..test_offer(&sender, id)
            };
            let initial = sender.add_outgoing_transfer(offer.clone());
            assert_eq!(initial.status, TransferStatus::Waiting);
            assert!(initial.awaiting_offer_reply);
            (offer, expected_status)
        };
        let mut relay = async |(offer, expected_status): (TransferOffer, TransferStatus)| {
            let id = offer.transfer_id.clone();
            handle_wire_message(
                WireMessage::TransferOffer { offer },
                &receiver,
                Some(&sender_id),
            )
            .await;
            let reply = replies.try_recv().expect("receiver reports task status");
            assert!(matches!(
                &reply,
                WireMessage::TransferProgress { progress }
                    if progress.status == expected_status && !progress.awaiting_offer_reply
            ));
            handle_wire_message(reply, &sender, Some(&receiver_id)).await;
            let progress = sender.transfer(&id).expect("outgoing task").progress;
            assert_eq!(progress.status, expected_status);
            assert_eq!(progress.direction.as_deref(), Some("send"));
            assert!(!progress.awaiting_offer_reply);
            while let Ok(message) = replies.try_recv() {
                handle_wire_message(message, &sender, Some(&receiver_id)).await;
            }
        };

        let manual = new_offer("manual", TransferStatus::Waiting);
        relay(manual.clone()).await;
        let destination = data_dir.join("received");
        std::fs::create_dir(&destination).expect("create receive directory");
        receiver
            .set_receive_settings(ReceiveMode::Auto, Some(destination.clone()))
            .expect("enable auto receive");
        relay(manual).await;
        relay(new_offer("auto", TransferStatus::Accepted)).await;
        std::fs::remove_dir(&destination).expect("make directory unavailable");
        relay(new_offer("fallback", TransferStatus::Waiting)).await;
        std::fs::create_dir(&destination).expect("restore receive directory");
        relay(new_offer("restored", TransferStatus::Accepted)).await;
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[test]
    fn rejects_parent_and_absolute_paths() {
        assert!(has_parent_component("../secret.txt"));
        assert!(has_parent_component("/tmp/secret.txt"));
        assert!(!has_parent_component("folder/secret.txt"));
    }

    #[test]
    fn path_segments_are_percent_encoded() {
        assert_eq!(encode_path_segment("a/b?c"), "a%2Fb%3Fc");
        assert_eq!(encode_path_segment("uuid-like_1"), "uuid-like_1");
    }

    #[test]
    fn rejects_duplicate_paths_and_invalid_hashes() {
        let offer = TransferOffer {
            transfer_id: "transfer".to_string(),
            source_node_id: "source".to_string(),
            source_name: "source".to_string(),
            target_node_id: "target".to_string(),
            items: vec![
                TransferItem {
                    item_id: "one".to_string(),
                    name: "file.txt".to_string(),
                    relative_path: "folder/file.txt".to_string(),
                    size: 0,
                    sha256: "0".repeat(64),
                    media_type: "text/plain".to_string(),
                    directory: false,
                },
                TransferItem {
                    item_id: "two".to_string(),
                    name: "file.txt".to_string(),
                    relative_path: "folder//file.txt".to_string(),
                    size: 0,
                    sha256: "invalid".to_string(),
                    media_type: "text/plain".to_string(),
                    directory: false,
                },
            ],
            total_bytes: 0,
            created_at_ms: 1,
        };
        assert!(validate_offer(&offer, true).is_err());
    }

    #[test]
    fn empty_digest_allowed_only_for_page_manifests() {
        let offer = TransferOffer {
            transfer_id: "page".to_string(),
            source_node_id: "source".to_string(),
            source_name: "source".to_string(),
            target_node_id: "target".to_string(),
            items: vec![TransferItem {
                item_id: "one".to_string(),
                name: "file.txt".to_string(),
                relative_path: "file.txt".to_string(),
                size: 1,
                sha256: String::new(),
                media_type: "text/plain".to_string(),
                directory: false,
            }],
            total_bytes: 1,
            created_at_ms: 1,
        };
        assert!(validate_offer(&offer, false).is_ok());
        assert!(validate_offer(&offer, true).is_err());
    }

    #[test]
    fn accepts_large_payloads_and_more_than_the_old_item_limit() {
        let digest = "0".repeat(64);
        let large_item = TransferItem {
            item_id: "large".to_string(),
            name: "large.bin".to_string(),
            relative_path: "large.bin".to_string(),
            size: 5 * 1024 * 1024 * 1024 + 1,
            sha256: digest.clone(),
            media_type: "application/octet-stream".to_string(),
            directory: false,
        };
        let large_offer = TransferOffer {
            transfer_id: "large-offer".to_string(),
            source_node_id: "source".to_string(),
            source_name: "source".to_string(),
            target_node_id: "target".to_string(),
            items: vec![large_item],
            total_bytes: 5 * 1024 * 1024 * 1024 + 1,
            created_at_ms: 0,
        };
        assert!(validate_offer(&large_offer, true).is_ok());

        let items = (0..4_097)
            .map(|index| TransferItem {
                item_id: format!("item-{index}"),
                name: format!("file-{index}.bin"),
                relative_path: format!("file-{index}.bin"),
                size: 0,
                sha256: digest.clone(),
                media_type: "application/octet-stream".to_string(),
                directory: false,
            })
            .collect::<Vec<_>>();
        let many_items = TransferOffer {
            transfer_id: "many-items".to_string(),
            source_node_id: "source".to_string(),
            source_name: "source".to_string(),
            target_node_id: "target".to_string(),
            items,
            total_bytes: 0,
            created_at_ms: 0,
        };
        assert!(validate_offer(&many_items, true).is_ok());
    }

    #[tokio::test]
    async fn invalid_remote_clipboard_payload_is_ignored() {
        let data_dir =
            std::env::temp_dir().join(format!("meshshare-invalid-clipboard-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&data_dir).expect("create test directory");
        let runtime = NodeRuntime::new(data_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let event = ClipboardEvent {
            event_id: "bad-image".to_string(),
            origin_node_id: "peer".to_string(),
            clock: Hlc {
                physical_ms: 1,
                logical: 0,
                node_id: "peer".to_string(),
            },
            text: None,
            image_png: Some("not-base64".to_string()),
            updated_at_ms: 1,
        };
        handle_wire_message(
            WireMessage::ClipboardUpdate { event },
            &runtime,
            Some("peer"),
        )
        .await;
        assert!(runtime.clipboard_snapshot().is_none());
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn clipboard_history_thumbnail_route_serves_png_and_hides_missing() {
        let data_dir =
            std::env::temp_dir().join(format!("meshshare-clipboard-thumbnail-{}", Uuid::new_v4()));
        let static_root = data_dir.join("web");
        std::fs::create_dir_all(&static_root).expect("create static directory");
        std::fs::write(static_root.join("index.html"), "<html></html>")
            .expect("write static index");
        let runtime = NodeRuntime::new(data_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let mut png = Vec::new();
        image::DynamicImage::new_rgba8(200, 100)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .expect("encode png");
        let event = runtime
            .create_local_clipboard_named(None, Some(png), "local".to_string())
            .expect("create image clipboard event");
        let app = super::router(runtime.clone(), static_root);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let address = listener.local_addr().expect("listener address");
        let server_task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test router");
        });

        let response = runtime
            .http
            .get(format!(
                "http://{address}/api/v1/clipboard/history/{}/thumbnail",
                event.event_id
            ))
            .send()
            .await
            .expect("fetch thumbnail")
            .error_for_status()
            .expect("thumbnail response");
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .and_then(|value| value.to_str().ok()),
            Some("image/png")
        );
        let bytes = response.bytes().await.expect("thumbnail bytes");
        assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        let thumbnail = image::load_from_memory(&bytes).expect("decode thumbnail");
        assert_eq!((thumbnail.width(), thumbnail.height()), (96, 72));

        let missing = runtime
            .http
            .get(format!(
                "http://{address}/api/v1/clipboard/history/missing/thumbnail"
            ))
            .send()
            .await
            .expect("fetch missing thumbnail");
        assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);

        server_task.abort();
        let _ = server_task.await;
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn android_view_only_peer_stores_remote_clipboard_without_system_write() {
        let data_dir = std::env::temp_dir().join(format!(
            "meshshare-android-view-clipboard-{}",
            Uuid::new_v4()
        ));
        std::fs::create_dir_all(&data_dir).expect("create test directory");
        let runtime = NodeRuntime::new_with_profile(
            data_dir.clone(),
            0,
            env!("CARGO_PKG_VERSION").into(),
            NodeProfile::Android,
        )
        .expect("create Android runtime");
        let event = ClipboardEvent {
            event_id: "android-view".to_string(),
            origin_node_id: "peer".to_string(),
            clock: Hlc {
                physical_ms: 1,
                logical: 0,
                node_id: "peer".to_string(),
            },
            text: Some("from desktop".to_string()),
            image_png: None,
            updated_at_ms: 1,
        };
        handle_wire_message(
            WireMessage::ClipboardUpdate { event },
            &runtime,
            Some("peer"),
        )
        .await;
        assert_eq!(
            runtime
                .clipboard_snapshot()
                .and_then(|snapshot| snapshot.text),
            Some("from desktop".to_string())
        );
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn oversized_remote_clipboard_text_is_ignored() {
        let data_dir =
            std::env::temp_dir().join(format!("meshshare-large-clipboard-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&data_dir).expect("create test directory");
        let runtime = NodeRuntime::new(data_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let event = ClipboardEvent {
            event_id: "large-text".to_string(),
            origin_node_id: "peer".to_string(),
            clock: Hlc {
                physical_ms: 1,
                logical: 0,
                node_id: "peer".to_string(),
            },
            text: Some("x".repeat(CLIPBOARD_TEXT_LIMIT + 1)),
            image_png: None,
            updated_at_ms: 1,
        };
        handle_wire_message(
            WireMessage::ClipboardUpdate { event },
            &runtime,
            Some("peer"),
        )
        .await;
        assert!(runtime.clipboard_snapshot().is_none());
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn oversized_remote_metrics_payload_is_ignored() {
        let data_dir =
            std::env::temp_dir().join(format!("meshshare-large-metrics-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&data_dir).expect("create test directory");
        let runtime = NodeRuntime::new(data_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        runtime.set_peer(PeerInfo {
            node_id: "peer".to_string(),
            node_name: "peer".to_string(),
            platform: "test".to_string(),
            addresses: Vec::new(),
            mobile_addresses: Vec::new(),
            capabilities: vec![METRICS_SYSTEM_CAPABILITY.to_string()],
            online: true,
            last_seen_ms: crate::clock::now_millis(),
            endpoint: None,
            source: "test".to_string(),
            metrics: None,
            metrics_received_at_ms: None,
            metrics_stale: false,
        });
        let metrics = HardwareMetrics {
            node_id: "peer".to_string(),
            sampled_at_ms: 1,
            cpu_usage_percent: Some(2.0),
            memory_used_bytes: None,
            memory_total_bytes: None,
            network_down_bytes_per_sec: None,
            network_up_bytes_per_sec: None,
            gpus: vec![crate::model::GpuMetrics {
                name: "x".repeat(257),
                utilization_percent: None,
                memory_used_bytes: None,
                memory_total_bytes: None,
                memory_kind: crate::model::GpuMemoryKind::Unknown,
                unavailable_reason: None,
            }],
            ..Default::default()
        };
        handle_wire_message(
            WireMessage::MetricsUpdate { metrics },
            &runtime,
            Some("peer"),
        )
        .await;
        assert!(runtime.peers_snapshot()[0].metrics.is_none());
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[test]
    fn invalid_extended_metrics_values_are_rejected() {
        let metrics = HardwareMetrics {
            node_id: "peer".to_string(),
            sampled_at_ms: 1,
            memory_total_bytes: Some(10),
            memory_available_bytes: Some(11),
            disks: vec![DiskMetrics {
                name: "System".to_string(),
                mount_point: "/".to_string(),
                file_system: "ext4".to_string(),
                total_bytes: 100,
                available_bytes: 101,
                removable: false,
            }],
            ..Default::default()
        };
        assert!(!validate_metrics(&metrics));
    }

    #[tokio::test]
    async fn invalid_hello_does_not_register_a_peer() {
        let data_dir =
            std::env::temp_dir().join(format!("meshshare-invalid-hello-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&data_dir).expect("create test directory");
        let runtime = NodeRuntime::new(data_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let result = handle_wire_message(
            WireMessage::Hello {
                node: NodeInfo {
                    node_id: String::new(),
                    node_name: "peer".to_string(),
                    version: "test".to_string(),
                    protocol_version: PROTOCOL_VERSION,
                    platform: "test".to_string(),
                    port: 0,
                    addresses: Vec::new(),
                    mobile_addresses: Vec::new(),
                    capabilities: Vec::new(),
                },
            },
            &runtime,
            None,
        )
        .await;
        assert!(result.is_none());
        assert!(runtime.peers_snapshot().is_empty());
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn duplicate_hello_does_not_replace_existing_peer_metadata() {
        let data_dir =
            std::env::temp_dir().join(format!("meshshare-duplicate-hello-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&data_dir).expect("create test directory");
        let runtime = NodeRuntime::new(data_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let hello = |node_name: &str| WireMessage::Hello {
            node: NodeInfo {
                node_id: "peer".to_string(),
                node_name: node_name.to_string(),
                version: "test".to_string(),
                protocol_version: PROTOCOL_VERSION,
                platform: "test".to_string(),
                port: 0,
                addresses: Vec::new(),
                mobile_addresses: Vec::new(),
                capabilities: Vec::new(),
            },
        };
        assert_eq!(
            handle_wire_message(hello("first"), &runtime, None)
                .await
                .as_deref(),
            Some("peer")
        );
        let (tx, _rx) = mpsc::channel(1);
        assert!(runtime.add_peer_channel("peer".to_string(), tx));
        assert!(handle_wire_message(hello("duplicate"), &runtime, None)
            .await
            .is_none());
        assert_eq!(runtime.peer_name("peer").as_deref(), Some("first"));
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn stale_transfer_progress_cannot_regress_a_terminal_state() {
        let data_dir =
            std::env::temp_dir().join(format!("meshshare-stale-progress-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&data_dir).expect("create test directory");
        let runtime = NodeRuntime::new(data_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let offer = TransferOffer {
            transfer_id: "progress-order".to_string(),
            source_node_id: "peer".to_string(),
            source_name: "peer".to_string(),
            target_node_id: runtime.node_id(),
            items: vec![TransferItem {
                item_id: "item".to_string(),
                name: "empty.txt".to_string(),
                relative_path: "empty.txt".to_string(),
                size: 0,
                sha256: "0".repeat(64),
                media_type: "text/plain".to_string(),
                directory: false,
            }],
            total_bytes: 0,
            created_at_ms: crate::clock::now_millis(),
        };
        runtime.add_transfer(offer);
        runtime
            .mark_item_received("progress-order", "item", 0, "0".repeat(64))
            .expect("complete transfer");
        handle_wire_message(
            WireMessage::TransferProgress {
                progress: TransferProgress {
                    transfer_id: "progress-order".to_string(),
                    retry_count: 0,
                    status: TransferStatus::Waiting,
                    awaiting_offer_reply: false,
                    completed_bytes: 0,
                    total_bytes: 0,
                    completed_items: 0,
                    total_items: 1,
                    destination: None,
                    error: None,
                    offer: None,
                    direction: None,
                },
            },
            &runtime,
            Some("peer"),
        )
        .await;
        assert_eq!(
            runtime
                .transfer("progress-order")
                .expect("find transfer")
                .progress
                .status,
            TransferStatus::Completed
        );
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn retry_progress_can_reopen_only_with_next_generation() {
        let data_dir =
            std::env::temp_dir().join(format!("meshshare-retry-progress-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&data_dir).expect("create test directory");
        let runtime = NodeRuntime::new(data_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let offer = TransferOffer {
            transfer_id: "retry-progress".to_string(),
            source_node_id: "peer".to_string(),
            source_name: "peer".to_string(),
            target_node_id: runtime.node_id(),
            items: vec![TransferItem {
                item_id: "item".to_string(),
                name: "empty.txt".to_string(),
                relative_path: "empty.txt".to_string(),
                size: 0,
                sha256: "0".repeat(64),
                media_type: "text/plain".to_string(),
                directory: false,
            }],
            total_bytes: 0,
            created_at_ms: crate::clock::now_millis(),
        };
        runtime.add_transfer(offer);
        runtime
            .fail_transfer(
                "retry-progress",
                "temporary".to_string(),
                TransferStatus::Failed,
            )
            .expect("fail transfer");
        handle_wire_message(
            WireMessage::TransferProgress {
                progress: TransferProgress {
                    transfer_id: "retry-progress".to_string(),
                    retry_count: 1,
                    status: TransferStatus::Accepted,
                    awaiting_offer_reply: false,
                    completed_bytes: 0,
                    total_bytes: 0,
                    completed_items: 0,
                    total_items: 1,
                    destination: None,
                    error: None,
                    offer: None,
                    direction: None,
                },
            },
            &runtime,
            Some("peer"),
        )
        .await;
        let record = runtime.transfer("retry-progress").expect("find transfer");
        assert_eq!(record.progress.status, TransferStatus::Accepted);
        assert_eq!(record.progress.retry_count, 1);
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn rejecting_mobile_transfer_removes_staged_directory() {
        let data_dir =
            std::env::temp_dir().join(format!("meshshare-mobile-reject-{}", Uuid::new_v4()));
        let static_root = data_dir.join("web");
        std::fs::create_dir_all(&static_root).expect("create static directory");
        std::fs::write(static_root.join("index.html"), "<html></html>")
            .expect("write static index");
        let runtime = NodeRuntime::new(data_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let session = runtime
            .create_mobile_session("test phone".to_string())
            .expect("create mobile session");
        let offer = TransferOffer {
            transfer_id: "mobile-reject-cleanup".to_string(),
            source_node_id: runtime.node_id(),
            source_name: "desktop".to_string(),
            target_node_id: runtime.node_id(),
            items: vec![TransferItem {
                item_id: "item".to_string(),
                name: "empty.txt".to_string(),
                relative_path: "empty.txt".to_string(),
                size: 0,
                sha256: "0".repeat(64),
                media_type: "text/plain".to_string(),
                directory: false,
            }],
            total_bytes: 0,
            created_at_ms: crate::clock::now_millis(),
        };
        runtime
            .create_mobile_transfer(&session.session_id, offer)
            .expect("create mobile transfer");
        let record = runtime
            .mobile_transfer("mobile-reject-cleanup")
            .expect("find mobile transfer");
        std::fs::create_dir_all(&record.staged_directory).expect("create staging directory");
        std::fs::write(record.staged_directory.join("stale.part"), b"stale")
            .expect("write staged file");

        let app = super::router(runtime.clone(), static_root);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let address = listener.local_addr().expect("listener address");
        let server_task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test router");
        });
        let response = runtime
            .http
            .post(format!(
                "http://{address}/api/v1/mobile/transfers/mobile-reject-cleanup/reject"
            ))
            .json(&serde_json::json!({ "session_id": session.session_id }))
            .send()
            .await
            .expect("reject mobile transfer");
        assert!(response.status().is_success());
        assert!(!record.staged_directory.exists());
        assert_eq!(
            runtime
                .mobile_transfer("mobile-reject-cleanup")
                .expect("find rejected transfer")
                .progress
                .status,
            TransferStatus::Rejected
        );

        runtime
            .stopping
            .store(true, std::sync::atomic::Ordering::SeqCst);
        server_task.abort();
        let _ = server_task.await;
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn mobile_checksum_failure_removes_staged_directory() {
        let data_dir = std::env::temp_dir().join(format!(
            "meshshare-mobile-upload-failure-{}",
            Uuid::new_v4()
        ));
        let static_root = data_dir.join("web");
        std::fs::create_dir_all(&static_root).expect("create static directory");
        std::fs::write(static_root.join("index.html"), "<html></html>")
            .expect("write static index");
        let runtime = NodeRuntime::new(data_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let session = runtime
            .create_mobile_session("test phone".to_string())
            .expect("create mobile session");
        let mut hasher = Sha256::new();
        hasher.update(b"expected");
        let offer = TransferOffer {
            transfer_id: "mobile-checksum-failure".to_string(),
            source_node_id: runtime.node_id(),
            source_name: "desktop".to_string(),
            target_node_id: runtime.node_id(),
            items: vec![TransferItem {
                item_id: "item".to_string(),
                name: "payload.bin".to_string(),
                relative_path: "payload.bin".to_string(),
                size: 8,
                sha256: format!("{:x}", hasher.finalize()),
                media_type: "application/octet-stream".to_string(),
                directory: false,
            }],
            total_bytes: 8,
            created_at_ms: crate::clock::now_millis(),
        };
        runtime
            .create_mobile_transfer(&session.session_id, offer)
            .expect("create mobile transfer");
        runtime
            .set_mobile_transfer_status(
                "mobile-checksum-failure",
                &session.session_id,
                TransferStatus::Accepted,
            )
            .expect("accept mobile transfer");
        let record = runtime
            .mobile_transfer("mobile-checksum-failure")
            .expect("find mobile transfer");

        let app = super::router(runtime.clone(), static_root);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let address = listener.local_addr().expect("listener address");
        let server_task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test router");
        });
        let response = runtime
            .http
            .put(format!(
                "http://{address}/api/v1/mobile/transfers/mobile-checksum-failure/items/item"
            ))
            .body("wrong!!!")
            .send()
            .await
            .expect("upload wrong mobile payload");
        assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
        assert!(!record.staged_directory.exists());
        assert_eq!(
            runtime
                .mobile_transfer("mobile-checksum-failure")
                .expect("find failed mobile transfer")
                .progress
                .status,
            TransferStatus::Failed
        );

        runtime
            .stopping
            .store(true, std::sync::atomic::Ordering::SeqCst);
        server_task.abort();
        let _ = server_task.await;
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn mobile_download_requires_explicit_completion_after_stream_finishes() {
        let data_dir =
            std::env::temp_dir().join(format!("meshshare-mobile-download-{}", Uuid::new_v4()));
        let static_root = data_dir.join("web");
        std::fs::create_dir_all(&static_root).expect("create static directory");
        std::fs::write(static_root.join("index.html"), "<html></html>")
            .expect("write static index");
        let runtime = NodeRuntime::new(data_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let session = runtime
            .create_mobile_session("test phone".to_string())
            .expect("create mobile session");
        let bytes = b"hello mobile";
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let offer = TransferOffer {
            transfer_id: "mobile-download".to_string(),
            source_node_id: runtime.node_id(),
            source_name: "desktop".to_string(),
            target_node_id: runtime.node_id(),
            items: vec![TransferItem {
                item_id: "item".to_string(),
                name: "hello.txt".to_string(),
                relative_path: "hello.txt".to_string(),
                size: bytes.len() as u64,
                sha256: format!("{:x}", hasher.finalize()),
                media_type: "text/plain".to_string(),
                directory: false,
            }],
            total_bytes: bytes.len() as u64,
            created_at_ms: crate::clock::now_millis(),
        };
        runtime
            .create_mobile_transfer(&session.session_id, offer)
            .expect("create mobile transfer");
        runtime
            .set_mobile_transfer_status(
                "mobile-download",
                &session.session_id,
                TransferStatus::Accepted,
            )
            .expect("accept mobile transfer");
        let record = runtime
            .mobile_transfer("mobile-download")
            .expect("find mobile transfer");
        std::fs::create_dir_all(&record.staged_directory).expect("create staged directory");
        std::fs::write(record.staged_directory.join("hello.txt"), bytes)
            .expect("write staged file");
        runtime
            .stage_mobile_item(
                "mobile-download",
                "item",
                bytes.len() as u64,
                format!("{:x}", Sha256::digest(bytes)),
            )
            .expect("stage mobile item");

        let app = super::router(runtime.clone(), static_root);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let address = listener.local_addr().expect("listener address");
        let server_task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test router");
        });
        let response = runtime
            .http
            .get(format!(
                "http://{address}/api/v1/mobile/transfers/mobile-download/items/item/download?session_id={}",
                session.session_id
            ))
            .send()
            .await
            .expect("download mobile item");
        assert!(response.status().is_success());
        assert_eq!(
            response
                .headers()
                .get("content-disposition")
                .and_then(|value| value.to_str().ok()),
            Some("attachment; filename=\"hello.txt\"; filename*=UTF-8''hello.txt")
        );
        let expected_content_length = bytes.len().to_string();
        assert_eq!(
            response
                .headers()
                .get("content-length")
                .and_then(|value| value.to_str().ok()),
            Some(expected_content_length.as_str())
        );
        assert_eq!(
            response.bytes().await.expect("read download").as_ref(),
            bytes
        );
        let completion = runtime
            .http
            .post(format!(
                "http://{address}/api/v1/mobile/transfers/mobile-download/items/item/complete"
            ))
            .json(&MobileSessionRequest {
                session_id: session.session_id.clone(),
            })
            .send()
            .await
            .expect("complete mobile item");
        assert!(completion.status().is_success());
        assert_eq!(
            runtime
                .mobile_transfer("mobile-download")
                .expect("find completed transfer")
                .progress
                .status,
            TransferStatus::Completed
        );
        runtime
            .stopping
            .store(true, std::sync::atomic::Ordering::SeqCst);
        server_task.abort();
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn direct_offer_for_a_different_node_is_not_registered() {
        let data_dir =
            std::env::temp_dir().join(format!("meshshare-foreign-offer-{}", Uuid::new_v4()));
        let runtime = NodeRuntime::new(data_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let offer = TransferOffer {
            transfer_id: "foreign-offer".to_string(),
            source_node_id: "demo-sender".to_string(),
            source_name: "demo sender".to_string(),
            target_node_id: "demo-other-node".to_string(),
            items: vec![TransferItem {
                item_id: "item".to_string(),
                name: "demo.txt".to_string(),
                relative_path: "demo.txt".to_string(),
                size: 0,
                sha256: format!("{:x}", Sha256::digest([])),
                media_type: "text/plain".to_string(),
                directory: false,
            }],
            total_bytes: 0,
            created_at_ms: crate::clock::now_millis(),
        };
        let error = super::create_transfer(
            super::State(super::AppState {
                control: super::ControlPlane::runtime_only(runtime.clone()),
                runtime: runtime.clone(),
                static_root: data_dir.clone(),
            }),
            super::Json(offer),
        )
        .await
        .expect_err("reject wrong target");
        assert_eq!(error.status, super::StatusCode::MISDIRECTED_REQUEST);
        assert!(runtime.transfer("foreign-offer").is_none());
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn browser_download_completes_and_keeps_staged_items_retryable() {
        let data_dir =
            std::env::temp_dir().join(format!("meshshare-browser-download-{}", Uuid::new_v4()));
        let static_root = data_dir.join("web");
        std::fs::create_dir_all(&static_root).expect("create static directory");
        std::fs::write(static_root.join("index.html"), "<html></html>")
            .expect("write static index");
        let runtime = NodeRuntime::new(data_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let session = runtime
            .create_mobile_session("test phone".to_string())
            .expect("create mobile session");
        let app = super::router(runtime.clone(), static_root.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let address = listener.local_addr().expect("listener address");
        let server_task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test router");
        });
        for (id, bytes) in [
            ("empty-download", vec![]),
            ("browser-download", vec![42; 32 * 1024]),
        ] {
            runtime
                .create_mobile_transfer(
                    &session.session_id,
                    TransferOffer {
                        transfer_id: id.to_string(),
                        source_node_id: runtime.node_id(),
                        source_name: "desktop".to_string(),
                        target_node_id: runtime.node_id(),
                        items: vec![TransferItem {
                            item_id: "item".to_string(),
                            name: "sample.bin".to_string(),
                            relative_path: "sample.bin".to_string(),
                            size: bytes.len() as u64,
                            sha256: format!("{:x}", Sha256::digest(&bytes)),
                            media_type: "application/octet-stream".to_string(),
                            directory: false,
                        }],
                        total_bytes: bytes.len() as u64,
                        created_at_ms: crate::clock::now_millis(),
                    },
                )
                .expect("create mobile transfer");
            runtime
                .set_mobile_transfer_status(id, &session.session_id, TransferStatus::Accepted)
                .expect("accept mobile transfer");
            let record = runtime.mobile_transfer(id).expect("find mobile transfer");
            std::fs::create_dir_all(&record.staged_directory).expect("create staged directory");
            let staged = record.staged_directory.join("sample.bin");
            std::fs::write(&staged, &bytes).expect("write staged file");
            runtime
                .stage_mobile_item(
                    id,
                    "item",
                    bytes.len() as u64,
                    format!("{:x}", Sha256::digest(&bytes)),
                )
                .expect("stage item");

            let url = format!("http://{address}/api/v1/mobile/transfers/{id}/items/item/download?session_id={}&auto_complete=true", session.session_id);
            assert!(runtime
                .http
                .head(&url)
                .send()
                .await
                .expect("head download")
                .status()
                .is_success());
            assert_eq!(
                runtime.mobile_transfer(id).unwrap().progress.status,
                TransferStatus::Ready
            );
            if !bytes.is_empty() {
                let response = super::download_mobile_item(
                    super::AxumPath((id.to_string(), "item".to_string())),
                    super::Query(super::MobileSessionQuery {
                        session_id: Some(session.session_id.clone()),
                        auto_complete: true,
                    }),
                    super::Method::GET,
                    super::State(super::AppState {
                        runtime: runtime.clone(),
                        control: super::ControlPlane::runtime_only(runtime.clone()),
                        static_root: static_root.clone(),
                    }),
                )
                .await
                .expect("start partial download");
                let mut body = response.into_body().into_data_stream();
                let first = body.next().await.expect("first chunk").expect("read chunk");
                assert!(first.len() < bytes.len());
                drop(body);
                assert_eq!(
                    runtime.mobile_transfer(id).unwrap().progress.status,
                    TransferStatus::Ready
                );
            }

            for _ in 0..2 {
                let response = runtime
                    .http
                    .get(&url)
                    .send()
                    .await
                    .expect("download mobile item");
                assert!(response.status().is_success());
                assert_eq!(
                    response.bytes().await.expect("read download").as_ref(),
                    bytes
                );
                assert_eq!(
                    runtime.mobile_transfer(id).unwrap().progress.status,
                    TransferStatus::Completed
                );
                assert!(staged.exists());
            }
            let view = runtime
                .mobile_transfers_snapshot(Some(&session.session_id))
                .into_iter()
                .find(|view| view.progress.transfer_id == id)
                .unwrap();
            assert_eq!(view.retryable_download_item_ids, ["item"]);
        }
        runtime
            .stopping
            .store(true, std::sync::atomic::Ordering::SeqCst);
        server_task.abort();
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn checksum_failure_removes_partial_file_and_fails_transfer() {
        let data_dir =
            std::env::temp_dir().join(format!("meshshare-upload-failure-{}", Uuid::new_v4()));
        let static_root = data_dir.join("web");
        let destination = data_dir.join("received");
        std::fs::create_dir_all(&static_root).expect("create static directory");
        std::fs::create_dir_all(&destination).expect("create destination");
        std::fs::write(static_root.join("index.html"), "<html></html>")
            .expect("write static index");
        let runtime = NodeRuntime::new(data_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let mut hasher = Sha256::new();
        hasher.update(b"expected");
        let offer = TransferOffer {
            transfer_id: "checksum-failure".to_string(),
            source_node_id: "peer".to_string(),
            source_name: "peer".to_string(),
            target_node_id: runtime.node_id(),
            items: vec![TransferItem {
                item_id: "item".to_string(),
                name: "payload.bin".to_string(),
                relative_path: "payload.bin".to_string(),
                size: 8,
                sha256: format!("{:x}", hasher.finalize()),
                media_type: "application/octet-stream".to_string(),
                directory: false,
            }],
            total_bytes: 8,
            created_at_ms: crate::clock::now_millis(),
        };
        runtime.add_transfer(offer);
        runtime
            .set_transfer_acceptance("checksum-failure", destination.clone())
            .expect("accept transfer");

        let app = super::router(runtime.clone(), static_root);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let address = listener.local_addr().expect("listener address");
        let server_task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test router");
        });
        let response = runtime
            .http
            .put(format!(
                "http://{address}/api/v1/transfers/checksum-failure/items/item"
            ))
            .body("wrong")
            .send()
            .await
            .expect("upload wrong payload");
        assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
        assert_eq!(
            runtime
                .transfer("checksum-failure")
                .expect("find failed transfer")
                .progress
                .status,
            TransferStatus::Failed
        );
        assert!(
            std::fs::read_dir(&destination)
                .expect("read destination")
                .next()
                .is_none(),
            "checksum failure must remove partial files"
        );
        assert_eq!(
            std::fs::read_to_string(data_dir.join("desktop-transfer-temp-files.json"))
                .expect("read desktop temp registry")
                .trim(),
            "[]"
        );
        runtime
            .stopping
            .store(true, std::sync::atomic::Ordering::SeqCst);
        server_task.abort();
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[test]
    fn cancelled_upload_guard_removes_registered_temp_file() {
        let data_dir =
            std::env::temp_dir().join(format!("meshshare-upload-guard-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&data_dir).expect("create data directory");
        let runtime = NodeRuntime::new(data_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let temp = data_dir.join("received.meshshare-test.part");
        runtime
            .register_desktop_temp_file(&temp)
            .expect("register temp file");
        std::fs::write(&temp, b"partial").expect("write partial file");

        let guard = super::DesktopTempFileGuard::new(runtime.clone(), temp.clone());
        drop(guard);

        assert!(!temp.exists());
        assert_eq!(
            std::fs::read_to_string(data_dir.join("desktop-transfer-temp-files.json"))
                .expect("read temp registry")
                .trim(),
            "[]"
        );
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[test]
    fn cancelled_upload_guard_removes_incomplete_hard_link_commit() {
        let data_dir =
            std::env::temp_dir().join(format!("meshshare-upload-link-guard-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&data_dir).expect("create data directory");
        let runtime = NodeRuntime::new(data_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let temp = data_dir.join("received.meshshare-test.part");
        let target = data_dir.join("received.png");
        runtime
            .register_desktop_temp_file(&temp)
            .expect("register temp file");
        std::fs::write(&temp, b"partial").expect("write partial file");
        std::fs::hard_link(&temp, &target).expect("link partial file");

        let mut guard = super::DesktopTempFileGuard::new(runtime.clone(), temp.clone());
        guard.mark_linked(target.clone());
        drop(guard);

        assert!(!temp.exists());
        assert!(!target.exists());
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn concurrent_android_style_commits_never_replace_target() {
        let data_dir =
            std::env::temp_dir().join(format!("meshshare-android-commit-race-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&data_dir).expect("create data directory");
        let target = data_dir.join("same-name.bin");
        let first_temp = data_dir.join("first.part");
        let second_temp = data_dir.join("second.part");
        std::fs::write(&first_temp, b"first payload").expect("write first temporary file");
        std::fs::write(&second_temp, b"second payload").expect("write second temporary file");

        let first_target = target.clone();
        let first_temp_for_task = first_temp.clone();
        let first = tokio::spawn(async move {
            super::commit_android_temp_file(&first_temp_for_task, &first_target).await
        });
        let second_target = target.clone();
        let second_temp_for_task = second_temp.clone();
        let second = tokio::spawn(async move {
            super::commit_android_temp_file(&second_temp_for_task, &second_target).await
        });
        let (first_result, second_result) = tokio::join!(first, second);
        let first_result = first_result.expect("first commit task");
        let second_result = second_result.expect("second commit task");
        let successes = [first_result.is_ok(), second_result.is_ok()]
            .into_iter()
            .filter(|success| *success)
            .count();
        assert_eq!(successes, 1, "at most one transfer may claim a target");
        assert!(first_result.is_ok() ^ second_result.is_ok());
        let content = std::fs::read(&target).expect("read committed target");
        assert!(content == b"first payload" || content == b"second payload");
        assert!(target.exists());
        assert!(!target.is_symlink());

        std::fs::write(&target, b"existing payload").expect("replace test target");
        let third_temp = data_dir.join("third.part");
        std::fs::write(&third_temp, b"must not replace").expect("write third temporary file");
        let result = super::commit_android_temp_file(&third_temp, &target).await;
        assert_eq!(
            result
                .expect_err("existing destination must reject commit")
                .kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            std::fs::read(&target).expect("read existing target"),
            b"existing payload"
        );

        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn multi_file_upload_preserves_relative_paths_and_completes_once() {
        let data_dir =
            std::env::temp_dir().join(format!("meshshare-multi-upload-{}", Uuid::new_v4()));
        let static_root = data_dir.join("web");
        let destination = data_dir.join("received");
        std::fs::create_dir_all(&static_root).expect("create static directory");
        std::fs::create_dir_all(&destination).expect("create destination");
        std::fs::write(static_root.join("index.html"), "<html></html>")
            .expect("write static index");
        let runtime = NodeRuntime::new(data_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let digest = |bytes: &[u8]| {
            let mut hasher = Sha256::new();
            hasher.update(bytes);
            format!("{:x}", hasher.finalize())
        };
        let first = b"one";
        let second = b"two2";
        let offer = TransferOffer {
            transfer_id: "multi-upload".to_string(),
            source_node_id: "peer".to_string(),
            source_name: "peer".to_string(),
            target_node_id: runtime.node_id(),
            items: vec![
                TransferItem {
                    item_id: "first".to_string(),
                    name: "one.txt".to_string(),
                    relative_path: "folder/one.txt".to_string(),
                    size: first.len() as u64,
                    sha256: digest(first),
                    media_type: "text/plain".to_string(),
                    directory: false,
                },
                TransferItem {
                    item_id: "second".to_string(),
                    name: "two.txt".to_string(),
                    relative_path: "folder/two.txt".to_string(),
                    size: second.len() as u64,
                    sha256: digest(second),
                    media_type: "text/plain".to_string(),
                    directory: false,
                },
            ],
            total_bytes: (first.len() + second.len()) as u64,
            created_at_ms: crate::clock::now_millis(),
        };
        runtime.add_transfer(offer);
        runtime
            .set_transfer_acceptance("multi-upload", destination.clone())
            .expect("accept transfer");

        let app = super::router(runtime.clone(), static_root);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let address = listener.local_addr().expect("listener address");
        let server_task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test router");
        });
        for (item_id, bytes) in [("first", first.as_slice()), ("second", second.as_slice())] {
            let response = runtime
                .http
                .put(format!(
                    "http://{address}/api/v1/transfers/multi-upload/items/{item_id}"
                ))
                .body(bytes.to_vec())
                .send()
                .await
                .expect("upload item");
            assert!(response.status().is_success());
            if item_id == "first" {
                // 接收方已提交条目后，
                // 丢失的响应必须可以安全重试。
                let duplicate = runtime
                    .http
                    .put(format!(
                        "http://{address}/api/v1/transfers/multi-upload/items/{item_id}"
                    ))
                    .body(bytes.to_vec())
                    .send()
                    .await
                    .expect("retry committed item");
                assert!(duplicate.status().is_success());
                let progress = runtime
                    .transfer("multi-upload")
                    .expect("find transfer after retry")
                    .progress;
                assert_eq!(progress.completed_items, 1);
                assert_eq!(progress.completed_bytes, first.len() as u64);
            }
        }
        assert_eq!(
            runtime
                .transfer("multi-upload")
                .expect("find completed transfer")
                .progress
                .status,
            TransferStatus::Completed
        );
        assert_eq!(
            std::fs::read(destination.join("folder/one.txt")).expect("read first file"),
            first
        );
        assert_eq!(
            std::fs::read(destination.join("folder/two.txt")).expect("read second file"),
            second
        );
        runtime
            .stopping
            .store(true, std::sync::atomic::Ordering::SeqCst);
        server_task.abort();
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn prepare_target_rejects_symlinked_directories() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!("meshshare-target-{}", Uuid::new_v4()));
        let outside = std::env::temp_dir().join(format!("meshshare-outside-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("create target root");
        std::fs::create_dir_all(&outside).expect("create outside root");
        symlink(&outside, root.join("escape")).expect("create target symlink");
        assert!(super::prepare_target(&root, "escape/file.txt")
            .await
            .is_err());
        assert!(!outside.join("file.txt").exists());
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(outside);
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
            mobile_addresses: Vec::new(),
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
            ..Default::default()
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
        // 该测试只验证线上协议处理；使用只读配置可以让
        // 剪贴板快照留在本地，
        // 而不把测试数据写进宿主系统剪贴板。
        let runtime = NodeRuntime::new_with_profile(
            data_dir.clone(),
            0,
            env!("CARGO_PKG_VERSION").into(),
            NodeProfile::Android,
        )
        .expect("create runtime");
        runtime.set_peer(PeerInfo {
            node_id: "relay".to_string(),
            node_name: "Relay".to_string(),
            platform: "test".to_string(),
            addresses: Vec::new(),
            mobile_addresses: Vec::new(),
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
        // 剪贴板消息是连接冒烟测试的一部分，
        // 但测试不得改动宿主系统剪贴板。
        let runtime = NodeRuntime::new_with_profile(
            data_dir.clone(),
            0,
            env!("CARGO_PKG_VERSION").into(),
            NodeProfile::Android,
        )
        .expect("create runtime");
        let app = super::router(runtime.clone(), static_root);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let address = listener.local_addr().expect("listener address");
        let server_task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test router");
        });
        let denied_request = Request::builder()
            .uri(format!("ws://{address}/api/v1/ws"))
            .header("Origin", "https://evil.example")
            .body(())
            .expect("build denied websocket request");
        assert!(connect_async(denied_request).await.is_err());
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
            mobile_addresses: Vec::new(),
            capabilities: vec![METRICS_SYSTEM_CAPABILITY.to_string()],
        };
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                serde_json::to_string(&WireMessage::Hello { node: peer }).expect("serialize hello"),
            ))
            .await
            .expect("send hello");
        for index in 0..1_000 {
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
                serde_json::to_string(&WireMessage::ClipboardSnapshot {
                    snapshot: ClipboardSnapshot {
                        event_id: "websocket-snapshot".to_string(),
                        source_node_id: "peer".to_string(),
                        source_name: "Peer".to_string(),
                        clock: Hlc {
                            physical_ms: 31,
                            logical: 0,
                            node_id: "peer".to_string(),
                        },
                        text: Some("snapshot".to_string()),
                        image_png: None,
                        updated_at_ms: 31,
                    },
                })
                .expect("clipboard snapshot"),
            ))
            .await
            .expect("send clipboard snapshot");
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                serde_json::to_string(&WireMessage::MetricsUpdate {
                    metrics: HardwareMetrics {
                        node_id: "peer".to_string(),
                        sampled_at_ms: 31,
                        cpu_usage_percent: Some(12.0),
                        memory_used_bytes: None,
                        memory_total_bytes: None,
                        network_down_bytes_per_sec: None,
                        network_up_bytes_per_sec: None,
                        gpus: Vec::new(),
                        ..Default::default()
                    },
                })
                .expect("metrics update"),
            ))
            .await
            .expect("send metrics update");
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

    #[tokio::test]
    async fn websocket_keeps_ui_client_alive_with_application_ping() {
        let data_dir =
            std::env::temp_dir().join(format!("meshshare-ui-heartbeat-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&data_dir).expect("create test directory");
        let static_root = data_dir.join("web");
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
        let _ = socket.next().await.expect("server hello");
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                serde_json::to_string(&WireMessage::Ping { at_ms: 1 }).expect("ping"),
            ))
            .await
            .expect("send ui ping");
        let pong = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let message = socket
                    .next()
                    .await
                    .expect("pong response")
                    .expect("valid message");
                if let tokio_tungstenite::tungstenite::Message::Text(body) = message {
                    if matches!(
                        serde_json::from_str::<WireMessage>(&body).expect("decode pong"),
                        WireMessage::Pong { .. }
                    ) {
                        break;
                    }
                }
            }
        })
        .await;
        assert!(
            pong.is_ok(),
            "UI heartbeat should receive an application pong"
        );
        tokio::time::sleep(PEER_HANDSHAKE_TIMEOUT + Duration::from_millis(100)).await;
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                serde_json::to_string(&WireMessage::Ping { at_ms: 2 }).expect("second ping"),
            ))
            .await
            .expect("send second ui ping");
        let pong = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let message = socket
                    .next()
                    .await
                    .expect("second pong response")
                    .expect("valid message");
                if let tokio_tungstenite::tungstenite::Message::Text(body) = message {
                    if matches!(
                        serde_json::from_str::<WireMessage>(&body).expect("decode second pong"),
                        WireMessage::Pong { .. }
                    ) {
                        break;
                    }
                }
            }
        })
        .await;
        assert!(
            pong.is_ok(),
            "UI socket should remain alive after handshake deadline"
        );

        runtime
            .stopping
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let _ = socket.close(None).await;
        server_task.abort();
        let _ = server_task.await;
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn websocket_closes_if_peer_never_sends_hello() {
        let data_dir =
            std::env::temp_dir().join(format!("meshshare-handshake-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&data_dir).expect("create test directory");
        let static_root = data_dir.join("web");
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
        assert!(socket.next().await.is_some(), "server hello");

        tokio::time::sleep(PEER_HANDSHAKE_TIMEOUT + Duration::from_secs(1)).await;
        let closed = tokio::time::timeout(Duration::from_secs(1), socket.next())
            .await
            .expect("handshake timeout should close socket");
        assert!(matches!(closed, None | Some(Err(_))));

        runtime
            .stopping
            .store(true, std::sync::atomic::Ordering::SeqCst);
        server_task.abort();
        let _ = server_task.await;
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn transfer_history_routes_list_and_clear_without_touching_active() {
        let data_dir =
            std::env::temp_dir().join(format!("meshshare-transfer-history-{}", Uuid::new_v4()));
        let static_root = data_dir.join("web");
        std::fs::create_dir_all(&static_root).expect("create static directory");
        std::fs::write(static_root.join("index.html"), "<html></html>")
            .expect("write static index");
        let runtime = NodeRuntime::new(data_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        runtime.add_transfer(test_offer(&runtime, "history-done"));
        runtime
            .mark_item_received("history-done", "history-done-item", 0, "0".repeat(64))
            .expect("complete transfer");
        runtime.add_outgoing_transfer(test_offer(&runtime, "active-one"));
        let app = super::router(runtime.clone(), static_root);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let address = listener.local_addr().expect("listener address");
        let server_task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test router");
        });

        let history = runtime
            .http
            .get(format!("http://{address}/api/v1/transfers/history"))
            .send()
            .await
            .expect("list history")
            .error_for_status()
            .expect("history response")
            .json::<Vec<TransferHistoryEntry>>()
            .await
            .expect("decode history");
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].transfer_id, "history-done");

        let cleared = runtime
            .http
            .delete(format!("http://{address}/api/v1/transfers/history"))
            .send()
            .await
            .expect("clear history");
        assert!(cleared.status().is_success());
        let body = cleared
            .json::<serde_json::Value>()
            .await
            .expect("decode cleared");
        assert_eq!(body["cleared"], 1);

        let after = runtime
            .http
            .get(format!("http://{address}/api/v1/transfers/history"))
            .send()
            .await
            .expect("list history after clear")
            .error_for_status()
            .expect("history response")
            .json::<Vec<TransferHistoryEntry>>()
            .await
            .expect("decode history");
        assert!(after.is_empty());
        let transfers = runtime
            .http
            .get(format!("http://{address}/api/v1/transfers"))
            .send()
            .await
            .expect("list transfers")
            .error_for_status()
            .expect("transfers response")
            .json::<Vec<TransferProgress>>()
            .await
            .expect("decode transfers");
        assert!(transfers
            .iter()
            .any(|item| item.transfer_id == "active-one"));

        runtime
            .stopping
            .store(true, std::sync::atomic::Ordering::SeqCst);
        server_task.abort();
        let _ = server_task.await;
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn transfer_history_deleted_message_requires_identified_peer() {
        let data_dir =
            std::env::temp_dir().join(format!("meshshare-history-message-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&data_dir).expect("create data directory");
        let runtime = NodeRuntime::new(data_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let mut offer = test_offer(&runtime, "peer-history");
        offer.source_node_id = "peer-a".to_string();
        offer.source_name = "A".to_string();
        runtime.add_transfer(offer);
        runtime
            .mark_item_received("peer-history", "peer-history-item", 0, "0".repeat(64))
            .expect("complete transfer");
        assert_eq!(runtime.transfer_history_snapshot(None).len(), 1);

        assert!(handle_wire_message(
            WireMessage::TransferHistoryDeleted {
                transfer_ids: vec!["peer-history".to_string()],
            },
            &runtime,
            None,
        )
        .await
        .is_none());
        assert_eq!(runtime.transfer_history_snapshot(None).len(), 1);

        assert!(handle_wire_message(
            WireMessage::TransferHistoryDeleted {
                transfer_ids: vec!["peer-history".to_string()],
            },
            &runtime,
            Some("peer-a"),
        )
        .await
        .is_some());
        assert!(runtime.transfer_history_snapshot(None).is_empty());
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn stream_body_to_file_reports_each_chunk_length() {
        let data_dir =
            std::env::temp_dir().join(format!("meshshare-stream-chunks-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&data_dir).expect("create data directory");
        let target = data_dir.join("body.bin");
        let file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .await
            .expect("open target");
        let body = axum::body::Body::from_stream(futures_util::stream::iter(vec![
            Ok::<_, std::io::Error>(b"abc".to_vec()),
            Ok::<_, std::io::Error>(b"defgh".to_vec()),
        ]));
        let mut lengths = Vec::new();
        let (total, digest) = super::stream_body_to_file(body, file, 8, &data_dir, |length| {
            lengths.push(length);
            Ok(())
        })
        .await
        .expect("stream body");
        assert_eq!(lengths, vec![3, 5]);
        assert_eq!(total, 8);
        assert_eq!(digest, format!("{:x}", Sha256::digest(b"abcdefgh")));
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn upload_item_reports_in_flight_progress_before_commit() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let data_dir =
            std::env::temp_dir().join(format!("meshshare-live-upload-{}", Uuid::new_v4()));
        let static_root = data_dir.join("web");
        let destination = data_dir.join("received");
        std::fs::create_dir_all(&static_root).expect("create static directory");
        std::fs::create_dir_all(&destination).expect("create destination");
        std::fs::write(static_root.join("index.html"), "<html></html>")
            .expect("write static index");
        let runtime = NodeRuntime::new(data_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let content = b"abcdefgh";
        let offer = TransferOffer {
            transfer_id: "live-upload".to_string(),
            source_node_id: "peer".to_string(),
            source_name: "peer".to_string(),
            target_node_id: runtime.node_id(),
            items: vec![TransferItem {
                item_id: "item".to_string(),
                name: "file.bin".to_string(),
                relative_path: "file.bin".to_string(),
                size: content.len() as u64,
                sha256: format!("{:x}", Sha256::digest(content)),
                media_type: "application/octet-stream".to_string(),
                directory: false,
            }],
            total_bytes: content.len() as u64,
            created_at_ms: crate::clock::now_millis(),
        };
        runtime.add_transfer(offer);
        runtime
            .set_transfer_acceptance("live-upload", destination)
            .expect("accept transfer");

        let app = super::router(runtime.clone(), static_root);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let address = listener.local_addr().expect("listener address");
        let server_task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test router");
        });

        let mut socket = tokio::net::TcpStream::connect(address)
            .await
            .expect("connect upload");
        let headers = format!(
            "PUT /api/v1/transfers/live-upload/items/item HTTP/1.1\r\nHost: {address}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
        );
        socket
            .write_all(headers.as_bytes())
            .await
            .expect("write headers");
        socket
            .write_all(b"4\r\nabcd\r\n")
            .await
            .expect("write first chunk");
        socket.flush().await.expect("flush first chunk");

        let mut mid = None;
        for _ in 0..40 {
            let progress = runtime
                .transfer("live-upload")
                .expect("find transfer")
                .progress;
            if progress.completed_bytes == 4 {
                mid = Some(progress);
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let mid = mid.expect("first chunk should report in-flight progress");
        assert_eq!(mid.completed_bytes, 4);
        assert_eq!(mid.completed_items, 0);
        assert_eq!(mid.status, TransferStatus::Uploading);

        socket
            .write_all(b"4\r\nefgh\r\n0\r\n\r\n")
            .await
            .expect("write second chunk");
        socket.flush().await.expect("flush second chunk");
        let mut response = Vec::new();
        socket
            .read_to_end(&mut response)
            .await
            .expect("read upload response");
        let response = String::from_utf8_lossy(&response);
        assert!(
            response.starts_with("HTTP/1.1 200"),
            "unexpected response: {response}"
        );
        let completed = runtime
            .transfer("live-upload")
            .expect("find completed transfer")
            .progress;
        assert_eq!(completed.completed_bytes, 8);
        assert_eq!(completed.completed_items, 1);
        assert_eq!(completed.status, TransferStatus::Completed);
        server_task.abort();
        let _ = std::fs::remove_dir_all(data_dir);
    }

    fn test_offer(runtime: &NodeRuntime, transfer_id: &str) -> TransferOffer {
        TransferOffer {
            transfer_id: transfer_id.to_string(),
            source_node_id: runtime.node_id(),
            source_name: "desktop".to_string(),
            target_node_id: runtime.node_id(),
            items: vec![TransferItem {
                item_id: format!("{transfer_id}-item"),
                name: "file.txt".to_string(),
                relative_path: "file.txt".to_string(),
                size: 0,
                sha256: "0".repeat(64),
                media_type: "text/plain".to_string(),
                directory: false,
            }],
            total_bytes: 0,
            created_at_ms: crate::clock::now_millis(),
        }
    }

    #[tokio::test]
    async fn failed_transfer_can_be_removed_and_is_not_listed() {
        let data_dir =
            std::env::temp_dir().join(format!("meshshare-dismiss-transfer-{}", Uuid::new_v4()));
        let static_root = data_dir.join("web");
        std::fs::create_dir_all(&static_root).expect("create static directory");
        std::fs::write(static_root.join("index.html"), "<html></html>")
            .expect("write static index");
        let runtime = NodeRuntime::new(data_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        runtime.add_outgoing_transfer(test_offer(&runtime, "dismiss-failed"));
        runtime
            .fail_transfer(
                "dismiss-failed",
                "failed".to_string(),
                TransferStatus::Failed,
            )
            .expect("fail transfer");
        let app = super::router(runtime.clone(), static_root);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let address = listener.local_addr().expect("listener address");
        let server_task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test router");
        });

        let removed = runtime
            .http
            .delete(format!("http://{address}/api/v1/transfers/dismiss-failed"))
            .send()
            .await
            .expect("remove failed transfer");
        assert!(removed.status().is_success());
        let listed = runtime
            .http
            .get(format!("http://{address}/api/v1/transfers"))
            .send()
            .await
            .expect("list transfers")
            .error_for_status()
            .expect("list response")
            .json::<Vec<TransferProgress>>()
            .await
            .expect("decode transfers");
        assert!(listed
            .iter()
            .all(|progress| progress.transfer_id != "dismiss-failed"));
        assert!(runtime.transfer("dismiss-failed").is_none());
        let again = runtime
            .http
            .delete(format!("http://{address}/api/v1/transfers/dismiss-failed"))
            .send()
            .await
            .expect("remove again");
        assert_eq!(again.status().as_u16(), 404);

        runtime
            .stopping
            .store(true, std::sync::atomic::Ordering::SeqCst);
        server_task.abort();
        let _ = server_task.await;
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn active_transfer_removal_is_rejected() {
        let data_dir =
            std::env::temp_dir().join(format!("meshshare-dismiss-active-{}", Uuid::new_v4()));
        let static_root = data_dir.join("web");
        std::fs::create_dir_all(&static_root).expect("create static directory");
        std::fs::write(static_root.join("index.html"), "<html></html>")
            .expect("write static index");
        let runtime = NodeRuntime::new(data_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        runtime.add_outgoing_transfer(test_offer(&runtime, "dismiss-active"));
        let app = super::router(runtime.clone(), static_root);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let address = listener.local_addr().expect("listener address");
        let server_task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test router");
        });

        let denied = runtime
            .http
            .delete(format!("http://{address}/api/v1/transfers/dismiss-active"))
            .send()
            .await
            .expect("remove active transfer");
        assert_eq!(denied.status().as_u16(), 409);
        assert!(runtime.transfer("dismiss-active").is_some());

        runtime
            .stopping
            .store(true, std::sync::atomic::Ordering::SeqCst);
        server_task.abort();
        let _ = server_task.await;
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[tokio::test]
    async fn failed_mobile_transfer_removal_cleans_staged_directory() {
        let data_dir =
            std::env::temp_dir().join(format!("meshshare-dismiss-mobile-{}", Uuid::new_v4()));
        let static_root = data_dir.join("web");
        std::fs::create_dir_all(&static_root).expect("create static directory");
        std::fs::write(static_root.join("index.html"), "<html></html>")
            .expect("write static index");
        let runtime = NodeRuntime::new(data_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        let session = runtime
            .create_mobile_session("test phone".to_string())
            .expect("create mobile session");
        runtime
            .create_mobile_transfer(&session.session_id, test_offer(&runtime, "mobile-dismiss"))
            .expect("create mobile transfer");
        let record = runtime
            .mobile_transfer("mobile-dismiss")
            .expect("find mobile transfer");
        std::fs::create_dir_all(&record.staged_directory).expect("create staging directory");
        std::fs::write(record.staged_directory.join("stale.part"), b"stale")
            .expect("write staged file");
        runtime
            .fail_mobile_transfer("mobile-dismiss", "failed".to_string())
            .expect("fail mobile transfer");
        let app = super::router(runtime.clone(), static_root);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test listener");
        let address = listener.local_addr().expect("listener address");
        let server_task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve test router");
        });

        let removed = runtime
            .http
            .delete(format!(
                "http://{address}/api/v1/mobile/transfers/mobile-dismiss"
            ))
            .send()
            .await
            .expect("remove failed mobile transfer");
        assert!(removed.status().is_success());
        assert!(runtime.mobile_transfer("mobile-dismiss").is_none());
        assert!(!record.staged_directory.exists());
        let again = runtime
            .http
            .delete(format!(
                "http://{address}/api/v1/mobile/transfers/mobile-dismiss"
            ))
            .send()
            .await
            .expect("remove again");
        assert_eq!(again.status().as_u16(), 404);

        runtime
            .stopping
            .store(true, std::sync::atomic::Ordering::SeqCst);
        server_task.abort();
        let _ = server_task.await;
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
    fn misdirected(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::MISDIRECTED_REQUEST,
            message: message.into(),
        }
    }
    fn too_early(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::TOO_EARLY,
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
    fn request_timeout(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::REQUEST_TIMEOUT,
            message: message.into(),
        }
    }
    fn too_many_requests(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
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

impl From<TransferRetryError> for ApiError {
    fn from(error: TransferRetryError) -> Self {
        match error {
            TransferRetryError::InstallingUpdate => Self::too_early(UPDATE_INSTALLING_MESSAGE),
            TransferRetryError::NotRetryable => Self::conflict("传输不是可重试的失败状态"),
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
