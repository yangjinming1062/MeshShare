use super::{
    bind_addresses, bindable_addresses, default_node_name, interface_rank, is_cellular_interface,
    is_placeholder_node_name, load_or_create_config, load_or_create_config_with_hostname,
    mobile_ca_url, network_addresses, network_addresses_for_bound, peer_channel,
    prune_clipboard_history_records_with_limit, sync_node_name, valid_history_delete_ids,
    ClipboardSnapshotRead, NodeProfile, NodeRuntime, PeerEndpoint, PeerEndpointSource,
    UpdateFrequency, CLIPBOARD_HISTORY_FILE, CLIPBOARD_PAYLOAD_DIR, DESKTOP_TEMP_REGISTRY_FILE,
    MAX_MOBILE_TRANSFER_RECORDS, TRANSFER_ACCEPTED_IDLE_TIMEOUT_MS, TRANSFER_ACTIVITY_TIMEOUT_MS,
    TRANSFER_HISTORY_FILE, TRANSFER_HISTORY_LIMIT,
};
use crate::model::{
    AddressInfo, ClipboardEvent, ClipboardHistoryMode, ClipboardSnapshot, DiskMetrics,
    GpuMemoryKind, GpuMetrics, HardwareMetrics, Hlc, LoadAverage, NodeInfo, PeerInfo, ReceiveMode,
    TransferItem, TransferOffer, TransferProgress, TransferStatus, WireMessage,
    CLIPBOARD_TEXT_LIMIT, CLIPBOARD_VIEW_CAPABILITY, MAX_HISTORY_DELETE_BATCH, MAX_MOBILE_SESSIONS,
    MAX_TRANSFER_RECORDS, METRICS_GPU_VIEW_CAPABILITY, METRICS_NETWORK_CAPABILITY,
    METRICS_SYSTEM_CAPABILITY, METRICS_SYSTEM_VIEW_CAPABILITY, TRANSFER_HISTORY_CAPABILITY,
};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::Arc;
use uuid::Uuid;

fn test_peer(node_id: &str, capabilities: Vec<String>) -> PeerInfo {
    PeerInfo {
        node_id: node_id.to_string(),
        node_name: "peer".to_string(),
        platform: "test".to_string(),
        addresses: Vec::new(),
        mobile_addresses: Vec::new(),
        terminal: None,
        capabilities,
        online: true,
        last_seen_ms: crate::clock::now_millis(),
        endpoint: None,
        source: "test".to_string(),
        metrics: None,
        metrics_received_at_ms: None,
        metrics_stale: false,
    }
}

