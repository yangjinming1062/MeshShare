use super::socket::{incoming_peer_from_hello, validate_metrics, PEER_HANDSHAKE_TIMEOUT};
use super::{
    content_disposition_filename, encode_path_segment, handle_wire_message, has_parent_component,
    is_allowed_origin, validate_offer, MobileSessionRequest,
};
use crate::model::{
    ClipboardEvent, ClipboardSnapshot, DiskMetrics, HardwareMetrics, Hlc, NodeInfo, PeerInfo,
    ReceiveMode, TransferHistoryEntry, TransferItem, TransferOffer, TransferProgress,
    TransferStatus, WireMessage, CLIPBOARD_TEXT_LIMIT, METRICS_SYSTEM_CAPABILITY, PROTOCOL_VERSION,
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

#[cfg(not(target_os = "android"))]
#[tokio::test]
async fn terminal_upgrade_requires_a_native_request_from_a_known_mesh_socket() {
    let dir = std::env::temp_dir().join(format!("meshshare-terminal-origin-{}", Uuid::new_v4()));
    let runtime = NodeRuntime::new(dir.clone(), 0, "test".into()).unwrap();
    let state = super::AppState {
        control: super::ControlPlane::runtime_only(runtime.clone()),
        runtime,
    };
    let request = || {
        Request::builder()
            .header("Connection", "upgrade")
            .header("Upgrade", "meshshare-ssh-v3")
            .header("X-MeshShare-Terminal-Target", state.runtime.node_id())
    };
    for origin in [
        "http://localhost",
        "tauri://localhost",
        "https://192.168.1.42",
        "null",
    ] {
        let req = request()
            .extension(super::ConnectInfo(
                "192.168.217.2:45123"
                    .parse::<std::net::SocketAddr>()
                    .unwrap(),
            ))
            .header("Origin", origin)
            .body(axum::body::Body::empty())
            .unwrap();
        assert_eq!(
            super::terminal_upgrade(super::State(state.clone()), req)
                .await
                .status(),
            super::StatusCode::FORBIDDEN
        );
    }
    let local = if_addrs::get_if_addrs()
        .unwrap()
        .into_iter()
        .map(|iface| Some(std::net::SocketAddr::new(iface.ip(), 45123)));
    let forbidden = [
        None,
        Some("127.0.0.1:45123"),
        Some("127.0.0.2:45123"),
        Some("203.0.113.5:45123"),
        Some("[2001:db8::5]:45123"),
    ];
    for source in forbidden
        .map(|addr| addr.map(|addr| addr.parse().unwrap()))
        .into_iter()
        .chain(local)
    {
        let mut req = request().body(axum::body::Body::empty()).unwrap();
        if let Some(source) = source {
            req.extensions_mut().insert(super::ConnectInfo(source));
        }
        assert_eq!(
            super::terminal_upgrade(super::State(state.clone()), req)
                .await
                .status(),
            super::StatusCode::FORBIDDEN
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[cfg(not(target_os = "android"))]
#[tokio::test]
async fn loopback_listener_rejects_terminal_upgrade_before_ssh() {
    let dir = std::env::temp_dir().join(format!("meshshare-local-ssh-{}", Uuid::new_v4()));
    let node = NodeRuntime::new(dir.clone(), 0, "test".into()).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = super::router(node.clone(), dir.join("www"));
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let response = node
        .http
        .get(format!("http://{addr}/api/v1/terminal/ssh"))
        .header("Connection", "upgrade")
        .header("Upgrade", "meshshare-ssh-v3")
        .header("X-MeshShare-Terminal-Target", node.node_id())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), super::StatusCode::FORBIDDEN);
    assert!(!node.update_has_active_work());
    server.abort();
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn management_file_endpoints_require_the_local_switch() {
    let data_dir = std::env::temp_dir().join(format!("meshshare-browse-switch-{}", Uuid::new_v4()));
    let root = data_dir.join("root");
    std::fs::create_dir_all(&root).expect("create root");
    std::fs::write(root.join("note.txt"), b"data").expect("write file");
    let runtime = NodeRuntime::new(data_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    let state = super::AppState {
        runtime: runtime.clone(),
        control: super::ControlPlane::runtime_only(runtime.clone()),
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
        super::create_managed_outgoing_paths(super::State(state.clone()), super::Json(prepare()))
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
    let data_dir = std::env::temp_dir().join(format!("meshshare-browse-list-{}", Uuid::new_v4()));
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
    let data_dir = std::env::temp_dir().join(format!("meshshare-settings-api-{}", Uuid::new_v4()));
    let static_root = data_dir.join("web");
    let destination = data_dir.join("received");
    std::fs::create_dir_all(&static_root).expect("create static directory");
    std::fs::create_dir_all(&destination).expect("create destination");
    std::fs::write(static_root.join("index.html"), "<html></html>").expect("write static index");
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

    let wrong_node = runtime
        .http
        .post(format!("http://{address}/api/v1/clipboard"))
        .header("X-MeshShare-Target", "previous-occupant")
        .json(&serde_json::json!({ "text": "private draft" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        wrong_node.status(),
        reqwest::StatusCode::MISDIRECTED_REQUEST
    );
    assert!(runtime.clipboard_snapshot().is_none());

    let allowed = runtime
        .http
        .patch(format!("http://{address}/api/v1/settings"))
        .header("X-MeshShare-Target", runtime.node_id())
        .header("Origin", "http://192.168.1.10:37880")
        .json(&serde_json::json!({ "clipboard_sync_enabled": true }))
        .send()
        .await
        .expect("send allowed origin request");
    assert!(allowed.status().is_success());

    let invalid = runtime
        .http
        .patch(format!("http://{address}/api/v1/settings"))
        .json(&serde_json::json!({"clipboard_sync_enabled": false, "update_frequency": "monthly"}))
        .send()
        .await
        .unwrap();
    assert_eq!(invalid.status(), reqwest::StatusCode::BAD_REQUEST);
    assert!(invalid.json::<serde_json::Value>().await.unwrap()["error"].is_string());
    assert!(runtime
        .sync_enabled
        .load(std::sync::atomic::Ordering::SeqCst));

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
    node.addresses.push(crate::model::AddressInfo {
        address: "192.168.1.8".into(),
        url: "http://192.168.1.8:37880".into(),
        label: "LAN".into(),
        kind: "lan".into(),
    });
    for source in [None, Some("203.0.113.5:43122".parse().unwrap())] {
        assert!(incoming_peer_from_hello(node.clone(), &runtime, source).is_none());
    }
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
        Some("ws://100.64.0.8:37880/api/v1/ws".to_string())
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
    let data_dir = std::env::temp_dir().join(format!("meshshare-offer-replay-{}", Uuid::new_v4()));
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
            offer: offer.clone().into(),
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
        WireMessage::TransferOffer {
            offer: offer.into(),
        },
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
    let data_dir = std::env::temp_dir().join(format!("meshshare-offer-reply-{}", Uuid::new_v4()));
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
            WireMessage::TransferOffer {
                offer: offer.into(),
            },
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
    std::fs::write(static_root.join("index.html"), "<html></html>").expect("write static index");
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
    let data_dir = std::env::temp_dir().join(format!("meshshare-large-metrics-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&data_dir).expect("create test directory");
    let runtime = NodeRuntime::new(data_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    runtime.set_peer(PeerInfo {
        node_id: "peer".to_string(),
        node_name: "peer".to_string(),
        platform: "test".to_string(),
        addresses: Vec::new(),
        mobile_addresses: Vec::new(),
        terminal: None,
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

#[test]
fn terminal_hello_requires_consistent_host_and_client_keys() {
    use super::socket::validate_node_info;
    use crate::model::TERMINAL_SSH_CAPABILITY;
    let dir = std::env::temp_dir().join(format!("meshshare-hello-keys-{}", Uuid::new_v4()));
    let runtime =
        NodeRuntime::new_with_profile(dir.clone(), 0, "test".into(), NodeProfile::Android).unwrap();
    let mut info = runtime.node_info();
    info.platform = "linux".to_owned();
    assert!(validate_node_info(&info));
    info.capabilities.push(TERMINAL_SSH_CAPABILITY.to_owned());
    assert!(!validate_node_info(&info));
    let pin = info.terminal.as_ref().unwrap().client_fingerprint.clone();
    info.terminal.as_mut().unwrap().host_fingerprint = Some(pin);
    assert!(validate_node_info(&info));
    info.platform = "android".to_owned();
    assert!(!validate_node_info(&info));
    info.terminal.as_mut().unwrap().host_fingerprint = None;
    info.capabilities
        .retain(|cap| cap != TERMINAL_SSH_CAPABILITY);
    info.node_id.push('\n');
    assert!(!validate_node_info(&info));
    info.node_id.pop();
    info.terminal.as_mut().unwrap().client_fingerprint = "invalid".to_owned();
    assert!(!validate_node_info(&info));
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn invalid_hello_does_not_register_a_peer() {
    let data_dir = std::env::temp_dir().join(format!("meshshare-invalid-hello-{}", Uuid::new_v4()));
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
                terminal: None,
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
            terminal: None,
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
    let data_dir = std::env::temp_dir().join(format!("meshshare-mobile-reject-{}", Uuid::new_v4()));
    let static_root = data_dir.join("web");
    std::fs::create_dir_all(&static_root).expect("create static directory");
    std::fs::write(static_root.join("index.html"), "<html></html>").expect("write static index");
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
    std::fs::write(static_root.join("index.html"), "<html></html>").expect("write static index");
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
    std::fs::write(static_root.join("index.html"), "<html></html>").expect("write static index");
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
    std::fs::write(record.staged_directory.join("hello.txt"), bytes).expect("write staged file");
    runtime
        .stage_mobile_item(
            "mobile-download",
            0,
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
    // 上传方可能在最后一次 PUT 成功后才提交失败通知；已就绪的文件仍须可下载。
    let failed = runtime
        .http
        .post(format!(
            "http://{address}/api/v1/mobile/transfers/mobile-download/fail"
        ))
        .json(&serde_json::json!({"error": "上传响应超时"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<crate::model::MobileTransferView>()
        .await
        .unwrap();
    assert_eq!(failed.progress.status, TransferStatus::Ready);
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
    let data_dir = std::env::temp_dir().join(format!("meshshare-foreign-offer-{}", Uuid::new_v4()));
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
    std::fs::write(static_root.join("index.html"), "<html></html>").expect("write static index");
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
                0,
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
    std::fs::write(static_root.join("index.html"), "<html></html>").expect("write static index");
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
    let data_dir = std::env::temp_dir().join(format!("meshshare-upload-guard-{}", Uuid::new_v4()));
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
    let data_dir = std::env::temp_dir().join(format!("meshshare-multi-upload-{}", Uuid::new_v4()));
    let static_root = data_dir.join("web");
    let destination = data_dir.join("received");
    std::fs::create_dir_all(&static_root).expect("create static directory");
    std::fs::create_dir_all(&destination).expect("create destination");
    std::fs::write(static_root.join("index.html"), "<html></html>").expect("write static index");
    let runtime = NodeRuntime::new(data_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    let digest = |bytes: &[u8]| {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        format!("{:x}", hasher.finalize())
    };
    let first = b"one";
    let second = b"two2";
    let sender = NodeRuntime::new(data_dir.join("sender"), 0, "test".into()).unwrap();
    let offer = TransferOffer {
        transfer_id: "multi-upload".to_string(),
        source_node_id: sender.node_id(),
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
    sender.add_outgoing_transfer(offer.clone());
    sender.mark_transfer_accepted("multi-upload", 0).unwrap();
    sender.begin_transfer_upload("multi-upload").unwrap();
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
    let base = format!("http://{address}/api/v1/transfers/multi-upload");
    let put =
        |id: &str, bytes: &'static [u8]| sender.http.put(format!("{base}/items/{id}")).body(bytes);
    // 接收方已落盘，但发送方丢失首次 PUT 确认，双方随后进入失败状态。
    put("first", first)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    for node in [&sender, &runtime] {
        node.fail_transfer("multi-upload", "确认丢失".into(), TransferStatus::Failed)
            .unwrap();
    }
    let retry = sender
        .http
        .post(format!("{base}/retry"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(sender.apply_peer_progress(&runtime.node_id(), retry, None));
    sender.begin_transfer_upload("multi-upload").unwrap();
    assert!(runtime
        .confirm_received_item("multi-upload", "first", 0)
        .is_none());

    for (item_id, bytes) in [("first", first.as_slice()), ("second", second.as_slice())] {
        let progress: TransferProgress = put(item_id, bytes)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(progress.retry_count, 1);
        if item_id == "first" {
            assert_eq!(progress.status, TransferStatus::Uploading);
            assert_eq!(progress.completed_items, 1);
            assert_eq!(progress.completed_bytes, first.len() as u64);
        }
        assert!(sender.apply_peer_progress(&runtime.node_id(), progress, Some(item_id)));
    }
    assert_eq!(
        sender.transfer("multi-upload").unwrap().progress.status,
        TransferStatus::Completed
    );
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
    let data_dir = std::env::temp_dir().join(format!("meshshare-server-test-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&data_dir).expect("create test directory");
    let runtime = NodeRuntime::new(data_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    runtime.set_peer(PeerInfo {
        node_id: "peer".to_string(),
        node_name: "peer".to_string(),
        platform: "test".to_string(),
        addresses: Vec::new(),
        mobile_addresses: Vec::new(),
        terminal: None,
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
    let data_dir = std::env::temp_dir().join(format!("meshshare-forward-test-{}", Uuid::new_v4()));
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
        terminal: None,
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
    std::fs::write(static_root.join("index.html"), "<html></html>").expect("write static index");
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
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .expect("serve test router");
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
        terminal: None,
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
    let data_dir = std::env::temp_dir().join(format!("meshshare-ui-heartbeat-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&data_dir).expect("create test directory");
    let static_root = data_dir.join("web");
    std::fs::create_dir_all(&static_root).expect("create static directory");
    std::fs::write(static_root.join("index.html"), "<html></html>").expect("write static index");
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
    std::fs::write(static_root.join("index.html"), "<html></html>").expect("write static index");
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
    std::fs::write(static_root.join("index.html"), "<html></html>").expect("write static index");
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
    let data_dir = std::env::temp_dir().join(format!("meshshare-stream-chunks-{}", Uuid::new_v4()));
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

    let data_dir = std::env::temp_dir().join(format!("meshshare-live-upload-{}", Uuid::new_v4()));
    let static_root = data_dir.join("web");
    let destination = data_dir.join("received");
    std::fs::create_dir_all(&static_root).expect("create static directory");
    std::fs::create_dir_all(&destination).expect("create destination");
    std::fs::write(static_root.join("index.html"), "<html></html>").expect("write static index");
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
    std::fs::write(static_root.join("index.html"), "<html></html>").expect("write static index");
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
    std::fs::write(static_root.join("index.html"), "<html></html>").expect("write static index");
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
    std::fs::write(static_root.join("index.html"), "<html></html>").expect("write static index");
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

#[cfg(all(feature = "headless", target_os = "linux"))]
#[tokio::test]
async fn restart_refuses_wrong_target_and_an_active_terminal_before_contacting_systemd() {
    let dir = tempfile::tempdir().unwrap();
    let node =
        NodeRuntime::new_with_profile(dir.path().into(), 0, "1.0.0".into(), NodeProfile::Headless)
            .unwrap();
    node.set_update_frequency(crate::runtime::UpdateFrequency::Disabled)
        .unwrap();
    let control = super::ControlPlane::with_headless(node.clone());
    let state = super::AppState {
        runtime: node.clone(),
        control,
    };
    let mut headers = axum::http::HeaderMap::new();
    headers.insert("x-meshshare-target", HeaderValue::from_static("different"));
    assert_eq!(
        super::restart_headless(super::State(state.clone()), headers.clone())
            .await
            .unwrap_err()
            .status,
        super::StatusCode::MISDIRECTED_REQUEST
    );
    headers.insert("x-meshshare-target", node.node_id().parse().unwrap());
    let _session = node.reserve_terminal().unwrap();
    assert_eq!(
        super::restart_headless(super::State(state), headers)
            .await
            .unwrap_err()
            .status,
        super::StatusCode::CONFLICT
    );
    node.stopping
        .store(true, std::sync::atomic::Ordering::SeqCst);
}

#[tokio::test]
async fn managed_phone_transfer_uses_node_files_only_after_acceptance() {
    let dir = std::env::temp_dir().join(format!("meshshare-managed-phone-{}", Uuid::new_v4()));
    let runtime =
        NodeRuntime::new_with_profile(dir.clone(), 0, "test".into(), NodeProfile::Headless)
            .unwrap();
    let phone = runtime.create_mobile_session("browser".into()).unwrap();
    let state = super::AppState {
        runtime: runtime.clone(),
        control: super::ControlPlane::runtime_only(runtime.clone()),
    };
    let source = dir.join("file.txt");
    std::fs::write(&source, b"on the NAS").unwrap();
    let offer = || {
        super::create_managed_mobile_offer(
            super::State(state.clone()),
            super::Json(super::ManagedMobileRequest {
                session_id: phone.session_id.clone(),
                paths: vec![source.display().to_string()],
            }),
        )
    };
    let accept = |id: String| {
        super::accept_mobile_transfer(
            super::AxumPath(id),
            super::State(state.clone()),
            super::Json(super::MobileSessionRequest {
                session_id: phone.session_id.clone(),
            }),
        )
    };
    let wait_for = |id: String, expected: TransferStatus| {
        let runtime = runtime.clone();
        async move {
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    let record = runtime.mobile_transfer(&id).unwrap();
                    if record.progress.status == expected {
                        return record;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap()
        }
    };

    runtime.set_remote_file_access(false).unwrap();
    assert_eq!(
        offer().await.unwrap_err().status,
        super::StatusCode::FORBIDDEN
    );
    runtime.set_remote_file_access(true).unwrap();
    let first = offer().await.unwrap().0.progress;
    let id = first.transfer_id;
    let item = first.offer.unwrap().items[0].item_id.clone();
    assert_eq!(first.status, TransferStatus::Waiting);
    assert!(!runtime
        .mobile_transfer(&id)
        .unwrap()
        .staged_directory
        .exists());
    let err = super::upload_mobile_item(
        super::AxumPath((id.clone(), item.clone())),
        super::State(state.clone()),
        axum::body::Body::from("fake bytes"),
    )
    .await
    .unwrap_err();
    assert_eq!(err.status, super::StatusCode::FORBIDDEN);
    let _ = accept(id.clone()).await.unwrap();
    let ready = wait_for(id.clone(), TransferStatus::Ready).await;
    assert_eq!(
        ready.offer().items[0].sha256,
        format!("{:x}", Sha256::digest(b"on the NAS"))
    );
    assert_eq!(
        std::fs::read(runtime.mobile_item_path(&id, &item).unwrap()).unwrap(),
        b"on the NAS"
    );

    let changed = offer().await.unwrap().0.progress.transfer_id;
    std::fs::write(&source, b"changed after selection").unwrap();
    let _ = accept(changed.clone()).await.unwrap();
    let failed = wait_for(changed, TransferStatus::Failed).await;
    assert!(failed.progress.error.unwrap().contains("变化"));
    assert!(!failed.staged_directory.exists());
    std::fs::remove_dir_all(dir).unwrap();
}
