mod socket;
use crate::clipboard;
use crate::control::{accept_transfer_at, ControlPlane, SettingsPatch, SettingsSnapshot};
use crate::discovery;
use crate::model::{
    ClipboardEvent, ClipboardHistoryDetail, ClipboardHistoryList, ClipboardSnapshot,
    HardwareMetrics, MobileSession, MobileTransferView, NodeInfo, PeerInfo, TransferAction,
    TransferHistoryEntry, TransferOffer, TransferProgress, TransferStatus, WireMessage,
    CLIPBOARD_TEXT_LIMIT, MAX_WIRE_ERROR_LENGTH,
};
use crate::runtime::{
    validate_receive_directory, NodeRuntime, OutgoingPrepareView, TransferRetryError,
    TRANSFER_PROGRESS_INTERVAL, UPDATE_INSTALLING_MESSAGE,
};
use axum::body::Body;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path as AxumPath, Query, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::{from_fn, from_fn_with_state, Next};
use axum::response::Response as AxumResponse;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use futures_util::StreamExt;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(not(target_os = "android"))]
pub(crate) use socket::validate_node_info;
pub(crate) use socket::{handle_wire_message, peer_from_hello, sync_peer_state};
use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex as AsyncMutex;
use tokio_util::io::ReaderStream;
use tower_http::cors::{AllowHeaders, AllowMethods, AllowOrigin, CorsLayer};
use tower_http::services::{ServeDir, ServeFile};

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
            .map_err(|_| ApiError::new(StatusCode::REQUEST_TIMEOUT, "上传长时间没有数据"))?;
        let Some(chunk) = chunk else {
            break;
        };
        let chunk = chunk.map_err(|error| ApiError::internal(error.to_string()))?;
        on_chunk(chunk.len() as u64)?;
        total = total
            .checked_add(chunk.len() as u64)
            .ok_or_else(|| ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, "传输内容大小溢出"))?;
        if total > limit {
            return Err(ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "传输内容超过清单大小",
            ));
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
    let state = AppState { runtime, control };
    let app = Router::new();
    #[cfg(not(target_os = "android"))]
    let app = if state.runtime.is_android_profile() {
        app
    } else {
        app.route("/api/v1/terminal/ssh", get(terminal_upgrade))
    };
    #[cfg(all(feature = "headless", target_os = "linux"))]
    let app = if state.runtime.supports_capability("node.headless.v1") {
        app.route("/api/v1/management/service/restart", post(restart_headless))
    } else {
        app
    };
    app.route("/api/v1/info", get(info))
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
        .route("/api/v1/ws", get(socket::websocket))
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
        .route(
            "/api/v1/management/mobile/outgoing/paths",
            post(create_managed_mobile_offer),
        )
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
        .fallback_service(ServeDir::new(static_root).fallback(ServeFile::new(index)))
        .layer(DefaultBodyLimit::max(32 * 1024 * 1024))
        .layer(from_fn_with_state(state.runtime.clone(), target_guard))
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

async fn target_guard(
    State(runtime): State<Arc<NodeRuntime>>,
    request: axum::extract::Request,
    next: Next,
) -> AxumResponse {
    if request
        .headers()
        .get("x-meshshare-target")
        .is_some_and(|id| id.as_bytes() != runtime.node_id().as_bytes())
    {
        return ApiError::new(StatusCode::MISDIRECTED_REQUEST, "地址上的设备与目标不匹配")
            .into_response();
    }
    next.run(request).await
}

#[cfg(not(target_os = "android"))]
async fn terminal_upgrade(
    State(state): State<AppState>,
    request: axum::extract::Request,
) -> AxumResponse {
    state
        .runtime
        .ssh
        .clone()
        .upgrade(request, state.runtime)
        .await
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
    Json(state.control.snapshot().await)
}

#[cfg(all(feature = "headless", target_os = "linux"))]
async fn restart_headless(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
) -> Result<StatusCode, ApiError> {
    if headers
        .get("x-meshshare-target")
        .and_then(|id| id.to_str().ok())
        != Some(&state.runtime.node_id())
    {
        return Err(ApiError::new(
            StatusCode::MISDIRECTED_REQUEST,
            "重启请求的目标节点不匹配",
        ));
    }
    let service = state
        .control
        .headless()
        .ok_or_else(|| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "服务控制尚未就绪"))?;
    service.restart().await.map_err(|error| match error {
        crate::headless_service::RestartError::Busy => {
            ApiError::conflict("文件准备、传输、终端或维护正在进行，暂时无法重启")
        }
        crate::headless_service::RestartError::Unavailable(reason) => {
            ApiError::new(StatusCode::SERVICE_UNAVAILABLE, reason)
        }
    })?;
    Ok(StatusCode::ACCEPTED)
}