fn temp_data_dir() -> PathBuf {
    let path = std::env::temp_dir().join(format!("meshshare-test-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&path).expect("create test directory");
    path
}

#[test]
fn peer_http_base_prefers_verified_websocket_endpoint() {
    let data_dir = temp_data_dir();
    let runtime = NodeRuntime::new_with_profile(
        data_dir.clone(),
        37880,
        "test".to_string(),
        NodeProfile::Desktop,
    )
    .expect("create runtime");
    let mut peer = test_peer("peer", Vec::new());
    peer.endpoint = Some("ws://100.64.0.2:37880/api/v1/ws".to_string());
    peer.addresses = vec![
        AddressInfo {
            address: "192.168.1.20".to_string(),
            url: "http://192.168.1.20:37880".to_string(),
            label: "Wi-Fi".to_string(),
            kind: "lan".to_string(),
        },
        AddressInfo {
            address: "100.64.0.2".to_string(),
            url: "http://100.64.0.2:37880".to_string(),
            label: "Tailscale".to_string(),
            kind: "tailscale".to_string(),
        },
    ];
    runtime.set_peer(peer);
    assert_eq!(
        runtime.peer_http_base("peer").as_deref(),
        Some("http://100.64.0.2:37880")
    );
    assert_eq!(
        runtime.peer_http_bases("peer"),
        vec![
            "http://100.64.0.2:37880".to_string(),
            "http://192.168.1.20:37880".to_string(),
        ]
    );
    let mut hello = test_peer("peer", Vec::new());
    hello.source = "websocket".to_string();
    hello.endpoint = Some("ws://192.168.1.20:37880/api/v1/ws".to_string());
    hello.addresses = runtime
        .peers_snapshot()
        .into_iter()
        .next()
        .expect("peer metadata")
        .addresses;
    runtime.set_peer(hello);
    assert_eq!(
        runtime.peer_http_base("peer").as_deref(),
        Some("http://100.64.0.2:37880")
    );
    let _ = std::fs::remove_dir_all(data_dir);
}

#[test]
fn bound_addresses_publish_only_network_ipv4_in_access_order() {
    let bound = [
        Ipv4Addr::LOCALHOST,
        Ipv4Addr::new(127, 0, 0, 2),
        Ipv4Addr::new(192, 168, 1, 10),
        Ipv4Addr::new(100, 64, 0, 2),
        Ipv4Addr::new(169, 254, 2, 3),
        Ipv4Addr::new(8, 8, 8, 8),
    ]
    .into_iter()
    .collect();
    let addresses = network_addresses_for_bound(37880, &bound);
    assert_eq!(
        addresses
            .iter()
            .map(|address| address.address.as_str())
            .collect::<Vec<_>>(),
        ["192.168.1.10", "100.64.0.2", "169.254.2.3"]
    );
}

#[test]
fn cellular_interfaces_are_recognized_by_name() {
    for name in [
        "rmnet_data0",
        "ccmni0",
        "radio0",
        "clat4",
        "ppp0",
        "wwan0",
        "pdp0",
        "v4-rmnet_data0",
        "v6-rmnet_data0",
    ] {
        assert!(is_cellular_interface(name), "{name} should be cellular");
    }
    for name in ["wlan0", "eth0", "en0", "vEthernet (WSL)"] {
        assert!(
            !is_cellular_interface(name),
            "{name} should not be cellular"
        );
    }
}

#[test]
fn android_skips_cellular_interfaces_when_binding() {
    let interfaces = vec![
        (
            "wlan0".to_string(),
            "192.168.1.20".parse::<IpAddr>().expect("wlan address"),
        ),
        (
            "rmnet_data0".to_string(),
            "10.136.144.220"
                .parse::<IpAddr>()
                .expect("cellular address"),
        ),
        (
            "vEthernet (WSL)".to_string(),
            "172.20.0.1".parse::<IpAddr>().expect("virtual address"),
        ),
    ];
    let android = bindable_addresses(NodeProfile::Android, interfaces.clone());
    assert!(android.contains(&Ipv4Addr::new(192, 168, 1, 20)));
    assert!(!android.contains(&Ipv4Addr::new(10, 136, 144, 220)));
    let desktop = bindable_addresses(NodeProfile::Desktop, interfaces);
    assert!(desktop.contains(&Ipv4Addr::new(192, 168, 1, 20)));
    assert!(desktop.contains(&Ipv4Addr::new(10, 136, 144, 220)));
    assert!(desktop.contains(&Ipv4Addr::new(172, 20, 0, 1)));
}

#[test]
fn android_skips_vpn_interfaces_but_keeps_tailscale() {
    let interfaces = vec![
        (
            "tun0".to_string(),
            "10.8.0.2".parse::<IpAddr>().expect("VPN address"),
        ),
        (
            "tailscale0".to_string(),
            "100.64.0.2".parse::<IpAddr>().expect("Tailscale address"),
        ),
    ];
    let android = bindable_addresses(NodeProfile::Android, interfaces);
    assert!(!android.contains(&Ipv4Addr::new(10, 8, 0, 2)));
    assert!(android.contains(&Ipv4Addr::new(100, 64, 0, 2)));
}

#[test]
fn physical_address_outranks_cellular_address() {
    let physical = Ipv4Addr::new(192, 168, 1, 20);
    let cellular = Ipv4Addr::new(10, 136, 144, 220);
    let bound = [physical, cellular].into_iter().collect();
    let names = HashMap::from([
        (physical, "wlan0".to_string()),
        (cellular, "rmnet_data0".to_string()),
    ]);
    let addresses = network_addresses(37880, &bound, &names);
    assert_eq!(
        addresses
            .iter()
            .map(|address| address.address.as_str())
            .collect::<Vec<_>>(),
        ["192.168.1.20", "10.136.144.220"]
    );
}

#[test]
fn mobile_ca_url_only_accepts_private_node_addresses() {
    for allowed in [
        "http://192.168.1.10:37880/api/v1/mobile/ca",
        "http://100.64.0.2:37880/api/v1/mobile/ca",
        "http://169.254.2.3:37880/api/v1/mobile/ca",
        "https://127.0.0.1:37881/api/v1/mobile/ca",
    ] {
        assert!(
            mobile_ca_url(allowed).is_ok(),
            "{allowed} should be allowed"
        );
    }
    for rejected in [
        "http://8.8.8.8:37880/api/v1/mobile/ca",
        "http://example.com/api/v1/mobile/ca",
        "file:///etc/passwd",
        "not a url",
    ] {
        assert!(
            mobile_ca_url(rejected).is_err(),
            "{rejected} should be rejected"
        );
    }
}

#[test]
fn physical_networks_rank_before_virtual_and_cellular_interfaces() {
    for name in [
        "wlan0",
        "eth0",
        "Ethernet",
        "Ethernet 2",
        "Wi-Fi",
        "en0",
        "wlp2s0",
    ] {
        assert_eq!(interface_rank(name), 0, "{name}");
    }
    assert_eq!(interface_rank("rmnet_data0"), 2);
    assert_eq!(interface_rank("vEthernet (WSL)"), 2);
    assert_eq!(interface_rank("docker0"), 2);
    assert_eq!(interface_rank("utun4"), 2);
    assert_eq!(interface_rank("vpn0"), 2);
    assert_eq!(interface_rank("ipsec0"), 2);
    assert_eq!(interface_rank("wireguard0"), 2);
    assert_eq!(interface_rank("unknown0"), 1);
}

#[test]
fn actual_bind_addresses_are_explicit_allowed_ipv4() {
    let addresses = bind_addresses();
    assert!(!addresses.is_empty());
    assert!(addresses.iter().all(|address| {
        !address.is_unspecified()
            && (address.is_loopback()
                || address.is_private()
                || address.is_link_local()
                || (u32::from(*address) & 0xffc0_0000) == 0x6440_0000)
    }));
}

#[test]
fn actual_bind_addresses_accept_ephemeral_listeners_without_wildcards() {
    for address in bind_addresses() {
        let listener = std::net::TcpListener::bind((address, 0))
            .expect("allowed interface address must be bindable");
        assert_eq!(
            listener.local_addr().expect("listener address").ip(),
            address
        );
    }
}

fn mobile_offer(runtime: &NodeRuntime, transfer_id: &str, total_bytes: u64) -> TransferOffer {
    TransferOffer {
        transfer_id: transfer_id.to_string(),
        source_node_id: runtime.node_id(),
        source_name: "desktop".to_string(),
        target_node_id: runtime.node_id(),
        items: vec![TransferItem {
            item_id: format!("{transfer_id}-item"),
            name: "file.bin".to_string(),
            relative_path: "file.bin".to_string(),
            size: total_bytes,
            sha256: "hash".to_string(),
            media_type: "application/octet-stream".to_string(),
            directory: false,
        }],
        total_bytes,
        created_at_ms: crate::clock::now_millis(),
    }
}

#[test]
fn placeholder_hostnames_are_not_node_names() {
    for name in ["", "  ", "localhost", "LOCALHOST", "localhost.localdomain"] {
        assert!(
            is_placeholder_node_name(name),
            "{name} should be a placeholder"
        );
    }
    for name in ["demo-desktop", "demo-phone", "demo-workstation.local"] {
        assert!(
            !is_placeholder_node_name(name),
            "{name} should be a usable name"
        );
    }
}

#[test]
fn android_nodes_default_to_mobile_name() {
    assert_eq!(
        default_node_name(NodeProfile::Android, Some("localhost".to_string())),
        "移动设备"
    );
    assert_eq!(default_node_name(NodeProfile::Android, None), "移动设备");
    assert_eq!(
        default_node_name(NodeProfile::Android, Some("Redmi-K70".to_string())),
        "Redmi-K70"
    );
}

#[test]
fn desktop_nodes_keep_hostname_or_fallback() {
    assert_eq!(
        default_node_name(NodeProfile::Desktop, Some("YJM-MI-PC".to_string())),
        "YJM-MI-PC"
    );
    assert_eq!(
        default_node_name(NodeProfile::Desktop, Some(" localhost ".to_string())),
        "MeshShare 节点"
    );
    assert_eq!(
        default_node_name(NodeProfile::Desktop, None),
        "MeshShare 节点"
    );
}

#[test]
fn hostname_change_replaces_saved_node_name() {
    let path = temp_data_dir();
    let original = load_or_create_config_with_hostname(
        &path,
        NodeProfile::Desktop,
        Some("OLD-PC".to_string()),
    )
    .expect("create config");
    let config = load_or_create_config_with_hostname(
        &path,
        NodeProfile::Desktop,
        Some("NEW-PC".to_string()),
    )
    .expect("load config");
    assert_eq!(config.node_name, "NEW-PC");
    let persisted: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(path.join("node.json")).expect("read config file"),
    )
    .expect("parse config file");
    assert_eq!(persisted["node_name"], "NEW-PC");
    assert_eq!(persisted["node_id"], original.node_id);
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn saved_node_name_survives_missing_hostname() {
    let path = temp_data_dir();
    load_or_create_config_with_hostname(&path, NodeProfile::Desktop, Some("手动名称".into()))
        .expect("create named config");
    let config = load_or_create_config_with_hostname(&path, NodeProfile::Desktop, None)
        .expect("load config");
    assert_eq!(config.node_name, "手动名称");
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn unavailable_names_use_platform_default() {
    let mut name = "localhost".to_string();
    assert!(sync_node_name(
        &mut name,
        NodeProfile::Android,
        Some("localhost".to_string())
    ));
    assert_eq!(name, "移动设备");

    let mut kept = "Redmi-K70".to_string();
    assert!(!sync_node_name(
        &mut kept,
        NodeProfile::Android,
        Some("localhost".to_string())
    ));
    assert_eq!(kept, "Redmi-K70");

    let mut blank = String::new();
    assert!(sync_node_name(&mut blank, NodeProfile::Desktop, None));
    assert_eq!(blank, "MeshShare 节点");
}

#[test]
fn long_hostname_is_truncated_on_char_boundary() {
    let mut ascii = "a".repeat(300);
    assert!(sync_node_name(
        &mut ascii,
        NodeProfile::Desktop,
        Some("b".repeat(300))
    ));
    assert_eq!(ascii.len(), 256);

    let long_name = format!("{}{}", "a".repeat(255), "中");
    let mut names = long_name.clone();
    assert!(sync_node_name(
        &mut names,
        NodeProfile::Desktop,
        Some(long_name)
    ));
    assert_eq!(names.len(), 255);
    assert!(names.is_char_boundary(names.len()));
}

#[test]
fn new_nodes_use_default_settings() {
    let path = temp_data_dir();
    let config = load_or_create_config(&path, NodeProfile::Desktop).expect("load config");
    assert!(config.monitor_visible);
    assert_eq!(config.monitor_position, None);
    assert!(!config.monitor_locked);
    assert!(config.autostart_enabled);
    assert_eq!(config.update_frequency, UpdateFrequency::Weekly);
    assert_eq!(config.update_last_checked_at_ms, None);
    assert_eq!(config.pending_update_version, None);
    assert_eq!(
        config.clipboard_history_mode,
        crate::model::ClipboardHistoryMode::Count
    );
    assert_eq!(
        config.clipboard_history_count,
        crate::model::DEFAULT_CLIPBOARD_HISTORY_COUNT
    );
    assert_eq!(
        config.clipboard_history_days,
        crate::model::DEFAULT_CLIPBOARD_HISTORY_DAYS
    );
    assert_eq!(config.receive_mode, ReceiveMode::Confirm);
    assert_eq!(config.receive_directory, None);
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn auto_receive_settings_are_persisted_and_used_for_new_transfers() {
    let path = temp_data_dir();
    let destination = path.join("received");
    std::fs::create_dir_all(&destination).expect("create receive directory");
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    runtime
        .set_receive_settings(ReceiveMode::Auto, Some(destination.clone()))
        .expect("save receive settings");
    assert_eq!(runtime.receive_settings().mode, ReceiveMode::Auto);
    let default_path = runtime
        .receive_settings()
        .default_path
        .expect("default receive path");
    assert!(
        !default_path.starts_with(r"\\?\"),
        "接收设置不应展示扩展长度前缀：{default_path}"
    );
    assert_eq!(
        std::fs::canonicalize(&default_path).expect("canonical default path"),
        std::fs::canonicalize(&destination).expect("canonical destination")
    );
    let offer = mobile_offer(&runtime, "auto-receive", 1);
    assert!(runtime.add_transfer(offer));
    let progress = runtime
        .transfer("auto-receive")
        .expect("auto transfer")
        .progress;
    assert_eq!(progress.status, TransferStatus::Accepted);
    let saved = progress.destination.expect("auto destination");
    assert!(
        !saved.starts_with(r"\\?\"),
        "接收记录不应展示扩展长度前缀：{saved}"
    );
    std::fs::remove_dir_all(&destination).expect("remove receive directory");
    let offer = mobile_offer(&runtime, "fallback-confirm", 1);
    assert!(runtime.add_transfer(offer));
    assert_eq!(
        runtime
            .transfer("fallback-confirm")
            .expect("fallback transfer")
            .progress
            .status,
        TransferStatus::Waiting
    );
    let _ = std::fs::remove_dir_all(path);
}

#[tokio::test]
async fn auto_receive_publishes_one_progress_with_the_manifest() {
    let path = temp_data_dir();
    let destination = path.join("received");
    std::fs::create_dir_all(&destination).expect("create receive directory");
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    runtime
        .set_receive_settings(ReceiveMode::Auto, Some(destination))
        .expect("save receive settings");
    let mut events = runtime.subscribe();

    assert!(runtime.add_transfer(mobile_offer(&runtime, "auto-order", 1)));
    let first = events.recv().await.expect("receive progress event");
    assert!(matches!(
        first,
        WireMessage::TransferProgress { progress }
            if progress.status == TransferStatus::Accepted && progress.offer.is_some()
    ));
    assert!(matches!(
        events.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn corrupted_config_is_reported_without_replacing_node_identity() {
    let path = temp_data_dir();
    let config_path = path.join("node.json");
    for contents in ["{\"node_id\":", r#"{"node_id":"keep","node_name":"test"}"#] {
        std::fs::write(&config_path, contents).expect("write invalid config");
        assert!(load_or_create_config(&path, NodeProfile::Desktop).is_err());
        assert_eq!(std::fs::read_to_string(&config_path).unwrap(), contents);
    }
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn existing_install_opens_its_saved_identity_permissions_and_history() {
    use serde_json::json;
    use std::fs;
    let root = temp_data_dir();
    let policy = br#"{"policy":{"enabled":true},"signature":"previously confirmed locally"}"#;
    fs::write(root.join("terminal-access.json"), policy).unwrap();
    let name = default_node_name(NodeProfile::Desktop, super::system_hostname());
    let config = serde_json::to_vec(&json!({
        "node_id": "original-node", "node_name": name, "monitor_visible": false,
        "monitor_position": {"x": 10, "y": 20}, "monitor_locked": true,
        "autostart_enabled": false, "update_frequency": "disabled",
        "clipboard_history_mode": "count", "clipboard_history_count": 42, "clipboard_history_days": 7,
        "receive_mode": "confirm", "remote_file_access": true,
        "terminal_access_migration": {"accepted_policy": format!("{:x}", Sha256::digest(policy))}
    })).unwrap();
    fs::write(root.join("node.json"), &config).unwrap();
    let ca = crate::mobile_tls::ensure(&root, &[std::net::Ipv4Addr::LOCALHOST]).unwrap();
    let ca_bytes = fs::read(&ca.ca_certificate_path).unwrap();
    let ca_key_path = root.join("mobile-tls/ca-key.pem");
    let ca_key_bytes = fs::read(&ca_key_path).unwrap();

    let clock = json!({"physical_ms": 1, "logical": 0, "node_id": "from"});
    let snapshot = json!({
        "event_id": "copy", "source_node_id": "from", "source_name": "source",
        "clock": clock, "text": "hello", "updated_at_ms": 1
    });
    fs::write(root.join("clipboard-current.json"), snapshot.to_string()).unwrap();
    fs::create_dir(root.join(CLIPBOARD_PAYLOAD_DIR)).unwrap();
    fs::write(root.join(CLIPBOARD_PAYLOAD_DIR).join("copy.txt"), b"hello").unwrap();
    let clipboard_index = json!([{
        "event_id": "copy", "source_node_id": "from", "source_name": "source", "clock": clock,
        "updated_at_ms": 1, "recorded_at_ms": 1, "content_hash": crate::clipboard::payload_hash(Some("hello"), None),
        "text_path": "copy.txt", "text_len": 5, "image_len": 0
    }]);
    fs::write(
        root.join(CLIPBOARD_HISTORY_FILE),
        clipboard_index.to_string(),
    )
    .unwrap();
    let transfer_history = json!({"entries": [{
        "transfer_id": "sent", "direction": "send", "peer_node_id": "target", "peer_name": "target",
        "status": "completed", "completed_bytes": 3, "total_bytes": 3,
        "completed_items": 1, "total_items": 1, "finished_at_ms": 1
    }], "pending_deletions": {}});
    fs::write(
        root.join(TRANSFER_HISTORY_FILE),
        transfer_history.to_string(),
    )
    .unwrap();

    let node = NodeRuntime::new_with_profile(root.clone(), 0, "test".into(), NodeProfile::Desktop)
        .unwrap();
    assert_eq!(node.node_id(), "original-node");
    assert_eq!(node.update_settings().0, UpdateFrequency::Disabled);
    assert!(node.remote_file_access());
    assert_eq!(node.clipboard_history_settings().count, 42);
    assert_eq!(
        node.clipboard_history_detail("copy")
            .unwrap()
            .snapshot
            .text
            .as_deref(),
        Some("hello")
    );
    assert_eq!(node.clipboard_snapshot().unwrap().event_id, "copy");
    assert_eq!(node.transfer_history_snapshot(None)[0].transfer_id, "sent");
    #[cfg(all(feature = "gui", not(target_os = "android")))]
    assert!(node.ssh.host_fingerprint().is_some());
    let reopened_ca = crate::mobile_tls::ensure(&root, &[std::net::Ipv4Addr::LOCALHOST]).unwrap();
    assert_eq!(reopened_ca.fingerprint, ca.fingerprint);
    assert_eq!(fs::read(reopened_ca.ca_certificate_path).unwrap(), ca_bytes);
    assert_eq!(fs::read(ca_key_path).unwrap(), ca_key_bytes);
    assert_eq!(fs::read(root.join("node.json")).unwrap(), config);
    assert_eq!(fs::read(root.join("terminal-access.json")).unwrap(), policy);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn autostart_preference_is_persisted() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    assert!(runtime.autostart_enabled());
    runtime
        .set_autostart_enabled(false)
        .expect("disable autostart");

    let restored = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("restore runtime");
    assert!(!restored.autostart_enabled());
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn update_preference_and_pending_state_are_persisted() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    runtime
        .set_update_frequency(UpdateFrequency::Daily)
        .expect("save update frequency");
    runtime
        .set_update_check_state(Some(123), Some("1.2.3".to_string()))
        .expect("save update state");

    let restored = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("restore runtime");
    assert_eq!(
        restored.update_settings(),
        (UpdateFrequency::Daily, Some(123), Some("1.2.3".to_string()))
    );
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn terminal_reservation_and_update_install_exclude_each_other() {
    let path = temp_data_dir();
    let profile = if cfg!(all(feature = "headless", not(feature = "gui"))) {
        NodeProfile::Headless
    } else {
        NodeProfile::current()
    };
    let runtime = NodeRuntime::new_with_profile(path.clone(), 0, "test".into(), profile).unwrap();
    let local = runtime.reserve_terminal().unwrap();
    let remote = runtime.reserve_terminal().unwrap();
    assert!(!runtime.begin_update_install());
    drop(local);
    assert!(runtime.update_has_active_work());
    drop(remote);
    assert!(runtime.begin_update_install());
    assert!(runtime.reserve_terminal().is_err());
    runtime.end_update_install();
    assert!(runtime.reserve_terminal().is_ok());
    assert_eq!(
        runtime.supports_capability(crate::model::TERMINAL_SSH_CAPABILITY),
        !runtime.is_android_profile()
    );
    assert!(!NodeProfile::Android
        .capabilities()
        .iter()
        .any(|item| item == crate::model::TERMINAL_SSH_CAPABILITY));
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn active_transfers_block_update_installation() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    assert!(!runtime.has_active_transfers());
    let offer = mobile_offer(&runtime, "active", 1);
    runtime.add_outgoing_transfer(offer);
    assert!(runtime.has_active_transfers());
    runtime
        .fail_transfer(
            "active",
            "test complete".to_string(),
            TransferStatus::Completed,
        )
        .expect("finish transfer");
    assert!(!runtime.has_active_transfers());

    let session = runtime
        .create_mobile_session("phone".to_string())
        .expect("create mobile session");
    runtime
        .create_mobile_transfer(
            &session.session_id,
            mobile_offer(&runtime, "mobile-active", 1),
        )
        .expect("create mobile transfer");
    assert!(runtime.has_active_transfers());
    runtime
        .fail_mobile_transfer("mobile-active", "test complete".to_string())
        .expect("finish mobile transfer");
    runtime.add_transfer(mobile_offer(&runtime, "retry-during-update", 1));
    let failed = runtime
        .fail_transfer(
            "retry-during-update",
            "test failure".to_string(),
            TransferStatus::Failed,
        )
        .expect("mark transfer failed");
    assert!(!runtime.has_active_transfers());
    let pending = mobile_offer(&runtime, "staging-before-update", 1);
    runtime.begin_outgoing_staging(pending.clone()).unwrap();
    assert!(!runtime.begin_update_install());
    runtime.remove_outgoing_staging(&pending.transfer_id);
    assert!(runtime.begin_update_install());
    assert!(runtime.begin_outgoing_staging(pending).is_err());
    let source = path.join("source.txt");
    std::fs::write(&source, "payload").unwrap();
    assert_eq!(
        runtime
            .begin_outgoing_paths("peer".into(), vec![source.display().to_string()])
            .unwrap_err(),
        super::UPDATE_INSTALLING_MESSAGE
    );
    let blocked = runtime.add_outgoing_transfer(mobile_offer(&runtime, "blocked", 1));
    assert_eq!(blocked.status, TransferStatus::Failed);
    assert!(runtime
        .create_mobile_transfer(
            &session.session_id,
            mobile_offer(&runtime, "blocked-mobile", 1)
        )
        .is_err());
    assert!(matches!(
        runtime.retry_transfer("retry-during-update", TransferStatus::Accepted),
        Err(super::TransferRetryError::InstallingUpdate)
    ));
    runtime.apply_peer_progress(
        &runtime.node_id(),
        super::TransferProgress {
            status: TransferStatus::Accepted,
            retry_count: 1,
            ..failed
        },
        None,
    );
    assert!(!runtime.has_active_transfers());
    runtime.end_update_install();
    assert!(runtime
        .retry_transfer("retry-during-update", TransferStatus::Accepted)
        .is_ok());
    assert!(runtime.has_active_transfers());
    let _ = std::fs::remove_dir_all(path);
}

#[tokio::test]
async fn ca_download_rejects_oversized_chunked_body_before_eof() {
    use futures_util::StreamExt;

    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    let app = axum::Router::new().route(
        "/ca.crt",
        axum::routing::get(|| async {
            let first = futures_util::stream::once(async {
                Ok::<_, std::io::Error>(axum::body::Bytes::from(vec![
                    b'x';
                    super::MAX_MOBILE_CA_BYTES + 1
                ]))
            });
            axum::body::Body::from_stream(first.chain(futures_util::stream::pending()))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test listener");
    let address = listener.local_addr().expect("listener address");
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve CA fixture");
    });
    let url = format!("http://{address}/ca.crt");
    let error = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        runtime.fetch_mobile_ca(&url),
    )
    .await
    .expect("reject before the sender reaches EOF")
    .expect_err("reject oversized CA response");
    assert!(error.contains("超出大小限制"));
    server.abort();
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn monitor_position_lock_and_visibility_are_persisted() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    runtime
        .set_monitor_position(crate::model::MonitorPosition::new(123, 456))
        .expect("save monitor position");
    runtime.set_monitor_locked(true).expect("save monitor lock");
    runtime
        .set_monitor_visible(false)
        .expect("save monitor visibility");

    let restored = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("restore runtime");
    assert_eq!(
        restored.monitor_position(),
        Some(crate::model::MonitorPosition::new(123, 456))
    );
    assert!(restored.monitor_locked());
    assert!(!restored.monitor_visible());
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn failed_monitor_settings_write_does_not_mutate_runtime_state() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    std::fs::remove_dir_all(&path).expect("remove config directory");

    assert!(runtime.set_monitor_visible(false).is_err());
    assert!(runtime.monitor_visible());
    assert!(runtime.set_monitor_locked(true).is_err());
    assert!(!runtime.monitor_locked());
    assert!(runtime
        .set_monitor_position(crate::model::MonitorPosition::new(10, 20))
        .is_err());
    assert_eq!(runtime.monitor_position(), None);
    assert!(runtime.set_autostart_enabled(false).is_err());
    assert!(runtime.autostart_enabled());
    assert!(runtime
        .set_update_frequency(UpdateFrequency::Daily)
        .is_err());
    assert_eq!(runtime.update_settings().0, UpdateFrequency::Weekly);
    assert!(runtime
        .set_update_check_state(Some(42), Some("1.2.3".to_string()))
        .is_err());
    assert_eq!(runtime.update_settings().1, None);
    assert_eq!(runtime.update_settings().2, None);
}

#[test]
fn metrics_are_only_sent_to_capable_peers() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    let (tx, mut rx) = peer_channel(8);
    runtime.set_peer(test_peer(
        "capable",
        vec![METRICS_SYSTEM_CAPABILITY.to_string()],
    ));
    runtime.add_peer_channel("capable".to_string(), tx);
    runtime.set_peer(test_peer("no-metrics", Vec::new()));
    let (no_metrics_tx, mut no_metrics_rx) = peer_channel(8);
    runtime.add_peer_channel("no-metrics".to_string(), no_metrics_tx);
    runtime.publish_metrics(HardwareMetrics {
        node_id: runtime.node_id(),
        sampled_at_ms: 1,
        cpu_usage_percent: Some(1.0),
        memory_used_bytes: None,
        memory_total_bytes: None,
        network_down_bytes_per_sec: None,
        network_up_bytes_per_sec: None,
        gpus: vec![GpuMetrics {
            name: "GPU".to_string(),
            utilization_percent: Some(50.0),
            memory_used_bytes: Some(1),
            memory_total_bytes: Some(2),
            memory_kind: GpuMemoryKind::Dedicated,
            unavailable_reason: None,
        }],
        ..Default::default()
    });
    let message = rx.try_recv().expect("capable peer receives metrics");
    match message {
        crate::model::WireMessage::MetricsUpdate { metrics } => {
            assert!(metrics.gpus.is_empty());
        }
        _ => panic!("unexpected message"),
    }
    assert!(no_metrics_rx.try_recv().is_err());
    runtime
        .stopping
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn network_metric_peers_receive_only_network_fields() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    let (tx, mut rx) = peer_channel(8);
    runtime.set_peer(test_peer(
        "network-only",
        vec![METRICS_NETWORK_CAPABILITY.to_string()],
    ));
    runtime.add_peer_channel("network-only".to_string(), tx);
    runtime.publish_metrics(HardwareMetrics {
        node_id: runtime.node_id(),
        sampled_at_ms: 1,
        cpu_usage_percent: Some(12.0),
        cpu_model: Some("CPU".to_string()),
        memory_used_bytes: Some(10),
        memory_total_bytes: Some(20),
        memory_available_bytes: Some(10),
        swap_used_bytes: Some(1),
        swap_total_bytes: Some(2),
        load_average: Some(LoadAverage {
            one: 1.0,
            five: 0.8,
            fifteen: 0.6,
        }),
        uptime_seconds: Some(100),
        network_down_bytes_per_sec: Some(30),
        network_up_bytes_per_sec: Some(40),
        gpus: vec![GpuMetrics {
            name: "GPU".to_string(),
            utilization_percent: Some(50.0),
            memory_used_bytes: Some(1),
            memory_total_bytes: Some(2),
            memory_kind: GpuMemoryKind::Dedicated,
            unavailable_reason: None,
        }],
        disks: vec![DiskMetrics {
            name: "System".to_string(),
            mount_point: "/".to_string(),
            file_system: "ext4".to_string(),
            total_bytes: 100,
            available_bytes: 40,
            removable: false,
        }],
        ..Default::default()
    });
    let message = rx.try_recv().expect("network peer receives metrics");
    match message {
        WireMessage::MetricsUpdate { metrics } => {
            assert_eq!(metrics.cpu_usage_percent, None);
            assert_eq!(metrics.cpu_model, None);
            assert_eq!(metrics.memory_used_bytes, None);
            assert_eq!(metrics.memory_available_bytes, None);
            assert_eq!(metrics.swap_total_bytes, None);
            assert_eq!(metrics.load_average, None);
            assert_eq!(metrics.uptime_seconds, None);
            assert!(metrics.disks.is_empty());
            assert_eq!(metrics.network_down_bytes_per_sec, Some(30));
            assert_eq!(metrics.network_up_bytes_per_sec, Some(40));
            assert!(metrics.gpus.is_empty());
        }
        _ => panic!("unexpected message"),
    }
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn android_view_peers_receive_desktop_system_and_gpu_metrics() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    let (tx, mut rx) = peer_channel(8);
    runtime.set_peer(test_peer(
        "android",
        vec![
            METRICS_NETWORK_CAPABILITY.to_string(),
            METRICS_SYSTEM_VIEW_CAPABILITY.to_string(),
            METRICS_GPU_VIEW_CAPABILITY.to_string(),
        ],
    ));
    runtime.add_peer_channel("android".to_string(), tx);
    runtime.publish_metrics(HardwareMetrics {
        node_id: runtime.node_id(),
        sampled_at_ms: 1,
        cpu_usage_percent: Some(12.0),
        memory_used_bytes: Some(10),
        memory_total_bytes: Some(20),
        network_down_bytes_per_sec: Some(30),
        network_up_bytes_per_sec: Some(40),
        gpus: vec![GpuMetrics {
            name: "GPU".to_string(),
            utilization_percent: Some(50.0),
            memory_used_bytes: Some(1),
            memory_total_bytes: Some(2),
            memory_kind: GpuMemoryKind::Dedicated,
            unavailable_reason: None,
        }],
        ..Default::default()
    });
    match rx.try_recv().expect("Android view peer receives metrics") {
        WireMessage::MetricsUpdate { metrics } => {
            assert_eq!(metrics.cpu_usage_percent, Some(12.0));
            assert_eq!(metrics.memory_used_bytes, Some(10));
            assert_eq!(metrics.memory_total_bytes, Some(20));
            assert_eq!(metrics.network_down_bytes_per_sec, Some(30));
            assert_eq!(metrics.network_up_bytes_per_sec, Some(40));
            assert_eq!(metrics.gpus.len(), 1);
        }
        _ => panic!("unexpected message"),
    }
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn android_profile_advertises_transfer_network_and_view_metrics() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new_with_profile(
        path.clone(),
        0,
        env!("CARGO_PKG_VERSION").into(),
        super::NodeProfile::Android,
    )
    .expect("create Android runtime");
    let node = runtime.node_info();
    assert!(runtime.is_android_profile());
    assert!(node.capabilities.iter().any(|item| item == "transfer.file"));
    assert!(node
        .capabilities
        .iter()
        .any(|item| item == METRICS_NETWORK_CAPABILITY));
    assert!(!node
        .capabilities
        .iter()
        .any(|item| item == METRICS_SYSTEM_CAPABILITY));
    assert!(node
        .capabilities
        .iter()
        .any(|item| item == METRICS_SYSTEM_VIEW_CAPABILITY));
    assert!(node
        .capabilities
        .iter()
        .any(|item| item == METRICS_GPU_VIEW_CAPABILITY));
    assert!(!node
        .capabilities
        .iter()
        .any(|item| item == "clipboard.text"));
    assert!(node
        .capabilities
        .iter()
        .any(|item| item == CLIPBOARD_VIEW_CAPABILITY));
    assert!(!node.capabilities.iter().any(|item| item == "web.mobile"));
    let _ = std::fs::remove_dir_all(path);
}

fn named_peer(node_id: &str, node_name: &str, capabilities: Vec<String>) -> PeerInfo {
    let mut peer = test_peer(node_id, capabilities);
    peer.node_name = node_name.to_string();
    peer
}

fn incoming_history_offer(
    runtime: &NodeRuntime,
    transfer_id: &str,
    source_node_id: &str,
) -> TransferOffer {
    TransferOffer {
        transfer_id: transfer_id.to_string(),
        source_node_id: source_node_id.to_string(),
        source_name: format!("{source_node_id}-name"),
        target_node_id: runtime.node_id(),
        items: vec![TransferItem {
            item_id: format!("{transfer_id}-item"),
            name: "file.bin".to_string(),
            relative_path: "file.bin".to_string(),
            size: 4,
            sha256: "0".repeat(64),
            media_type: "application/octet-stream".to_string(),
            directory: false,
        }],
        total_bytes: 4,
        created_at_ms: crate::clock::now_millis(),
    }
}

fn outgoing_history_offer(
    runtime: &NodeRuntime,
    transfer_id: &str,
    target_node_id: &str,
) -> TransferOffer {
    TransferOffer {
        transfer_id: transfer_id.to_string(),
        source_node_id: runtime.node_id(),
        source_name: "本机".to_string(),
        target_node_id: target_node_id.to_string(),
        items: vec![TransferItem {
            item_id: format!("{transfer_id}-item"),
            name: "file.bin".to_string(),
            relative_path: "file.bin".to_string(),
            size: 4,
            sha256: "0".repeat(64),
            media_type: "application/octet-stream".to_string(),
            directory: false,
        }],
        total_bytes: 4,
        created_at_ms: crate::clock::now_millis(),
    }
}

fn complete_incoming(runtime: &NodeRuntime, transfer_id: &str, source_node_id: &str) {
    runtime.add_transfer(incoming_history_offer(runtime, transfer_id, source_node_id));
    runtime
        .mark_item_received(
            transfer_id,
            &format!("{transfer_id}-item"),
            4,
            "0".repeat(64),
        )
        .expect("complete incoming transfer");
}

#[test]
fn transfer_progress_reports_in_flight_bytes_without_double_counting() {
    let path = temp_data_dir();
    let destination = path.join("received");
    std::fs::create_dir_all(&destination).expect("create destination");
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    let mut offer = incoming_history_offer(&runtime, "live-progress", "peer-sender");
    offer.items[0].size = 100;
    offer.total_bytes = 100;
    runtime.add_transfer(offer);
    runtime
        .set_transfer_acceptance("live-progress", destination)
        .expect("accept transfer");
    runtime
        .begin_transfer_upload("live-progress")
        .expect("begin upload");

    for (received, expected) in [(40u64, 40u64), (60, 60), (30, 60)] {
        let progress = runtime
            .report_transfer_progress("live-progress", "live-progress-item", received)
            .expect("report progress");
        assert_eq!(progress.completed_bytes, expected);
        assert!(progress.offer.is_none());
        assert_eq!(progress.completed_items, 0);
        assert_eq!(progress.status, TransferStatus::Uploading);
    }
    let capped = runtime
        .report_transfer_progress("live-progress", "live-progress-item", 999)
        .expect("report capped progress");
    assert_eq!(capped.completed_bytes, 100);
    let snapshot = runtime.transfers_snapshot().pop().unwrap();
    assert_eq!(snapshot.completed_bytes, 100);
    assert_eq!(
        snapshot.offer.unwrap().items[0].item_id,
        "live-progress-item"
    );

    let completed = runtime
        .mark_item_received("live-progress", "live-progress-item", 100, "0".repeat(64))
        .expect("complete transfer");
    assert_eq!(completed.completed_bytes, 100);
    assert_eq!(completed.completed_items, 1);
    assert_eq!(completed.status, TransferStatus::Completed);
    let history = runtime.transfer_history_snapshot(None);
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].completed_bytes, 100);
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn transfer_progress_continues_from_committed_bytes_across_items() {
    let path = temp_data_dir();
    let destination = path.join("received");
    std::fs::create_dir_all(&destination).expect("create destination");
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    let mut offer = incoming_history_offer(&runtime, "multi-live", "peer-sender");
    offer.items[0].item_id = "first".to_string();
    offer.items[0].size = 10;
    let mut second = offer.items[0].clone();
    second.item_id = "second".to_string();
    second.relative_path = "second.bin".to_string();
    second.size = 20;
    offer.items.push(second);
    offer.total_bytes = 30;
    runtime.add_transfer(offer);
    runtime
        .set_transfer_acceptance("multi-live", destination)
        .expect("accept transfer");
    runtime
        .begin_transfer_upload("multi-live")
        .expect("begin upload");

    assert_eq!(
        runtime
            .report_transfer_progress("multi-live", "first", 10)
            .expect("report first")
            .completed_bytes,
        10
    );
    assert_eq!(
        runtime
            .report_transfer_progress("multi-live", "first", 5)
            .expect("keep first high watermark")
            .completed_bytes,
        10
    );
    let after_first = runtime
        .mark_item_received("multi-live", "first", 10, "0".repeat(64))
        .expect("commit first");
    assert_eq!(after_first.completed_bytes, 10);
    assert_eq!(after_first.completed_items, 1);
    assert_eq!(after_first.status, TransferStatus::Uploading);
    assert_eq!(
        runtime
            .report_transfer_progress("multi-live", "second", 5)
            .expect("report second")
            .completed_bytes,
        15
    );
    let completed = runtime
        .mark_item_received("multi-live", "second", 20, "0".repeat(64))
        .expect("commit second");
    assert_eq!(completed.completed_bytes, 30);
    assert_eq!(completed.completed_items, 2);
    assert_eq!(completed.status, TransferStatus::Completed);
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn outgoing_progress_uses_receiver_ack_as_new_baseline() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    let mut offer = outgoing_history_offer(&runtime, "send-live", "peer-target");
    offer.items[0].item_id = "first".to_string();
    offer.items[0].size = 10;
    let mut second = offer.items[0].clone();
    second.item_id = "second".to_string();
    second.relative_path = "second.bin".to_string();
    second.size = 20;
    offer.items.push(second);
    offer.total_bytes = 30;
    runtime.add_outgoing_transfer(offer);
    runtime
        .transfers
        .write()
        .get_mut("send-live")
        .expect("find outgoing transfer")
        .progress
        .status = TransferStatus::Accepted;
    runtime
        .begin_transfer_upload("send-live")
        .expect("begin upload");
    assert_eq!(
        runtime
            .report_transfer_progress("send-live", "first", 10)
            .expect("report first")
            .completed_bytes,
        10
    );
    let mut ack = runtime
        .transfer("send-live")
        .expect("find outgoing transfer")
        .progress;
    ack.completed_bytes = 10;
    ack.completed_items = 1;
    ack.status = TransferStatus::Uploading;
    assert!(!runtime.apply_peer_progress("other-peer", ack.clone(), Some("first")));
    assert!(runtime.apply_peer_progress("peer-target", ack, Some("first")));
    let record = runtime
        .transfer("send-live")
        .expect("find outgoing transfer");
    assert_eq!(record.committed_bytes, 10);
    assert!(record.upload_in_flight.is_none());
    assert!(record.received_items.contains("first"));
    assert!(runtime
        .report_transfer_progress("send-live", "first", 5)
        .is_none());
    assert_eq!(
        runtime
            .report_transfer_progress("send-live", "second", 5)
            .expect("report second")
            .completed_bytes,
        15
    );
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn transfer_history_keeps_latest_ten_terminal_records() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    for index in 0..12 {
        complete_incoming(&runtime, &format!("history-{index}"), "peer-a");
    }
    let history = runtime.transfer_history_snapshot(None);
    assert_eq!(history.len(), TRANSFER_HISTORY_LIMIT);
    assert_eq!(history[0].transfer_id, "history-11");
    assert_eq!(history[9].transfer_id, "history-2");
    assert!(history
        .iter()
        .all(|entry| entry.status == TransferStatus::Completed));
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn transfer_history_survives_restart_and_ignores_corruption() {
    let path = temp_data_dir();
    {
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        complete_incoming(&runtime, "persisted", "peer-a");
    }
    {
        let restored = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("restore runtime");
        let history = restored.transfer_history_snapshot(None);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].transfer_id, "persisted");
    }
    std::fs::write(path.join(TRANSFER_HISTORY_FILE), b"{broken").expect("write corrupt history");
    let recovered = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("start with corrupt history");
    assert!(recovered.transfer_history_snapshot(None).is_empty());
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn transfer_history_captures_direction_peer_and_paths() {
    let path = temp_data_dir();
    let destination = path.join("received");
    std::fs::create_dir_all(&destination).expect("create destination");
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    runtime.set_peer(named_peer(
        "peer-target",
        "目标电脑",
        vec![TRANSFER_HISTORY_CAPABILITY.to_string()],
    ));
    runtime.add_transfer(incoming_history_offer(
        &runtime,
        "receive-one",
        "peer-sender",
    ));
    runtime
        .set_transfer_acceptance("receive-one", destination.clone())
        .expect("accept receive");
    runtime
        .mark_item_received("receive-one", "receive-one-item", 4, "0".repeat(64))
        .expect("complete receive");
    runtime.add_outgoing_transfer(outgoing_history_offer(&runtime, "send-one", "peer-target"));
    runtime.set_transfer_source_paths(
        "send-one",
        vec!["C:/资料".to_string(), "D:/照片".to_string()],
    );
    runtime
        .fail_transfer("send-one", "网络中断".to_string(), TransferStatus::Failed)
        .expect("fail send");
    let history = runtime.transfer_history_snapshot(None);
    let receive = history
        .iter()
        .find(|entry| entry.transfer_id == "receive-one")
        .expect("receive history");
    assert_eq!(receive.direction, "receive");
    assert_eq!(receive.peer_node_id, "peer-sender");
    assert_eq!(receive.peer_name, "peer-sender-name");
    assert_eq!(
        receive.destination,
        Some(destination.to_string_lossy().into_owned())
    );
    assert_eq!(receive.item_root.as_deref(), Some("file.bin"));
    let send = history
        .iter()
        .find(|entry| entry.transfer_id == "send-one")
        .expect("send history");
    assert_eq!(send.direction, "send");
    assert_eq!(send.peer_node_id, "peer-target");
    assert_eq!(send.peer_name, "目标电脑");
    assert_eq!(
        send.source_paths,
        vec!["C:/资料".to_string(), "D:/照片".to_string()]
    );
    assert_eq!(send.item_root, None);
    assert_eq!(send.error.as_deref(), Some("网络中断"));
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn receive_history_reveals_only_committed_items() {
    let path = temp_data_dir();
    let destination = path.join("received");
    std::fs::create_dir(&destination).expect("create destination");
    std::fs::write(destination.join("file.bin"), b"existing").expect("create unrelated file");
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    let mut offer = incoming_history_offer(&runtime, "partial", "peer-a");
    let mut second = offer.items[0].clone();
    second.item_id = "second".to_string();
    second.relative_path = "folder/file.bin".to_string();
    offer.total_bytes += second.size;
    offer.items.push(second);
    runtime.add_transfer(offer);
    runtime
        .set_transfer_acceptance("partial", destination)
        .expect("accept transfer");
    runtime
        .fail_transfer("partial", "目标已存在".to_string(), TransferStatus::Failed)
        .expect("fail before any file is committed");
    assert_eq!(runtime.transfer_history_snapshot(None)[0].item_root, None);

    runtime
        .retry_transfer("partial", TransferStatus::Accepted)
        .expect("retry in the same directory");
    runtime
        .mark_item_received("partial", "second", 4, "0".repeat(64))
        .expect("commit the second item");
    runtime
        .fail_transfer("partial", "目标已存在".to_string(), TransferStatus::Failed)
        .expect("fail the first item");
    let history = runtime.transfer_history_snapshot(None);
    assert_eq!(history[0].item_root.as_deref(), Some("folder"));

    let new_destination = path.join("another-receive-directory");
    std::fs::create_dir_all(new_destination.join("folder")).expect("create unrelated folder");
    runtime
        .retry_transfer("partial", TransferStatus::Waiting)
        .expect("retry after losing access to the first directory");
    runtime
        .set_transfer_acceptance("partial", new_destination)
        .expect("choose another receive directory");
    runtime
        .fail_transfer("partial", "网络中断".to_string(), TransferStatus::Failed)
        .expect("fail before committing into the new directory");
    assert_eq!(runtime.transfer_history_snapshot(None)[0].item_root, None);
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn retry_removes_failed_history_and_dismiss_keeps_it() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    runtime.add_transfer(incoming_history_offer(&runtime, "retry-one", "peer-a"));
    runtime
        .fail_transfer("retry-one", "失败".to_string(), TransferStatus::Failed)
        .expect("fail transfer");
    assert_eq!(runtime.transfer_history_snapshot(None).len(), 1);
    runtime
        .retry_transfer("retry-one", TransferStatus::Waiting)
        .expect("retry transfer");
    assert!(runtime.transfer_history_snapshot(None).is_empty());
    runtime
        .fail_transfer("retry-one", "再次失败".to_string(), TransferStatus::Failed)
        .expect("fail transfer again");
    let history = runtime.transfer_history_snapshot(None);
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].retry_count, 1);
    runtime
        .remove_failed_transfer("retry-one")
        .expect("remove failed transfer");
    assert_eq!(runtime.transfer_history_snapshot(None).len(), 1);
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn peer_reopened_transfer_clears_local_failure_history() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    runtime.add_outgoing_transfer(outgoing_history_offer(&runtime, "peer-retry", "peer-b"));
    runtime
        .fail_transfer("peer-retry", "失败".to_string(), TransferStatus::Failed)
        .expect("fail transfer");
    assert_eq!(runtime.transfer_history_snapshot(None).len(), 1);
    let mut progress = runtime
        .transfer("peer-retry")
        .expect("transfer record")
        .progress;
    progress.status = TransferStatus::Waiting;
    progress.error = None;
    progress.retry_count = 1;
    let mut stale = progress.clone();
    stale.retry_count = 0;
    assert!(!runtime.apply_peer_progress("peer-b", stale, None));
    assert!(runtime.apply_peer_progress("peer-b", progress, None));
    assert!(runtime
        .fail_transfer_at_generation(
            "peer-retry",
            "晚到的失败".into(),
            TransferStatus::Failed,
            Some(0)
        )
        .is_none());
    assert_eq!(
        runtime.transfer("peer-retry").unwrap().progress.status,
        TransferStatus::Waiting
    );
    assert!(runtime.transfer_history_snapshot(None).is_empty());
    let _ = std::fs::remove_dir_all(path);
}
#[test]
fn clear_transfer_history_persists_paired_deletions() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    complete_incoming(&runtime, "from-a", "peer-a");
    complete_incoming(&runtime, "from-b", "peer-b");
    let cleared = runtime.clear_transfer_history(None).expect("clear history");
    assert_eq!(cleared, 2);
    assert!(runtime.transfer_history_snapshot(None).is_empty());
    let pending = runtime.transfer_history.lock().pending_deletions.clone();
    assert_eq!(pending.len(), 2);
    assert_eq!(pending["peer-a"], ["from-a"]);
    let restored = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("restore runtime");
    assert!(restored.transfer_history_snapshot(None).is_empty());
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn mobile_transfer_history_is_scoped_to_session() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    let first = runtime
        .create_mobile_session("手机 A".to_string())
        .expect("create first session");
    let second = runtime
        .create_mobile_session("手机 B".to_string())
        .expect("create second session");
    for (session_id, transfer_id) in [
        (&first.session_id, "mobile-a"),
        (&second.session_id, "mobile-b"),
    ] {
        let offer = mobile_offer(&runtime, transfer_id, 4);
        runtime
            .create_mobile_transfer(session_id, offer)
            .expect("create mobile transfer");
        runtime
            .set_mobile_transfer_status(transfer_id, session_id, TransferStatus::Accepted)
            .expect("accept mobile transfer");
        runtime
            .stage_mobile_item(
                transfer_id,
                0,
                &format!("{transfer_id}-item"),
                4,
                "hash".to_string(),
            )
            .expect("stage mobile item");
        runtime
            .mark_mobile_item_downloaded(transfer_id, &format!("{transfer_id}-item"), false)
            .expect("complete mobile download");
    }
    let all = runtime.transfer_history_snapshot(None);
    assert_eq!(all.len(), 2);
    assert!(all.iter().all(|entry| entry.session_id.is_some()));
    let scoped = runtime.transfer_history_snapshot(Some(&first.session_id));
    assert_eq!(scoped.len(), 1);
    assert_eq!(scoped[0].peer_name, "手机 A");
    let cleared = runtime
        .clear_transfer_history(Some(&first.session_id))
        .expect("clear session history");
    assert_eq!(cleared, 1);
    assert!(runtime.transfer_history.lock().pending_deletions.is_empty());
    assert_eq!(runtime.transfer_history_snapshot(None).len(), 1);
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn history_deletions_queue_for_offline_peer_and_flush_on_reconnect() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    complete_incoming(&runtime, "offline-one", "peer-offline");
    runtime.clear_transfer_history(None).expect("clear history");
    assert_eq!(
        runtime
            .transfer_history
            .lock()
            .pending_deletions
            .get("peer-offline")
            .cloned(),
        Some(vec!["offline-one".to_string()])
    );
    let restored = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("restore runtime");
    assert_eq!(
        restored
            .transfer_history
            .lock()
            .pending_deletions
            .get("peer-offline")
            .cloned(),
        Some(vec!["offline-one".to_string()])
    );
    let (tx, mut rx) = peer_channel(8);
    restored.set_peer(named_peer(
        "peer-offline",
        "离线电脑",
        vec![TRANSFER_HISTORY_CAPABILITY.to_string()],
    ));
    restored.add_peer_channel("peer-offline".to_string(), tx);
    assert!(restored.flush_history_deletions("peer-offline"));
    match rx.try_recv().expect("peer receives history deletion") {
        WireMessage::TransferHistoryDeleted { transfer_ids } => {
            assert_eq!(transfer_ids, vec!["offline-one".to_string()]);
        }
        _ => panic!("unexpected message"),
    }
    assert!(restored
        .transfer_history
        .lock()
        .pending_deletions
        .is_empty());
    restored.set_peer(named_peer("no-history", "设备", Vec::new()));
    complete_incoming(&restored, "unsupported-one", "no-history");
    restored
        .clear_transfer_history(None)
        .expect("clear history");
    assert!(restored
        .transfer_history
        .lock()
        .pending_deletions
        .is_empty());
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn history_deletion_from_peer_only_removes_paired_entries() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    complete_incoming(&runtime, "from-a", "peer-a");
    complete_incoming(&runtime, "from-b", "peer-b");
    let session = runtime
        .create_mobile_session("网页".to_string())
        .expect("create session");
    let offer = mobile_offer(&runtime, "mobile-one", 4);
    runtime
        .create_mobile_transfer(&session.session_id, offer)
        .expect("create mobile transfer");
    runtime
        .set_mobile_transfer_status("mobile-one", &session.session_id, TransferStatus::Accepted)
        .expect("accept mobile transfer");
    runtime
        .stage_mobile_item("mobile-one", 0, "mobile-one-item", 4, "hash".to_string())
        .expect("stage mobile item");
    runtime
        .mark_mobile_item_downloaded("mobile-one", "mobile-one-item", false)
        .expect("complete mobile download");
    assert_eq!(
        runtime.forget_transfer_history_from_peer("peer-a", &["from-a".to_string()]),
        1
    );
    let remaining = runtime.transfer_history_snapshot(None);
    assert_eq!(remaining.len(), 2);
    assert!(remaining.iter().any(|entry| entry.transfer_id == "from-b"));
    assert!(remaining
        .iter()
        .any(|entry| entry.transfer_id == "mobile-one"));
    assert_eq!(
        runtime.forget_transfer_history_from_peer(&session.session_id, &["mobile-one".to_string()]),
        0
    );
    assert!(!valid_history_delete_ids(&[]));
    assert!(!valid_history_delete_ids(&[" ".to_string()]));
    assert!(valid_history_delete_ids(&["ok".to_string()]));
    let too_many = vec!["x".to_string(); MAX_HISTORY_DELETE_BATCH + 1];
    assert!(!valid_history_delete_ids(&too_many));
    let _ = std::fs::remove_dir_all(path);
}
#[test]
fn duplicate_transfer_offer_does_not_reset_progress() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    let offer = mobile_offer(&runtime, "duplicate", 1);
    runtime.add_transfer(offer.clone());
    runtime
        .mark_transfer_accepted("duplicate", 0)
        .expect("accept transfer");
    runtime.add_transfer(offer);
    assert_eq!(
        runtime
            .transfer("duplicate")
            .expect("find transfer")
            .progress
            .status,
        TransferStatus::Accepted
    );
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn active_transfer_records_have_a_bounded_count() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    for index in 0..MAX_TRANSFER_RECORDS {
        runtime.add_transfer(mobile_offer(&runtime, &format!("transfer-{index}"), 1));
    }
    runtime.add_transfer(mobile_offer(&runtime, "overflow", 1));
    assert_eq!(runtime.transfers.read().len(), MAX_TRANSFER_RECORDS);
    assert!(runtime.transfer("overflow").is_none());
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn disconnect_removes_peer_and_channel_state() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    runtime.set_peer(test_peer("peer", Vec::new()));
    let (tx, _rx) = peer_channel(1);
    runtime.add_peer_channel("peer".to_string(), tx);
    runtime.remove_peer("peer");
    assert!(runtime.peers_snapshot().is_empty());
    assert!(!runtime.send_to_peer("peer", WireMessage::Ping { at_ms: 1 }));
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn duplicate_discovery_keeps_one_dialer_and_updates_its_candidates() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    let mdns = PeerEndpoint {
        source: PeerEndpointSource::Mdns,
        url: "ws://192.168.1.20:37880/api/v1/ws".to_string(),
    };
    let candidates = runtime
        .offer_peer_endpoint("peer", mdns.clone())
        .expect("dialer");
    assert!(runtime.offer_peer_endpoint("peer", mdns.clone()).is_none());
    assert!(!candidates.has_changed().expect("dialer still registered"));
    let manual = PeerEndpoint {
        source: PeerEndpointSource::Manual,
        url: "ws://100.64.0.20:37880/api/v1/ws".to_string(),
    };
    assert!(runtime
        .offer_peer_endpoint("peer", manual.clone())
        .is_none());
    assert_eq!(&*candidates.borrow(), &[mdns, manual]);
    assert!(!runtime.retire_peer_dialer("peer"));
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn competing_peer_session_preserves_the_active_endpoint() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    let mut first = test_peer("peer", Vec::new());
    first.endpoint = Some("ws://100.64.0.20:37880/api/v1/ws".to_string());
    let (first_tx, _first_rx) = peer_channel(1);
    let session = runtime
        .attach_peer_session(first.clone(), first_tx)
        .expect("first session");
    let (second_tx, _second_rx) = peer_channel(1);
    assert!(runtime
        .attach_peer_session(test_peer("peer", Vec::new()), second_tx)
        .is_none());
    assert_eq!(
        runtime.peer("peer").expect("active peer").endpoint,
        first.endpoint
    );
    drop(session);
    assert!(!runtime.has_peer_channel("peer"));
    assert!(runtime.peer("peer").is_none());
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn peer_metrics_become_stale_after_timeout() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    let mut peer = test_peer("peer", vec![METRICS_SYSTEM_CAPABILITY.to_string()]);
    peer.metrics = Some(HardwareMetrics {
        node_id: "peer".to_string(),
        sampled_at_ms: 1,
        cpu_usage_percent: Some(1.0),
        memory_used_bytes: None,
        memory_total_bytes: None,
        network_down_bytes_per_sec: None,
        network_up_bytes_per_sec: None,
        gpus: Vec::new(),
        ..Default::default()
    });
    peer.metrics_received_at_ms = Some(crate::clock::now_millis() - 6_000);
    runtime.set_peer(peer);
    assert!(runtime.peers_snapshot()[0].metrics_stale);
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn mobile_transfer_requires_acceptance_and_becomes_ready_after_staging() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    let session = runtime
        .create_mobile_session("test phone".to_string())
        .expect("create mobile session");
    let offer = TransferOffer {
        transfer_id: "mobile-transfer".to_string(),
        source_node_id: runtime.node_id(),
        source_name: "desktop".to_string(),
        target_node_id: runtime.node_id(),
        items: vec![TransferItem {
            item_id: "item".to_string(),
            name: "hello.txt".to_string(),
            relative_path: "hello.txt".to_string(),
            size: 3,
            sha256: String::new(),
            media_type: "text/plain".to_string(),
            directory: false,
        }],
        total_bytes: 3,
        created_at_ms: crate::clock::now_millis(),
    };
    let waiting = runtime
        .create_mobile_transfer(&session.session_id, offer)
        .expect("create mobile transfer");
    assert_eq!(waiting.progress.status, TransferStatus::Waiting);
    runtime
        .set_mobile_transfer_status(
            "mobile-transfer",
            &session.session_id,
            TransferStatus::Accepted,
        )
        .expect("accept mobile transfer");
    runtime
        .stage_mobile_item("mobile-transfer", 0, "item", 3, "hash".to_string())
        .expect("stage item");
    let ready = runtime.mobile_transfer_view("mobile-transfer").unwrap();
    assert_eq!(ready.progress.status, TransferStatus::Ready);
    let (unchanged, cleanup) = runtime
        .set_mobile_transfer_status(
            "mobile-transfer",
            &session.session_id,
            TransferStatus::Rejected,
        )
        .expect("a repeated decision returns current state");
    assert_eq!(unchanged.progress.status, TransferStatus::Ready);
    assert!(
        cleanup.is_none(),
        "a late rejection must preserve the download"
    );
    let manifest = ready.progress.offer.as_ref().expect("staged manifest");
    assert_eq!(manifest.items[0].sha256, "hash");
    let completed = runtime
        .mark_mobile_item_downloaded("mobile-transfer", "item", false)
        .expect("mark downloaded");
    assert_eq!(completed.progress.status, TransferStatus::Completed);
    assert!(runtime
        .mobile_item_path("mobile-transfer", "item")
        .is_some());
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn mobile_staging_updates_each_manifest_slot_in_place() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, "test".into()).unwrap();
    let session = runtime.create_mobile_session("phone".into()).unwrap();
    let mut offer = mobile_offer(&runtime, "many-small-files", 1);
    let template = offer.items.pop().unwrap();
    let count = super::OUTGOING_PREPARE_MAX_ITEMS;
    offer.items = (0..count)
        .map(|i| TransferItem {
            item_id: i.to_string(),
            relative_path: format!("{i}.bin"),
            ..template.clone()
        })
        .collect();
    offer.total_bytes = count as u64;
    let id = offer.transfer_id.clone();
    runtime
        .create_mobile_transfer(&session.session_id, offer)
        .unwrap();
    runtime
        .set_mobile_transfer_status(&id, &session.session_id, TransferStatus::Accepted)
        .unwrap();
    assert!(runtime
        .stage_mobile_item(&id, 1, "0", 1, "bad".into())
        .is_none());
    for i in (0..count).rev() {
        let item_id = i.to_string();
        runtime.begin_mobile_upload(&id).unwrap();
        runtime
            .stage_mobile_item(&id, i, &item_id, 1, format!("digest-{i}"))
            .unwrap();
    }
    let progress = runtime.mobile_transfer_view(&id).unwrap().progress;
    assert_eq!(progress.status, TransferStatus::Ready);
    assert_eq!(progress.completed_items, count);
    assert_eq!(progress.completed_bytes, count as u64);
    for (i, item) in progress.offer.unwrap().items.iter().enumerate() {
        assert_eq!(item.sha256, format!("digest-{i}"));
    }
    std::fs::remove_dir_all(path).unwrap();
}

#[test]
fn mobile_transfer_staging_directory_is_internal_and_cleanup_is_bounded() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    let session = runtime
        .create_mobile_session("test phone".to_string())
        .expect("create mobile session");
    let transfer_id = "../outside";
    runtime
        .create_mobile_transfer(&session.session_id, mobile_offer(&runtime, transfer_id, 1))
        .expect("create mobile transfer");
    let record = runtime
        .mobile_transfer(transfer_id)
        .expect("find mobile transfer");
    runtime
        .create_mobile_transfer(&session.session_id, mobile_offer(&runtime, "", 1))
        .expect("create empty-id mobile transfer");
    let empty_id_record = runtime
        .mobile_transfer("")
        .expect("find empty-id mobile transfer");
    let staging_root = path.join("mobile-transfers");
    assert_eq!(
        record.staged_directory.parent(),
        Some(staging_root.as_path())
    );
    assert_eq!(
        empty_id_record.staged_directory.parent(),
        Some(staging_root.as_path())
    );
    assert!(record.staged_directory != staging_root);
    assert!(empty_id_record.staged_directory != staging_root);
    assert!(record
        .staged_directory
        .file_name()
        .is_some_and(|name| { Uuid::parse_str(&name.to_string_lossy()).is_ok() }));

    let outside = path.join("outside");
    std::fs::create_dir_all(&outside).expect("create outside directory");
    std::fs::write(outside.join("keep.txt"), b"keep").expect("write outside marker");
    runtime.mobile_sessions.write().clear();
    runtime
        .mobile_transfers
        .write()
        .get_mut(transfer_id)
        .expect("find transfer record")
        .staged_directory = staging_root.join("..").join("outside");
    let directories = runtime.prune_mobile_transfers();
    assert!(directories.iter().all(|directory| directory != &outside));
    assert!(outside.join("keep.txt").exists());
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn startup_removes_stale_mobile_staging_directory() {
    let path = temp_data_dir();
    let staging = path.join("mobile-transfers");
    std::fs::create_dir_all(&staging).expect("create stale staging directory");
    std::fs::write(staging.join("stale.part"), b"stale").expect("write stale file");
    let _runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    assert!(!staging.exists());
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn startup_removes_registered_desktop_temp_files() {
    let path = temp_data_dir();
    let received = path.join("received");
    std::fs::create_dir_all(&received).expect("create received directory");
    let temp = received.join("payload.meshshare-test.part");
    std::fs::write(&temp, b"stale").expect("write desktop temp file");
    {
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
            .expect("create runtime");
        runtime
            .register_desktop_temp_file(&temp)
            .expect("register desktop temp file");
    }
    let _restored = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("restore runtime");
    assert!(!temp.exists());
    let registry = std::fs::read_to_string(path.join(DESKTOP_TEMP_REGISTRY_FILE))
        .expect("read desktop temp registry");
    assert_eq!(registry.trim(), "[]");
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn mobile_sessions_have_a_bounded_active_count() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    for index in 0..MAX_MOBILE_SESSIONS {
        runtime
            .create_mobile_session(format!("phone-{index}"))
            .expect("create mobile session");
    }
    assert!(runtime
        .create_mobile_session("overflow".to_string())
        .is_err());
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn mobile_transfer_records_have_a_bounded_resource_count() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    let session = runtime
        .create_mobile_session("test phone".to_string())
        .expect("create mobile session");
    for index in 0..MAX_MOBILE_TRANSFER_RECORDS {
        runtime
            .create_mobile_transfer(
                &session.session_id,
                mobile_offer(&runtime, &format!("mobile-record-{index}"), 1),
            )
            .expect("create mobile transfer record");
    }
    let error = runtime
        .create_mobile_transfer(
            &session.session_id,
            mobile_offer(&runtime, "mobile-record-overflow", 1),
        )
        .expect_err("resource guard must bound mobile records");
    assert!(error.contains("资源"));
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn stalled_mobile_transfer_fails_and_allows_another_large_transfer() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    let session = runtime
        .create_mobile_session("test phone".to_string())
        .expect("create mobile session");
    runtime
        .create_mobile_transfer(
            &session.session_id,
            mobile_offer(&runtime, "stalled", u64::MAX - 1),
        )
        .expect("create mobile transfer");
    runtime
        .set_mobile_transfer_status("stalled", &session.session_id, TransferStatus::Accepted)
        .expect("accept mobile transfer");
    let record = runtime.mobile_transfer("stalled").expect("find transfer");
    std::fs::create_dir_all(&record.staged_directory).expect("create staging directory");
    runtime
        .mobile_transfers
        .write()
        .get_mut("stalled")
        .expect("find mutable transfer")
        .updated_at_ms = crate::clock::now_millis() - TRANSFER_ACTIVITY_TIMEOUT_MS - 1;

    let cleanup = runtime.prune_mobile_transfers();
    assert!(cleanup.contains(&record.staged_directory));
    assert_eq!(
        runtime
            .mobile_transfer("stalled")
            .expect("find failed transfer")
            .progress
            .status,
        TransferStatus::Failed
    );
    runtime
        .create_mobile_transfer(
            &session.session_id,
            mobile_offer(&runtime, "after-stalled", 1),
        )
        .expect("another transfer is accepted");
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn recently_completed_mobile_transfer_uses_completion_time_for_retention() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    let session = runtime
        .create_mobile_session("test phone".to_string())
        .expect("create mobile session");
    let mut offer = mobile_offer(&runtime, "recent-mobile", 1);
    offer.created_at_ms = crate::clock::now_millis() - 11 * 60 * 1000;
    runtime
        .create_mobile_transfer(&session.session_id, offer)
        .expect("create mobile transfer");
    runtime
        .set_mobile_transfer_status(
            "recent-mobile",
            &session.session_id,
            TransferStatus::Accepted,
        )
        .expect("accept mobile transfer");
    runtime
        .stage_mobile_item(
            "recent-mobile",
            0,
            "recent-mobile-item",
            1,
            "hash".to_string(),
        )
        .expect("stage mobile item");
    runtime
        .mark_mobile_item_downloaded("recent-mobile", "recent-mobile-item", false)
        .expect("download mobile item");

    runtime.prune_mobile_transfers();
    assert!(runtime.mobile_transfer("recent-mobile").is_some());
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn old_terminal_transfers_are_pruned() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    let mut offer = mobile_offer(&runtime, "old-terminal", 1);
    offer.created_at_ms = crate::clock::now_millis() - 11 * 60 * 1000;
    runtime.add_outgoing_transfer(offer);
    runtime
        .fail_transfer(
            "old-terminal",
            "done".to_string(),
            TransferStatus::Cancelled,
        )
        .expect("cancel transfer");
    runtime
        .transfers
        .write()
        .get_mut("old-terminal")
        .expect("find terminal transfer")
        .updated_at_ms = crate::clock::now_millis() - 11 * 60 * 1000;
    runtime.prune_transfers();
    assert!(runtime.transfer("old-terminal").is_none());
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn failed_transfer_removal_drops_record_and_keeps_active() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    runtime.add_outgoing_transfer(mobile_offer(&runtime, "removable", 1));
    runtime
        .fail_transfer("removable", "failed".to_string(), TransferStatus::Failed)
        .expect("fail transfer");
    runtime.add_outgoing_transfer(mobile_offer(&runtime, "active", 1));

    assert!(runtime.remove_failed_transfer("active").is_none());
    assert!(runtime.transfer("active").is_some());

    let removed = runtime
        .remove_failed_transfer("removable")
        .expect("remove failed transfer");
    assert_eq!(removed.status, TransferStatus::Failed);
    assert!(runtime.transfer("removable").is_none());
    assert!(runtime
        .transfers_snapshot()
        .iter()
        .all(|progress| progress.transfer_id != "removable"));
    assert!(runtime.remove_failed_transfer("removable").is_none());
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn failed_mobile_transfer_removal_releases_manifest_bytes() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    let session = runtime
        .create_mobile_session("test phone".to_string())
        .expect("create mobile session");
    let offer = mobile_offer(&runtime, "mobile-removable", 1);
    let manifest_bytes = serde_json::to_vec(&offer).unwrap().len() as u64;
    runtime
        .create_mobile_transfer(&session.session_id, offer)
        .expect("create mobile transfer");
    assert_eq!(
        runtime
            .mobile_manifest_bytes
            .load(std::sync::atomic::Ordering::SeqCst),
        manifest_bytes
    );
    assert!(runtime
        .remove_failed_mobile_transfer("mobile-removable")
        .is_none());
    runtime
        .fail_mobile_transfer("mobile-removable", "failed".to_string())
        .expect("fail mobile transfer");

    let removed = runtime
        .remove_failed_mobile_transfer("mobile-removable")
        .expect("remove failed mobile transfer");
    assert_eq!(removed.progress.status, TransferStatus::Failed);
    assert!(runtime.mobile_transfer("mobile-removable").is_none());
    assert_eq!(
        runtime
            .mobile_manifest_bytes
            .load(std::sync::atomic::Ordering::SeqCst),
        0
    );
    runtime
        .create_mobile_transfer(
            &session.session_id,
            mobile_offer(&runtime, "mobile-after-removal", 1),
        )
        .expect("create mobile transfer after removal");
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn failed_transfer_retry_preserves_progress_and_increments_generation() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    runtime.add_outgoing_transfer(mobile_offer(&runtime, "retryable", 8));
    {
        let mut transfers = runtime.transfers.write();
        let record = transfers.get_mut("retryable").expect("find transfer");
        record.progress.status = TransferStatus::Failed;
        record.progress.completed_bytes = 4;
        record.progress.completed_items = 1;
        record.progress.error = Some("temporary".to_string());
    }
    let progress = runtime
        .retry_transfer("retryable", TransferStatus::Waiting)
        .expect("retry failed transfer");
    assert!(runtime.mark_transfer_accepted("retryable", 0).is_none());
    assert_eq!(
        runtime.transfer("retryable").unwrap().progress.status,
        TransferStatus::Waiting
    );
    let progress = runtime
        .mark_transfer_accepted("retryable", progress.retry_count)
        .unwrap();
    assert_eq!(progress.status, TransferStatus::Accepted);
    assert_eq!(progress.retry_count, 1);
    assert_eq!(progress.completed_bytes, 4);
    assert_eq!(progress.completed_items, 1);
    assert_eq!(progress.error, None);
    assert!(runtime
        .retry_transfer("retryable", TransferStatus::Accepted)
        .is_err());
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn inactive_transfer_is_failed_by_lifecycle_cleanup() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    runtime.add_outgoing_transfer(mobile_offer(&runtime, "inactive", 1));
    runtime
        .transfers
        .write()
        .get_mut("inactive")
        .expect("find transfer")
        .updated_at_ms = crate::clock::now_millis() - 6 * 60 * 1000;
    runtime.prune_transfers();
    assert_eq!(
        runtime
            .transfer("inactive")
            .expect("find failed transfer")
            .progress
            .status,
        TransferStatus::Failed
    );
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn clipboard_queue_overflow_schedules_latest_snapshot_resync() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    let event = runtime
        .create_local_clipboard_named(Some("latest".to_string()), None, "local".to_string())
        .expect("create clipboard event");
    let (tx, mut rx) = peer_channel(1);
    runtime.set_peer(test_peer("peer", vec!["clipboard.text".to_string()]));
    runtime.add_peer_channel("peer".to_string(), tx);
    assert!(runtime.send_to_peer("peer", WireMessage::Ping { at_ms: 1 }));
    assert!(!runtime.send_to_peer(
        "peer",
        WireMessage::ClipboardUpdate {
            event: event.clone().into(),
        }
    ));
    let _ = rx.try_recv().expect("drain full queue");
    assert!(runtime.flush_clipboard_resync("peer"));
    match rx.try_recv().expect("receive snapshot resync") {
        WireMessage::ClipboardSnapshot { snapshot } => {
            assert_eq!(snapshot.text.as_deref(), Some("latest"))
        }
        _ => panic!("expected clipboard snapshot"),
    }
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn clipboard_queue_bounds_payload_bytes_and_recovers_the_latest_snapshot() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into()).unwrap();
    let latest = runtime
        .create_local_clipboard_named(Some("latest".to_string()), None, "local".to_string())
        .unwrap();
    let (tx, mut rx) = peer_channel(8);
    runtime.set_peer(test_peer("peer", vec!["clipboard.text".to_string()]));
    runtime.add_peer_channel("peer".to_string(), tx);
    let mut large: ClipboardEvent = latest.clone().into();
    large.text = Some("x".repeat(CLIPBOARD_TEXT_LIMIT).into());
    let update = WireMessage::ClipboardUpdate { event: large };
    assert!(runtime.send_to_peer("peer", update.clone()));
    assert!(runtime.send_to_peer("peer", update.clone()));
    assert!(!runtime.send_to_peer("peer", update));
    assert!(!runtime.flush_clipboard_resync("peer"));
    assert!(runtime.send_to_peer("peer", WireMessage::Ping { at_ms: 1 }));
    assert!(matches!(
        rx.try_recv(),
        Ok(WireMessage::ClipboardUpdate { .. })
    ));
    assert!(runtime.flush_clipboard_resync("peer"));
    assert!(matches!(
        rx.try_recv(),
        Ok(WireMessage::ClipboardUpdate { .. })
    ));
    assert!(matches!(rx.try_recv(), Ok(WireMessage::Ping { at_ms: 1 })));
    assert!(matches!(
        rx.try_recv(),
        Ok(WireMessage::ClipboardSnapshot { snapshot }) if snapshot.event_id == latest.event_id
    ));
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn clipboard_messages_skip_peers_without_clipboard_capability() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    let event = runtime
        .create_local_clipboard_named(Some("manual".to_string()), None, "local".to_string())
        .expect("create clipboard event");
    let (tx, mut rx) = peer_channel(8);
    runtime.set_peer(test_peer(
        "android",
        vec![
            "transfer.file".to_string(),
            METRICS_NETWORK_CAPABILITY.to_string(),
        ],
    ));
    runtime.add_peer_channel("android".to_string(), tx);

    assert!(runtime.send_to_peer(
        "android",
        WireMessage::ClipboardUpdate {
            event: event.clone().into()
        },
    ));
    runtime.send_to_peers_except(
        None,
        WireMessage::ClipboardUpdate {
            event: event.into(),
        },
    );
    assert!(rx.try_recv().is_err());

    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn clipboard_messages_reach_view_only_peers() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    let event = runtime
        .create_local_clipboard_named(Some("view".to_string()), None, "local".to_string())
        .expect("create clipboard event");
    let (tx, mut rx) = peer_channel(8);
    runtime.set_peer(test_peer(
        "android",
        vec![CLIPBOARD_VIEW_CAPABILITY.to_string()],
    ));
    runtime.add_peer_channel("android".to_string(), tx);

    assert!(runtime.send_to_peer(
        "android",
        WireMessage::ClipboardUpdate {
            event: event.into()
        },
    ));
    assert!(matches!(
        rx.try_recv(),
        Ok(WireMessage::ClipboardUpdate { .. })
    ));

    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn concurrent_clipboard_updates_keep_the_newest_hlc() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    let mut updates = runtime.subscribe_clipboard();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let older = ClipboardEvent {
        event_id: "older".to_string(),
        origin_node_id: "peer-a".to_string(),
        clock: Hlc {
            physical_ms: 1,
            logical: 0,
            node_id: "peer-a".to_string(),
        },
        text: Some("older".into()),
        image_png: None,
        updated_at_ms: 1,
    };
    let newer = ClipboardEvent {
        event_id: "newer".to_string(),
        origin_node_id: "peer-b".to_string(),
        clock: Hlc {
            physical_ms: 2,
            logical: 0,
            node_id: "peer-b".to_string(),
        },
        text: Some("newer".into()),
        image_png: None,
        updated_at_ms: 2,
    };
    let older_runtime = runtime.clone();
    let older_barrier = barrier.clone();
    let older_thread = std::thread::spawn(move || {
        older_barrier.wait();
        older_runtime.receive_clipboard("peer-a", older.into_snapshot("older".into()));
    });
    let newer_runtime = runtime.clone();
    let newer_barrier = barrier.clone();
    let newer_thread = std::thread::spawn(move || {
        newer_barrier.wait();
        newer_runtime.receive_clipboard("peer-b", newer.into_snapshot("newer".into()));
    });
    barrier.wait();
    older_thread.join().expect("older update thread");
    newer_thread.join().expect("newer update thread");
    assert_eq!(
        runtime
            .clipboard_snapshot()
            .expect("clipboard snapshot")
            .text
            .as_deref(),
        Some("newer")
    );
    assert!(updates.has_changed().unwrap());
    assert_eq!(
        updates.borrow_and_update().as_ref().unwrap().event_id,
        "newer"
    );
    assert!(!updates.has_changed().unwrap());
    let _ = std::fs::remove_dir_all(path);
}

#[test]
#[cfg(not(target_os = "android"))]
fn system_clipboard_keeps_latest_pending_snapshot_and_discards_stale_reads() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    let observed_event_id = runtime.clipboard_event_id();

    // 系统线程还在读取时，节点仍能接受后续剪贴板事件。
    runtime
        .create_local_clipboard_named(Some("first".to_string()), None, "web".to_string())
        .expect("accept first publication");
    let newest = runtime
        .create_local_clipboard_named(Some("newest".to_string()), None, "web".to_string())
        .expect("accept latest publication");
    assert!(runtime
        .create_local_clipboard(
            Some("stale read".to_string()),
            None,
            observed_event_id.as_deref()
        )
        .is_none());
    let pending = runtime
        .take_system_clipboard_write()
        .expect("pending write");
    assert_eq!(pending.event_id, newest.event_id);
    assert!(runtime.take_system_clipboard_write().is_none());

    let copied = runtime
        .create_local_clipboard(Some("fresh copy".to_string()), None, Some(&newest.event_id))
        .expect("publish fresh system clipboard read");
    assert_eq!(
        runtime.clipboard_event_id().as_deref(),
        Some(copied.event_id.as_str())
    );
    assert!(runtime.take_system_clipboard_write().is_none());
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn local_clipboard_accepts_png_payloads() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    let mut png = Vec::new();
    image::DynamicImage::new_rgba8(1, 1)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .expect("encode png");
    let event = runtime
        .create_local_clipboard_named(None, Some(png), "local".to_string())
        .expect("create image clipboard event");
    assert!(event.image_png.is_some());
    assert!(runtime
        .clipboard_snapshot()
        .expect("clipboard snapshot")
        .image_png
        .is_some());
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn persisted_current_snapshot_advances_clock_after_restart() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    let future_clock = Hlc {
        physical_ms: crate::clock::now_millis() + 60_000,
        logical: 7,
        node_id: "peer".to_string(),
    };
    assert!(runtime.receive_clipboard(
        "peer",
        ClipboardSnapshot {
            source_name: "peer".into(),
            event_id: "future-event".to_string(),
            source_node_id: "peer".to_string(),
            clock: future_clock,
            text: Some("future".into()),
            image_png: None,
            updated_at_ms: crate::clock::now_millis(),
        },
    ));
    let current = runtime.clipboard_snapshot().expect("current snapshot");
    drop(runtime);

    let restored = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("restore runtime");
    let next = restored.clock.tick();
    assert!(next.compare(&current.clock).is_gt());
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn startup_removes_clipboard_history_temporary_files() {
    let path = temp_data_dir();
    let payload_dir = path.join("clipboard-history-payloads");
    std::fs::create_dir_all(&payload_dir).expect("create payload directory");
    for file in [
        path.join("clipboard-history.json.tmp-123"),
        path.join("clipboard-current.json.tmp-123"),
        payload_dir.join("payload.txt.tmp-123"),
        payload_dir.join("payload.png.tmp-123"),
    ] {
        std::fs::write(file, b"stale").expect("write stale clipboard file");
    }
    let _runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    assert!(!path.join("clipboard-history.json.tmp-123").exists());
    assert!(!path.join("clipboard-current.json.tmp-123").exists());
    assert!(!payload_dir.join("payload.txt.tmp-123").exists());
    assert!(!payload_dir.join("payload.png.tmp-123").exists());
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn replaying_current_snapshot_does_not_recreate_cleared_history() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    runtime
        .create_local_clipboard_named(Some("current".to_string()), None, "local".to_string())
        .expect("create clipboard event");
    let current = runtime.clipboard_snapshot().expect("current snapshot");
    drop(runtime);

    let restored = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("restore runtime");
    restored
        .clear_clipboard_history()
        .expect("clear clipboard history");
    assert!(!restored.receive_clipboard("peer", current.clone()));
    let mut replay = current;
    replay.clock = restored.clock.tick();
    replay.text = Some("changed under the same ID".into());
    assert!(!restored.receive_clipboard("peer", replay));
    assert_eq!(
        restored.clipboard_snapshot().unwrap().text.as_deref(),
        Some("current")
    );
    assert!(restored.clipboard_history_list().items.is_empty());
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn clipboard_history_persists_deduplicates_and_restores_current() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    runtime
        .create_local_clipboard_named(Some("one".to_string()), None, "local".to_string())
        .expect("first clipboard event");
    runtime
        .create_local_clipboard_named(Some("two".to_string()), None, "local".to_string())
        .expect("second clipboard event");
    runtime
        .create_local_clipboard_named(Some("one".to_string()), None, "local".to_string())
        .expect("duplicate clipboard event");
    let history = runtime.clipboard_history_list();
    assert_eq!(history.items.len(), 2);
    assert_eq!(history.items[0].text_preview.as_deref(), Some("one"));

    let restored = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("restore runtime");
    assert_eq!(
        restored
            .clipboard_snapshot()
            .expect("current snapshot")
            .text
            .as_deref(),
        Some("one")
    );
    let restored_history = restored.clipboard_history_list();
    assert_eq!(restored_history.items.len(), 2);
    let cleared = restored
        .clear_clipboard_history()
        .expect("clear clipboard history");
    assert_eq!(cleared, 2);
    assert!(restored.clipboard_history_list().items.is_empty());
    assert!(restored.clipboard_snapshot().is_some());
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn clipboard_preview_does_not_block_snapshots_or_cache_a_replaced_image() {
    let path = temp_data_dir();
    let executor = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    executor.block_on(async {
        let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into()).unwrap();
        let mut png = Vec::new();
        image::DynamicImage::new_rgba8(800, 400)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let image = runtime
            .create_local_clipboard_named(None, Some(png), "local".into())
            .unwrap();
        // 暂停阻塞线程池，使快照替换发生在预览取得载荷之后、编码完成之前。
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let blocker = tokio::task::spawn_blocking(move || {
            let _ = wait.recv();
        });
        let known_clock = format!(
            "{}:{}:{}",
            image.clock.physical_ms, image.clock.logical, image.clock.node_id
        );
        let known = Some((image.event_id.as_str(), known_clock.as_str()));
        let mut preview = std::pin::pin!(
            runtime.clipboard_preview_png(image.event_id.clone(), Some(&known_clock))
        );
        assert!(futures_util::poll!(&mut preview).is_pending());
        let mut duplicate = std::pin::pin!(
            runtime.clipboard_preview_png(image.event_id.clone(), Some(&known_clock))
        );
        assert!(futures_util::poll!(&mut duplicate).is_pending());
        assert!(runtime.clipboard.try_lock().is_some());
        assert!(matches!(
            runtime.read_clipboard_snapshot(known),
            ClipboardSnapshotRead::Unchanged
        ));
        let mut text = runtime
            .create_local_clipboard_named(Some("new".into()), None, "local".into())
            .unwrap();
        // 模拟事件 ID 已离开短期去重表后被复用，旧快照版本仍应失效。
        runtime.seen_events.lock().remove(&image.event_id);
        text.event_id.clone_from(&image.event_id);
        text.clock = runtime.clock.tick();
        assert!(runtime.receive_clipboard("peer", text.clone()));
        assert!(matches!(
            runtime.read_clipboard_snapshot(known),
            ClipboardSnapshotRead::Current(Some(snapshot)) if snapshot.clock == text.clock
        ));
        release.send(()).unwrap();
        blocker.await.unwrap();
        assert!(preview.await.unwrap().is_none());
        assert!(duplicate.await.unwrap().is_none());
        assert!(runtime
            .clipboard_preview_png(text.event_id, None)
            .await
            .unwrap()
            .is_none());
        assert!(runtime.clipboard.lock().preview_png.is_none());
    });
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn clipboard_history_thumbnail_is_generated_and_cleaned_up() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    let mut png = Vec::new();
    image::DynamicImage::new_rgba8(200, 100)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .expect("encode png");
    let event = runtime
        .create_local_clipboard_named(None, Some(png), "local".to_string())
        .expect("create image clipboard event");

    let thumbnail = runtime
        .clipboard_history_thumbnail(&event.event_id)
        .expect("thumbnail");
    let decoded = image::load_from_memory(&thumbnail).expect("decode thumbnail");
    assert_eq!((decoded.width(), decoded.height()), (96, 72));
    assert!(runtime
        .clipboard_history_thumbnail(&event.event_id)
        .is_some());

    let payload_dir = path.join(CLIPBOARD_PAYLOAD_DIR);
    assert!(std::fs::read_dir(&payload_dir)
        .expect("list payloads")
        .filter_map(Result::ok)
        .any(|entry| entry.file_name().to_string_lossy().ends_with(".thumb.png")));

    runtime
        .create_local_clipboard_named(Some("text".to_string()), None, "local".to_string())
        .expect("create text clipboard event");
    let text_event_id = runtime.clipboard_history_list().items[0].event_id.clone();
    assert!(runtime
        .clipboard_history_thumbnail(&text_event_id)
        .is_none());
    assert!(runtime.clipboard_history_thumbnail("missing").is_none());

    runtime.clear_clipboard_history().expect("clear history");
    assert_eq!(
        std::fs::read_dir(&payload_dir)
            .expect("list payloads after clear")
            .count(),
        0
    );
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn clipboard_history_count_setting_prunes_immediately() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    runtime
        .set_clipboard_history_settings(ClipboardHistoryMode::Count, 2)
        .expect("set history count");
    for value in ["one", "two", "three"] {
        runtime
            .create_local_clipboard_named(Some(value.to_string()), None, "local".to_string())
            .expect("create clipboard event");
    }
    let history = runtime.clipboard_history_list();
    assert_eq!(history.items.len(), 2);
    assert_eq!(history.items[0].text_preview.as_deref(), Some("three"));
    assert_eq!(history.items[1].text_preview.as_deref(), Some("two"));
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn clipboard_history_days_prune_uses_local_recorded_time() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    runtime
        .create_local_clipboard_named(Some("old".to_string()), None, "local".to_string())
        .expect("create clipboard event");
    runtime.clipboard_history.lock()[0].recorded_at_ms =
        crate::clock::now_millis() - 2 * 24 * 60 * 60 * 1000;
    runtime
        .create_local_clipboard_named(Some("new".to_string()), None, "local".to_string())
        .expect("create clipboard event");
    runtime
        .set_clipboard_history_settings(ClipboardHistoryMode::Days, 1)
        .expect("set history days");
    let history = runtime.clipboard_history_list();
    assert_eq!(history.items.len(), 1);
    assert_eq!(history.items[0].text_preview.as_deref(), Some("new"));
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn clipboard_history_write_failure_keeps_previous_record_and_payload() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    runtime
        .create_local_clipboard_named(Some("old".to_string()), None, "local".to_string())
        .expect("create old clipboard event");
    let old = runtime.clipboard_history.lock()[0].clone();
    let history_path = path.join(CLIPBOARD_HISTORY_FILE);
    std::fs::remove_file(&history_path).expect("remove history index");
    std::fs::create_dir(&history_path).expect("block history index");

    let result =
        runtime.create_local_clipboard_named(Some("new".to_string()), None, "local".to_string());
    assert!(
        result.is_some(),
        "failed history write must not block publication"
    );
    assert_eq!(
        super::load_clipboard_current(&path)
            .expect("load current snapshot")
            .text
            .as_deref(),
        Some("new")
    );
    let history = runtime.clipboard_history.lock();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].event_id, old.event_id);
    assert_eq!(history[0].text_path, old.text_path);
    drop(history);
    assert_eq!(
        std::fs::read_dir(super::clipboard_payload_dir(&path))
            .expect("list history payloads")
            .count(),
        1,
        "failed history write must discard the new payload"
    );
    std::fs::remove_dir(&history_path).expect("remove blocking directory");
    let detail = runtime
        .clipboard_history_detail(&old.event_id)
        .expect("old history detail");
    assert_eq!(detail.snapshot.text.as_deref(), Some("old"));
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn clipboard_history_prunes_oldest_records_at_byte_limit() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    for value in ["one", "two", "three"] {
        runtime
            .create_local_clipboard_named(Some(value.to_string()), None, "local".to_string())
            .expect("create clipboard event");
    }
    let config = runtime.config.lock().clone();
    let mut history = runtime.clipboard_history.lock().clone();
    prune_clipboard_history_records_with_limit(
        &mut history,
        &config,
        crate::clock::now_millis(),
        8,
    );
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].text_len, 5);
    assert_eq!(history[1].text_len, 3);
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn older_hlc_event_is_kept_in_history_without_replacing_current() {
    let path = temp_data_dir();
    let runtime = NodeRuntime::new(path.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    runtime
        .create_local_clipboard_named(Some("new".to_string()), None, "local".to_string())
        .expect("create clipboard event");
    let accepted_as_current = runtime.receive_clipboard(
        "peer",
        ClipboardSnapshot {
            source_name: "peer".into(),
            event_id: "older-event".to_string(),
            source_node_id: "peer".to_string(),
            clock: Hlc {
                physical_ms: 1,
                logical: 0,
                node_id: "peer".to_string(),
            },
            text: Some("old".into()),
            image_png: None,
            updated_at_ms: 1,
        },
    );
    assert!(!accepted_as_current);
    assert_eq!(
        runtime
            .clipboard_snapshot()
            .expect("current snapshot")
            .text
            .as_deref(),
        Some("new")
    );
    assert!(runtime
        .clipboard_history_list()
        .items
        .iter()
        .any(|item| item.text_preview.as_deref() == Some("old")));
    let _ = std::fs::remove_dir_all(path);
}

fn outgoing_test_item(name: &str, bytes: &[u8]) -> TransferItem {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    TransferItem {
        item_id: format!("item-{}", Uuid::new_v4()),
        name: name.to_string(),
        relative_path: name.to_string(),
        size: bytes.len() as u64,
        sha256: format!("{:x}", hasher.finalize()),
        media_type: "application/octet-stream".to_string(),
        directory: false,
    }
}

fn outgoing_test_offer(
    source_node_id: &str,
    target_node_id: &str,
    items: Vec<TransferItem>,
) -> TransferOffer {
    TransferOffer {
        transfer_id: Uuid::new_v4().to_string(),
        source_node_id: source_node_id.to_string(),
        source_name: "sender".to_string(),
        target_node_id: target_node_id.to_string(),
        total_bytes: items.iter().map(|item| item.size).sum(),
        items,
        created_at_ms: crate::clock::now_millis(),
    }
}

fn register_peer_endpoint(runtime: &Arc<NodeRuntime>, node: NodeInfo) {
    runtime.peers.write().insert(
        node.node_id.clone(),
        PeerInfo {
            node_id: node.node_id.clone(),
            node_name: node.node_name.clone(),
            platform: node.platform.clone(),
            addresses: node.addresses.clone(),
            mobile_addresses: node.mobile_addresses.clone(),
            capabilities: node.capabilities.clone(),
            terminal: node.terminal.clone(),
            online: true,
            last_seen_ms: crate::clock::now_millis(),
            endpoint: Some(format!("ws://127.0.0.1:{}/api/v1/ws", node.port)),
            source: "websocket".to_string(),
            metrics: None,
            metrics_received_at_ms: None,
            metrics_stale: false,
        },
    );
}

#[tokio::test]
async fn accepted_outgoing_transfer_streams_staged_items_without_page_state() {
    let receiver_dir =
        std::env::temp_dir().join(format!("meshshare-outgoing-receiver-{}", Uuid::new_v4()));
    let static_root = receiver_dir.join("web");
    std::fs::create_dir_all(&static_root).expect("create static directory");
    std::fs::write(static_root.join("index.html"), "<html></html>").expect("write static index");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind receiver");
    let address = listener.local_addr().expect("receiver address");
    let receiver = NodeRuntime::new(
        receiver_dir.clone(),
        address.port(),
        env!("CARGO_PKG_VERSION").into(),
    )
    .expect("create receiver runtime");
    let target_id = receiver.node_id();
    let app =
        crate::server::router(receiver.clone(), static_root).layer(axum::middleware::from_fn(
            move |req: axum::extract::Request, next: axum::middleware::Next| {
                if req.method() == axum::http::Method::PUT {
                    assert_eq!(
                        req.headers().get(crate::model::TARGET_HEADER).unwrap(),
                        target_id.as_str()
                    );
                }
                next.run(req)
            },
        ));
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve receiver");
    });

    let sender_dir =
        std::env::temp_dir().join(format!("meshshare-outgoing-sender-{}", Uuid::new_v4()));
    let sender = NodeRuntime::new(sender_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create sender runtime");
    register_peer_endpoint(&sender, receiver.node_info());

    let payload = b"meshshare staged payload".to_vec();
    let item = outgoing_test_item("payload.bin", &payload);
    let remaining = outgoing_test_item("remaining.bin", b"more");
    let offer = outgoing_test_offer(
        &sender.node_id(),
        &receiver.node_id(),
        vec![item.clone(), remaining.clone()],
    );
    sender
        .begin_outgoing_staging(offer.clone())
        .expect("begin staging");
    let staged = sender
        .outgoing_staged_target(&offer.transfer_id, &item.item_id)
        .expect("staged target");
    std::fs::create_dir_all(staged.parent().expect("staging directory"))
        .expect("create staging directory");
    std::fs::write(&staged, &payload).expect("write staged payload");
    sender.mark_outgoing_item_staged(&offer.transfer_id, &item.item_id, item.sha256.clone());
    sender.mark_outgoing_item_staged(
        &offer.transfer_id,
        &remaining.item_id,
        remaining.sha256.clone(),
    );

    receiver.add_transfer(offer.clone());
    let destination = receiver_dir.join("received");
    std::fs::create_dir_all(&destination).expect("create destination");
    receiver
        .set_transfer_acceptance(&offer.transfer_id, destination.clone())
        .expect("accept transfer");

    sender.add_outgoing_transfer(offer.clone());
    sender.mark_transfer_accepted(&offer.transfer_id, 0);
    assert!(sender
        .upload_outgoing_transfer(&offer.transfer_id)
        .await
        .is_err());
    assert!(!staged.exists(), "the acknowledged item has been released");
    let pending_file = sender
        .outgoing_staged_target(&offer.transfer_id, &remaining.item_id)
        .unwrap();
    std::fs::write(pending_file, b"more").unwrap();
    receiver
        .fail_transfer(
            &offer.transfer_id,
            "暂存文件不可用".into(),
            TransferStatus::Failed,
        )
        .unwrap();
    receiver
        .retry_transfer(&offer.transfer_id, TransferStatus::Accepted)
        .unwrap();
    sender
        .retry_transfer(&offer.transfer_id, TransferStatus::Accepted)
        .unwrap();
    sender
        .upload_outgoing_transfer(&offer.transfer_id)
        .await
        .expect("upload staged transfer");

    assert_eq!(
        sender
            .transfer(&offer.transfer_id)
            .expect("sender record")
            .progress
            .status,
        TransferStatus::Completed
    );
    assert_eq!(
        receiver
            .transfer(&offer.transfer_id)
            .expect("receiver record")
            .progress
            .status,
        TransferStatus::Completed
    );
    assert_eq!(
        std::fs::read(destination.join("payload.bin")).expect("read received payload"),
        payload
    );
    assert_eq!(
        std::fs::read(destination.join("remaining.bin")).unwrap(),
        b"more"
    );
    assert!(sender.outgoing_staging_counts(&offer.transfer_id).is_none());

    sender
        .stopping
        .store(true, std::sync::atomic::Ordering::SeqCst);
    server.abort();
    let _ = server.await;
    let _ = std::fs::remove_dir_all(&sender_dir);
    let _ = std::fs::remove_dir_all(&receiver_dir);
}

#[tokio::test]
async fn outgoing_staging_endpoints_verify_source_bytes() {
    let data_dir =
        std::env::temp_dir().join(format!("meshshare-outgoing-staging-{}", Uuid::new_v4()));
    let static_root = data_dir.join("web");
    std::fs::create_dir_all(&static_root).expect("create static directory");
    std::fs::write(static_root.join("index.html"), "<html></html>").expect("write static index");
    let runtime = NodeRuntime::new(data_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    let mut peer_node = runtime.node_info();
    peer_node.node_id = "staging-peer".to_string();
    peer_node.port = 9;
    register_peer_endpoint(&runtime, peer_node);

    let app = crate::server::router(runtime.clone(), static_root);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind staging server");
    let address = listener.local_addr().expect("staging address");
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .expect("serve staging router");
    });

    let item = outgoing_test_item("apk.bin", b"staged");
    let offer = outgoing_test_offer(&runtime.node_id(), "staging-peer", vec![item.clone()]);
    let created = runtime
        .http
        .post(format!("http://{address}/api/v1/outgoing"))
        .json(&offer)
        .send()
        .await
        .expect("create staging");
    assert!(created.status().is_success());

    let item_url = format!(
        "http://{address}/api/v1/outgoing/{}/items/{}",
        offer.transfer_id, item.item_id
    );
    let rejected = runtime
        .http
        .put(item_url.as_str())
        .body(b"wrong".to_vec())
        .send()
        .await
        .expect("stage wrong bytes");
    assert_eq!(rejected.status(), reqwest::StatusCode::BAD_REQUEST);
    assert!(!runtime.is_outgoing_item_ready(&offer.transfer_id, &item.item_id));

    let staged = runtime
        .http
        .put(item_url.as_str())
        .body(b"staged".to_vec())
        .send()
        .await
        .expect("stage bytes");
    assert!(staged.status().is_success());
    assert!(runtime.is_outgoing_item_ready(&offer.transfer_id, &item.item_id));
    let repeated = runtime
        .http
        .put(item_url.as_str())
        .body(b"staged".to_vec())
        .send()
        .await
        .expect("stage repeated bytes");
    assert!(repeated.status().is_success());

    let started = runtime
        .http
        .post(format!(
            "http://{address}/api/v1/outgoing/{}/start",
            offer.transfer_id
        ))
        .send()
        .await
        .expect("start staging");
    assert_eq!(started.status(), reqwest::StatusCode::CONFLICT);
    assert_eq!(
        runtime
            .transfer(&offer.transfer_id)
            .expect("staged record")
            .progress
            .status,
        TransferStatus::Failed
    );

    let staged_path = runtime
        .outgoing_staged_target(&offer.transfer_id, &item.item_id)
        .expect("staged path");
    assert!(staged_path.exists());

    runtime
        .stopping
        .store(true, std::sync::atomic::Ordering::SeqCst);
    server.abort();
    let _ = server.await;
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn accepted_transfer_without_upload_fails_and_notifies_source() {
    let data_dir = std::env::temp_dir().join(format!("meshshare-accepted-idle-{}", Uuid::new_v4()));
    let runtime = NodeRuntime::new(data_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    let (tx, mut rx) = peer_channel(8);
    assert!(runtime.add_peer_channel("source".to_string(), tx));
    let item = outgoing_test_item("payload.bin", b"staged");
    let offer = TransferOffer {
        transfer_id: "accepted-idle".to_string(),
        source_node_id: "source".to_string(),
        source_name: "source".to_string(),
        target_node_id: runtime.node_id(),
        items: vec![item],
        total_bytes: 6,
        created_at_ms: crate::clock::now_millis(),
    };
    assert!(runtime.add_transfer(offer));
    let destination = data_dir.join("received");
    std::fs::create_dir_all(&destination).expect("create destination");
    runtime
        .set_transfer_acceptance("accepted-idle", destination)
        .expect("accept transfer");
    {
        let mut transfers = runtime.transfers.write();
        let record = transfers.get_mut("accepted-idle").expect("transfer record");
        record.updated_at_ms = crate::clock::now_millis() - TRANSFER_ACCEPTED_IDLE_TIMEOUT_MS - 1;
    }
    runtime.prune_transfers();
    let record = runtime.transfer("accepted-idle").expect("transfer record");
    assert_eq!(record.progress.status, TransferStatus::Failed);
    assert!(record
        .progress
        .error
        .as_deref()
        .is_some_and(|error| error.contains("没有开始上传")));
    match rx.try_recv().expect("fail action queued") {
        WireMessage::TransferAction {
            transfer_id,
            action,
            reason,
            retry_count,
        } => {
            assert_eq!(transfer_id, "accepted-idle");
            assert_eq!(action, crate::model::TransferAction::Fail);
            assert!(reason.is_some());
            assert_eq!(retry_count, 0);
        }
        other => panic!("unexpected wire message: {other:?}"),
    }
    let _ = std::fs::remove_dir_all(data_dir);
}

#[tokio::test]
async fn outgoing_staging_survives_until_upload_starts() {
    let data_dir =
        std::env::temp_dir().join(format!("meshshare-staging-window-{}", Uuid::new_v4()));
    let runtime = NodeRuntime::new(data_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create runtime");
    let item = outgoing_test_item("payload.bin", b"staged");
    let offer = outgoing_test_offer(&runtime.node_id(), "receiver", vec![item.clone()]);
    runtime
        .begin_outgoing_staging(offer.clone())
        .expect("begin staging");
    runtime.spawn_outgoing_upload_supervisor();
    tokio::time::sleep(std::time::Duration::from_millis(1_200)).await;
    // 还没调用 /start 时没有传输记录，监督器不能把复制式暂存清掉。
    assert_eq!(
        runtime.outgoing_staging_counts(&offer.transfer_id),
        Some((0, 1))
    );
    runtime.mark_outgoing_item_staged(&offer.transfer_id, &item.item_id, item.sha256.clone());
    assert_eq!(
        runtime.outgoing_staging_counts(&offer.transfer_id),
        Some((1, 1))
    );
    runtime
        .stopping
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let _ = std::fs::remove_dir_all(data_dir);
}

async fn wait_for_outgoing_record(
    runtime: &Arc<NodeRuntime>,
    transfer_id: &str,
) -> TransferProgress {
    for _ in 0..300 {
        if let Some(record) = runtime.transfer(transfer_id) {
            return record.progress;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("outgoing transfer was not registered");
}

async fn wait_for_outgoing_prepare(runtime: &Arc<NodeRuntime>, transfer_id: &str) {
    for _ in 0..300 {
        if runtime.outgoing_prepare_view(transfer_id).is_none() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("outgoing prepare did not finish");
}

async fn outgoing_receiver_fixture() -> (
    Arc<NodeRuntime>,
    Arc<NodeRuntime>,
    std::net::SocketAddr,
    tokio::task::JoinHandle<()>,
    PathBuf,
) {
    let receiver_dir =
        std::env::temp_dir().join(format!("meshshare-path-receiver-{}", Uuid::new_v4()));
    let static_root = receiver_dir.join("web");
    std::fs::create_dir_all(&static_root).expect("create static directory");
    std::fs::write(static_root.join("index.html"), "<html></html>").expect("write static index");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind receiver");
    let address = listener.local_addr().expect("receiver address");
    let receiver = NodeRuntime::new(
        receiver_dir.clone(),
        address.port(),
        env!("CARGO_PKG_VERSION").into(),
    )
    .expect("create receiver runtime");
    let app = crate::server::router(receiver.clone(), static_root);
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let sender_dir = std::env::temp_dir().join(format!("meshshare-path-sender-{}", Uuid::new_v4()));
    let sender = NodeRuntime::new(sender_dir.clone(), 0, env!("CARGO_PKG_VERSION").into())
        .expect("create sender runtime");
    register_peer_endpoint(&sender, receiver.node_info());
    (receiver, sender, address, server, sender_dir)
}

#[tokio::test]
async fn mobile_proxy_uses_the_final_target_on_every_transfer_request() {
    let (occupant, gateway, _, receiver_task, dir) = outgoing_receiver_fixture().await;
    let mut stale = occupant.node_info();
    stale.node_id = "previous-node".into();
    register_peer_endpoint(&gateway, stale);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = crate::server::router(gateway.clone(), dir.join("web"));
    let gateway_task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let special_id = "传输 #3?50%";
    occupant.add_transfer(incoming_history_offer(&occupant, special_id, "source"));
    let path = crate::server::encode_path_segment(special_id);
    let url = format!(
        "http://{address}/api/v1/mobile/peers/{}/api/v1/transfers/{path}/status",
        occupant.node_id()
    );
    let progress = gateway
        .http
        .get(url)
        .header(crate::model::TARGET_HEADER, gateway.node_id())
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json::<TransferProgress>()
        .await
        .unwrap();
    assert_eq!(progress.transfer_id, special_id);

    let offer = incoming_history_offer(&occupant, "proxy-target", "source");
    occupant.add_transfer(offer.clone());
    let transfers = format!("http://{address}/api/v1/mobile/peers/previous-node/api/v1/transfers");
    let transfer = format!("{transfers}/proxy-target");
    let mut requested = offer;
    requested.target_node_id = "previous-node".into();
    for request in [
        gateway.http.post(&transfers).json(&requested),
        gateway.http.get(format!("{transfer}/status")),
        gateway.http.post(format!("{transfer}/cancel")),
        gateway.http.post(format!("{transfer}/retry")),
        gateway
            .http
            .post(format!("{transfer}/fail"))
            .json(&serde_json::json!({"error":"test"})),
        gateway
            .http
            .put(format!("{transfer}/items/proxy-target-item"))
            .body("data"),
    ] {
        let response = request
            .header(crate::model::TARGET_HEADER, gateway.node_id())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::MISDIRECTED_REQUEST);
    }
    assert_eq!(
        occupant.transfer("proxy-target").unwrap().progress.status,
        TransferStatus::Waiting
    );
    gateway_task.abort();
    receiver_task.abort();
    let _ = std::fs::remove_dir_all(dir);
    let _ = std::fs::remove_dir_all(&occupant.data_dir);
}

#[tokio::test]
async fn outgoing_paths_stream_original_files_without_copying() {
    let (receiver, sender, _address, server, sender_dir) = outgoing_receiver_fixture().await;
    let source_dir = sender_dir.join("source");
    std::fs::create_dir_all(&source_dir).expect("create source directory");
    let payload = b"original file payload".to_vec();
    let source_file = source_dir.join("payload.bin");
    std::fs::write(&source_file, &payload).expect("write source file");
    let mut hasher = Sha256::new();
    hasher.update(&payload);
    let expected_digest = format!("{:x}", hasher.finalize());

    let (tx, mut rx) = peer_channel(8);
    assert!(sender.add_peer_channel(receiver.node_id(), tx));
    let view = sender
        .begin_outgoing_paths(
            receiver.node_id(),
            vec![source_file.to_string_lossy().into_owned()],
        )
        .expect("begin outgoing paths");
    wait_for_outgoing_prepare(&sender, &view.transfer_id).await;
    let progress = wait_for_outgoing_record(&sender, &view.transfer_id).await;
    assert_eq!(progress.status, TransferStatus::Waiting);
    assert_eq!(progress.total_bytes, payload.len() as u64);
    let offer = match tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("offer was sent")
        .expect("offer message")
    {
        WireMessage::TransferOffer { offer } => offer,
        other => panic!("unexpected wire message: {other:?}"),
    };
    assert_eq!(offer.items[0].sha256, expected_digest);
    assert_eq!(offer.items[0].relative_path, "payload.bin");
    // 原路径直传不应产生任何暂存副本。
    let staging_dir = sender_dir.join("outgoing").join(&view.transfer_id);
    let staged_files = std::fs::read_dir(&staging_dir)
        .map(|entries| entries.filter_map(Result::ok).count())
        .unwrap_or(0);
    assert_eq!(staged_files, 0);

    receiver.add_transfer(offer.clone());
    let destination =
        std::env::temp_dir().join(format!("meshshare-path-target-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&destination).expect("create destination");
    receiver
        .set_transfer_acceptance(&offer.transfer_id, destination.clone())
        .expect("accept transfer");
    sender.mark_transfer_accepted(&offer.transfer_id, 0);
    sender
        .upload_outgoing_transfer(&offer.transfer_id)
        .await
        .expect("upload original file");
    assert_eq!(
        std::fs::read(destination.join("payload.bin")).expect("read received payload"),
        payload
    );
    assert_eq!(
        sender
            .transfer(&offer.transfer_id)
            .expect("sender record")
            .progress
            .status,
        TransferStatus::Completed
    );
    assert!(!staging_dir.exists());

    sender
        .stopping
        .store(true, std::sync::atomic::Ordering::SeqCst);
    server.abort();
    let _ = server.await;
    let _ = std::fs::remove_dir_all(&sender_dir);
    let _ = std::fs::remove_dir_all(destination);
}

#[tokio::test]
async fn outgoing_paths_expand_directory_relative_paths() {
    let (_receiver, sender, _address, server, sender_dir) = outgoing_receiver_fixture().await;
    let root = sender_dir.join("folder");
    std::fs::create_dir_all(root.join("sub")).expect("create nested directory");
    std::fs::write(root.join("a.txt"), b"alpha").expect("write first file");
    std::fs::write(root.join("sub/b.txt"), b"beta").expect("write second file");

    let target = "folder-peer";
    let (tx, mut rx) = peer_channel(8);
    assert!(sender.add_peer_channel(target.to_string(), tx));
    let view = sender
        .begin_outgoing_paths(
            target.to_string(),
            vec![root.to_string_lossy().into_owned()],
        )
        .expect("begin outgoing paths");
    wait_for_outgoing_prepare(&sender, &view.transfer_id).await;
    let progress = wait_for_outgoing_record(&sender, &view.transfer_id).await;
    assert_eq!(progress.total_items, 2);
    assert_eq!(progress.total_bytes, 9);
    let offer = match tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("offer was sent")
        .expect("offer message")
    {
        WireMessage::TransferOffer { offer } => offer,
        other => panic!("unexpected wire message: {other:?}"),
    };
    let mut paths = offer
        .items
        .iter()
        .map(|item| item.relative_path.clone())
        .collect::<Vec<_>>();
    paths.sort();
    assert_eq!(paths, vec!["folder/a.txt", "folder/sub/b.txt"]);

    sender
        .stopping
        .store(true, std::sync::atomic::Ordering::SeqCst);
    server.abort();
    let _ = server.await;
    let _ = std::fs::remove_dir_all(&sender_dir);
}

#[tokio::test]
async fn outgoing_paths_fail_when_source_changes() {
    let (receiver, sender, _address, server, sender_dir) = outgoing_receiver_fixture().await;
    let source_file = sender_dir.join("changing.bin");
    std::fs::write(&source_file, b"stable").expect("write source file");
    let (tx, mut rx) = peer_channel(8);
    assert!(sender.add_peer_channel(receiver.node_id(), tx));
    let view = sender
        .begin_outgoing_paths(
            receiver.node_id(),
            vec![source_file.to_string_lossy().into_owned()],
        )
        .expect("begin outgoing paths");
    wait_for_outgoing_prepare(&sender, &view.transfer_id).await;
    let offer = match rx.recv().await.expect("offer message") {
        WireMessage::TransferOffer { offer } => offer,
        other => panic!("unexpected wire message: {other:?}"),
    };
    receiver.add_transfer(offer.clone());
    let destination = sender_dir.join("changed-target");
    std::fs::create_dir_all(&destination).expect("create destination");
    receiver
        .set_transfer_acceptance(&offer.transfer_id, destination)
        .expect("accept transfer");
    sender.mark_transfer_accepted(&offer.transfer_id, 0);
    // 准备完成后源文件被追加内容：上传前的大小对比必须拦住。
    std::fs::write(&source_file, b"changed and longer").expect("rewrite source file");
    let error = sender
        .upload_outgoing_transfer(&offer.transfer_id)
        .await
        .expect_err("changed source must fail");
    assert!(error.contains("源文件已变化"), "unexpected error: {error}");
    assert_eq!(
        sender
            .transfer(&offer.transfer_id)
            .expect("sender record")
            .progress
            .status,
        TransferStatus::Failed
    );

    sender
        .stopping
        .store(true, std::sync::atomic::Ordering::SeqCst);
    server.abort();
    let _ = server.await;
    let _ = std::fs::remove_dir_all(&sender_dir);
}

#[tokio::test]
async fn headless_stores_and_relays_a_remote_event_once_without_writing_a_system_clipboard() {
    let path = temp_data_dir();
    let start = || {
        NodeRuntime::new_with_profile(path.clone(), 0, "1.0.0".into(), NodeProfile::Headless)
            .unwrap()
    };
    let nas = start();
    let id = nas.node_id();
    assert!(nas.remote_file_access());
    assert!(nas.supports_capability(crate::model::TERMINAL_SSH_CAPABILITY));
    assert!(!nas.clipboard_status().retrying);
    let wire = WireMessage::ClipboardUpdate {
        event: ClipboardEvent {
            event_id: "relay-test".into(),
            origin_node_id: "a".into(),
            clock: Hlc {
                physical_ms: 1,
                logical: 0,
                node_id: "a".into(),
            },
            text: Some("relay through NAS".into()),
            image_png: None,
            updated_at_ms: 1,
        },
    };
    let (sender, mut second) = peer_channel(4);
    nas.set_peer(test_peer("b", vec!["clipboard.text".into()]));
    assert!(nas.add_peer_channel("b".into(), sender));
    for source in ["a", "a", "b"] {
        crate::server::handle_wire_message(wire.clone(), &nas, Some(source)).await;
    }
    assert!(matches!(
        second.try_recv(),
        Ok(WireMessage::ClipboardUpdate { .. })
    ));
    assert!(second.try_recv().is_err());
    assert_eq!(nas.clipboard_history_list().items.len(), 1);
    #[cfg(not(target_os = "android"))]
    assert!(nas.take_system_clipboard_write().is_none());
    drop(nas);
    assert!(
        NodeRuntime::new_with_profile(path.clone(), 0, "1.0.0".into(), NodeProfile::Desktop)
            .is_err()
    );
    let restarted = start();
    assert_eq!(restarted.node_id(), id);
    assert_eq!(
        restarted.clipboard_snapshot().unwrap().text.as_deref(),
        Some("relay through NAS")
    );
    drop(restarted);
    std::fs::remove_dir_all(path).unwrap();
}