async fn patch_settings(
    State(state): State<AppState>,
    Json(patch): Json<SettingsPatch>,
) -> Result<Json<SettingsSnapshot>, ApiError> {
    state
        .control
        .apply_patch(patch)
        .await
        .map(Json)
        .map_err(ApiError::bad_request)
}

async fn request_update_check(
    State(state): State<AppState>,
) -> Result<Json<SettingsSnapshot>, ApiError> {
    state
        .control
        .request_update_check()
        .await
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
            StatusCode::MISDIRECTED_REQUEST => {
                ApiError::new(StatusCode::MISDIRECTED_REQUEST, message)
            }
            StatusCode::TOO_EARLY => ApiError::new(StatusCode::TOO_EARLY, message),
            StatusCode::REQUEST_TIMEOUT => ApiError::new(StatusCode::REQUEST_TIMEOUT, message),
            StatusCode::TOO_MANY_REQUESTS => ApiError::new(StatusCode::TOO_MANY_REQUESTS, message),
            StatusCode::PAYLOAD_TOO_LARGE => ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, message),
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
        return Err(ApiError::new(
            StatusCode::MISDIRECTED_REQUEST,
            "地址上的设备与传输目标不匹配",
        ));
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
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "图片超过 16 MiB 限制",
        ));
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
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "文字超过 16 MiB 限制",
        ));
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

async fn create_transfer(
    State(state): State<AppState>,
    Json(mut offer): Json<TransferOffer>,
) -> Result<Json<TransferProgress>, ApiError> {
    validate_offer(&offer, false)?;
    if offer.target_node_id != state.runtime.node_id() {
        return Err(ApiError::new(
            StatusCode::MISDIRECTED_REQUEST,
            "地址上的设备与传输目标不匹配",
        ));
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
    Ok(Json(TransferHistoryClearedBody { cleared }))
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

pub(crate) fn validate_mobile_offer(offer: &TransferOffer) -> Result<(), String> {
    validate_offer(offer, false).map_err(|error| error.message)
}

#[derive(Deserialize)]
struct ManagedMobileRequest {
    session_id: String,
    paths: Vec<String>,
}

async fn create_managed_mobile_offer(
    State(state): State<AppState>,
    Json(request): Json<ManagedMobileRequest>,
) -> Result<Json<MobileTransferView>, ApiError> {
    if !state
        .runtime
        .supports_capability(crate::model::MANAGED_MOBILE_PATHS_CAPABILITY)
    {
        return Err(ApiError::not_found(
            "当前节点不支持从本机路径发送到网页会话",
        ));
    }
    ensure_remote_file_access(&state)?;
    state
        .runtime
        .touch_mobile_session(&request.session_id)
        .ok_or_else(|| ApiError::not_found("手机会话已过期"))?;
    tokio::task::spawn_blocking(move || {
        state
            .runtime
            .offer_mobile_paths(&request.session_id, &request.paths)
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()))?
    .map(Json)
    .map_err(|e| {
        if e == UPDATE_INSTALLING_MESSAGE {
            ApiError::new(StatusCode::TOO_EARLY, e)
        } else {
            ApiError::bad_request(e)
        }
    })
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
    mobile_session_action(&state, &id, &payload.session_id, TransferStatus::Accepted).await
}

async fn reject_mobile_transfer(
    AxumPath(id): AxumPath<String>,
    State(state): State<AppState>,
    Json(payload): Json<MobileSessionRequest>,
) -> Result<Json<MobileTransferView>, ApiError> {
    mobile_session_action(&state, &id, &payload.session_id, TransferStatus::Rejected).await
}

async fn cancel_mobile_transfer(
    AxumPath(id): AxumPath<String>,
    State(state): State<AppState>,
    Json(payload): Json<MobileSessionRequest>,
) -> Result<Json<MobileTransferView>, ApiError> {
    mobile_session_action(&state, &id, &payload.session_id, TransferStatus::Cancelled).await
}

async fn mobile_session_action(
    state: &AppState,
    transfer_id: &str,
    session_id: &str,
    status: TransferStatus,
) -> Result<Json<MobileTransferView>, ApiError> {
    state
        .runtime
        .touch_mobile_session(session_id)
        .ok_or_else(|| ApiError::not_found("手机会话已过期"))?;
    let (view, cleanup) = state
        .runtime
        .set_mobile_transfer_status(transfer_id, session_id, status)
        .ok_or_else(|| ApiError::not_found("手机传输不存在"))?;
    if let Some(directory) = cleanup {
        cleanup_mobile_staging_directory(&directory).await;
    }
    if let Some(sources) = state.runtime.take_managed_mobile_sources(transfer_id) {
        let state = state.clone();
        let transfer_id = transfer_id.to_string();
        tokio::spawn(async move {
            for source in sources.iter() {
                let result = stage_managed_mobile_path(&state, &transfer_id, source).await;
                if let Err(error) = result {
                    if let Some(record) = state.runtime.mobile_transfer(&transfer_id) {
                        fail_mobile_upload(
                            &state.runtime,
                            &transfer_id,
                            &record.staged_directory,
                            error.message,
                        )
                        .await;
                    }
                    break;
                }
            }
        });
    }
    Ok(Json(view))
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
    if record.offer().source_node_id != state.runtime.node_id() {
        return Err(ApiError::forbidden("手机传输来源不匹配"));
    }
    let message = payload
        .error
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "手机文件上传失败".to_string());
    if message.len() > MAX_WIRE_ERROR_LENGTH {
        return Err(ApiError::bad_request("失败原因过长"));
    }
    fail_mobile_upload(&state.runtime, &id, &record.staged_directory, message)
        .await
        .map(Json)
        .ok_or_else(|| ApiError::not_found("手机传输不存在"))
}

async fn fail_mobile_upload(
    runtime: &NodeRuntime,
    id: &str,
    directory: &Path,
    message: String,
) -> Option<MobileTransferView> {
    let view = runtime.fail_mobile_transfer(id, message)?;
    if view.progress.status == TransferStatus::Failed {
        cleanup_mobile_staging_directory(directory).await;
    }
    Some(view)
}

async fn upload_mobile_item(
    AxumPath((transfer_id, item_id)): AxumPath<(String, String)>,
    State(state): State<AppState>,
    body: Body,
) -> Result<Json<MobileTransferView>, ApiError> {
    stage_mobile_body(&state, &transfer_id, &item_id, body, None).await?;
    state
        .runtime
        .mobile_transfer_view(&transfer_id)
        .map(Json)
        .ok_or_else(|| ApiError::not_found("手机传输不存在"))
}

async fn stage_managed_mobile_path(
    state: &AppState,
    id: &str,
    source: &crate::runtime::ManagedMobilePath,
) -> Result<(), ApiError> {
    ensure_remote_file_access(state)?;
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let file = options
        .open(&source.path)
        .await
        .map_err(|e| ApiError::bad_request(format!("打开源文件失败：{e}")))?;
    source
        .verify_file(&file)
        .await
        .map_err(ApiError::bad_request)?;
    let reader = tokio_util::io::ReaderStream::new(
        file.try_clone()
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?,
    );
    stage_mobile_body(
        state,
        id,
        &source.item_id,
        Body::from_stream(reader),
        Some((source, &file)),
    )
    .await
}

async fn stage_mobile_body(
    state: &AppState,
    transfer_id: &str,
    item_id: &str,
    body: Body,
    origin: Option<(&crate::runtime::ManagedMobilePath, &fs::File)>,
) -> Result<(), ApiError> {
    let _item_lock;
    let (session_id, staged_directory, index, item, staged) = {
        let transfers = state.runtime.mobile_transfers.read();
        let record = transfers
            .get(transfer_id)
            .ok_or_else(|| ApiError::not_found("手机传输不存在"))?;
        if record.managed_sources.is_some() != origin.is_some() {
            return Err(ApiError::forbidden("这项传输由目标节点读取源文件"));
        }
        let offer = record.offer();
        if offer.source_node_id != state.runtime.node_id()
            || !matches!(
                record.progress.status,
                TransferStatus::Accepted | TransferStatus::Uploading
            )
        {
            return Err(ApiError::conflict("手机传输尚未被接受"));
        }
        let index = match origin {
            Some((source, _)) => Some(source.item_index),
            None => offer.items.iter().position(|item| item.item_id == item_id),
        }
        .ok_or_else(|| ApiError::not_found("传输项目不存在"))?;
        let item = offer
            .items
            .get(index)
            .filter(|item| item.item_id == item_id)
            .ok_or_else(|| ApiError::not_found("传输项目不存在"))?;
        _item_lock = TransferItemLock::new(&state.runtime, transfer_id, item_id)?;
        (
            record.session_id.clone(),
            record.staged_directory.clone(),
            index,
            item.clone(),
            record.staged_items.contains(item_id),
        )
    };
    if state.runtime.touch_mobile_session(&session_id).is_none() {
        return Err(ApiError::not_found("手机会话已过期"));
    }
    if item.directory || has_parent_component(&item.relative_path) || item.relative_path.is_empty()
    {
        return Err(ApiError::bad_request("传输路径无效"));
    }
    if staged {
        return Ok(());
    }
    ensure_directory_without_symlink(&staged_directory).await?;
    let target = prepare_target(&staged_directory, &item.relative_path).await?;
    state
        .runtime
        .begin_mobile_upload(transfer_id)
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
            fail_mobile_upload(
                &state.runtime,
                transfer_id,
                &staged_directory,
                message.clone(),
            )
            .await;
            return Err(ApiError::internal(message));
        }
    };
    let stream_result =
        stream_body_to_file(body, file, item.size, &staged_directory, |_chunk_bytes| {
            if state.runtime.touch_mobile_session(&session_id).is_none() {
                return Err(ApiError::not_found("手机会话已过期"));
            }
            if !state.runtime.touch_mobile_transfer_activity(transfer_id)
                || (origin.is_some() && !state.runtime.remote_file_access())
            {
                return Err(ApiError::conflict("手机上传已取消或授权已关闭"));
            }
            Ok(())
        })
        .await;
    let (total, digest) = match stream_result {
        Ok(value) => value,
        Err(error) => {
            remove_file_with_diagnostics(&temp).await;
            return Err(error);
        }
    };
    if let Some((source, file)) = origin {
        if let Err(error) = source.verify_file(file).await {
            remove_file_with_diagnostics(&temp).await;
            return Err(ApiError::bad_request(error));
        }
    }
    if total != item.size || !digest_matches(&item.sha256, &digest) {
        remove_file_with_diagnostics(&temp).await;
        fail_mobile_upload(
            &state.runtime,
            transfer_id,
            &staged_directory,
            "文件校验失败".to_string(),
        )
        .await;
        return Err(ApiError::bad_request("文件校验失败"));
    }
    let target = match prepare_target(&staged_directory, &item.relative_path).await {
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
    if state
        .runtime
        .stage_mobile_item(transfer_id, index, item_id, total, digest)
        .is_none()
    {
        remove_file_with_diagnostics(&target).await;
        return Err(ApiError::conflict("手机传输已取消或完成"));
    }
    Ok(())
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
        .offer()
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
        .offer()
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
    finish_transfer(&state, id, TransferAction::Reject, "接收端拒绝了传输").await
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
    finish_transfer(&state, id, TransferAction::Cancel, "传输已取消").await
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
    finish_transfer(&state, id, TransferAction::Fail, &message).await
}

/// 结束本机发起的传输并把结果通知对端；拒绝、取消与上传失败共用同一收尾路径。
async fn finish_transfer(
    state: &AppState,
    transfer_id: String,
    action: TransferAction,
    reason: &str,
) -> Result<Json<TransferProgress>, ApiError> {
    let progress = state
        .runtime
        .fail_transfer(&transfer_id, reason.to_string(), action.status())
        .ok_or_else(|| ApiError::not_found("传输不存在"))?;
    if let Some(source_node_id) = state.runtime.transfer_source_id(&transfer_id) {
        let _ = state
            .runtime
            .send_to_peer_with_retry(
                &source_node_id,
                WireMessage::TransferAction {
                    transfer_id,
                    action,
                    reason: progress.error.clone(),
                    retry_count: progress.retry_count,
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
    let progress = state.runtime.retry_transfer(&id, status)?;
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
    let _item_lock = TransferItemLock::new(&state.runtime, &transfer_id, &item_id)?;
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
    if already_received {
        let target = resolve_target(&destination, &item.relative_path).await?;
        if !existing_file_matches(&target, item.size, &item.sha256).await? {
            return Err(ApiError::conflict("已完成的传输项目与目标文件校验不一致"));
        }
        let progress = state
            .runtime
            .confirm_received_item(&transfer_id, &item_id, record.progress.retry_count)
            .ok_or_else(|| ApiError::conflict("传输状态已变化"))?;
        state.runtime.publish_event(WireMessage::TransferProgress {
            progress: progress.clone(),
        });
        return Ok(Json(progress));
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
            return Err(ApiError::new(StatusCode::TOO_EARLY, "文件项目正在上传"));
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
            .ok_or_else(|| ApiError::new(StatusCode::PAYLOAD_TOO_LARGE, "文件大小溢出"))?;
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
mod tests;

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }
    fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }
    fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, message)
    }
    fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }
    fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, message)
    }
}

impl From<TransferRetryError> for ApiError {
    fn from(error: TransferRetryError) -> Self {
        match error {
            TransferRetryError::InstallingUpdate => {
                Self::new(StatusCode::TOO_EARLY, UPDATE_INSTALLING_MESSAGE)
            }
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
