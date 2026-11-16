use crate::{control::ControlPlane, mobile_tls, runtime, server};
use anyhow::Context;
use axum::Router;
use axum_server::tls_rustls::RustlsConfig;
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tauri::AppHandle;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

const RESCAN_INTERVAL: Duration = Duration::from_secs(2);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

struct Listener {
    handle: axum_server::Handle<SocketAddr>,
    task: Option<JoinHandle<()>>,
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.handle.graceful_shutdown(Some(SHUTDOWN_TIMEOUT));
    }
}

struct Listeners {
    http: BTreeMap<Ipv4Addr, Listener>,
    https: BTreeMap<Ipv4Addr, Listener>,
    failed_binds: BTreeSet<(Ipv4Addr, u16)>,
    last_candidates: BTreeSet<Ipv4Addr>,
    last_network_rebind_generation: u64,
    http_port: u16,
    https_port: u16,
    tls: RustlsConfig,
    certificate_addresses: BTreeSet<Ipv4Addr>,
    tls_data_dir: PathBuf,
    app: Router,
}

pub async fn start_with_app(
    data_dir: PathBuf,
    static_root: PathBuf,
    version: String,
    preferred_port: u16,
    app: AppHandle,
) -> anyhow::Result<(Arc<runtime::NodeRuntime>, Arc<ControlPlane>)> {
    let addresses = runtime::bind_addresses();
    let (mut listeners, runtime, control) = Listeners::start(
        data_dir,
        static_root,
        version,
        preferred_port,
        &addresses,
        Some(app),
    )
    .await?;
    listeners.reconcile(&runtime, &addresses).await;
    let owner = runtime.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(RESCAN_INTERVAL).await;
            if owner.stopping.load(Ordering::SeqCst) {
                break;
            }
            listeners
                .reconcile(&owner, &runtime::bind_addresses())
                .await;
        }
    });
    Ok((runtime, control))
}

impl Listeners {
    async fn start(
        data_dir: PathBuf,
        static_root: PathBuf,
        version: String,
        preferred_port: u16,
        addresses: &BTreeSet<Ipv4Addr>,
        control_app: Option<AppHandle>,
    ) -> anyhow::Result<(Self, Arc<runtime::NodeRuntime>, Arc<ControlPlane>)> {
        let local = Ipv4Addr::LOCALHOST;
        let http = match TcpListener::bind((local, preferred_port)).await {
            Ok(listener) => listener,
            Err(_) => TcpListener::bind((local, 0))
                .await
                .context("无法监听本机 HTTP")?,
        };
        let https = TcpListener::bind((local, 0))
            .await
            .context("无法监听本机 HTTPS")?;
        let http_port = http.local_addr()?.port();
        let https_port = https.local_addr()?.port();
        let certificate_addresses = addresses.iter().copied().collect::<Vec<_>>();
        let tls = mobile_tls::ensure(&data_dir, &certificate_addresses)?;
        let config =
            RustlsConfig::from_pem_file(&tls.server_certificate_path, &tls.server_key_path).await?;
        let runtime = runtime::NodeRuntime::new(data_dir.clone(), http_port, version)?;
        runtime.set_mobile_ca(tls.ca_certificate_path, tls.fingerprint);
        let control = control_app
            .map(|app| ControlPlane::with_app(runtime.clone(), app))
            .unwrap_or_else(|| ControlPlane::runtime_only(runtime.clone()));
        let app = server::router_with_control(runtime.clone(), static_root, control.clone());
        let listeners = Self {
            http: BTreeMap::from([(local, serve_http(http, app.clone())?)]),
            https: BTreeMap::from([(local, serve_https(https, app.clone(), config.clone())?)]),
            failed_binds: BTreeSet::new(),
            last_candidates: addresses.clone(),
            last_network_rebind_generation: runtime.network_rebind_generation(),
            http_port,
            https_port,
            tls: config,
            certificate_addresses: addresses.clone(),
            tls_data_dir: data_dir,
            app,
        };
        Ok((listeners, runtime, control))
    }

    async fn reconcile(&mut self, runtime: &runtime::NodeRuntime, candidates: &BTreeSet<Ipv4Addr>) {
        let network_changed = needs_network_listener_restart(
            runtime,
            &self.last_candidates,
            candidates,
            self.last_network_rebind_generation,
        );
        if network_changed {
            self.restart_network_listeners().await;
        }
        self.last_candidates = candidates.clone();
        self.last_network_rebind_generation = runtime.network_rebind_generation();
        self.http.retain(|ip, listener| {
            candidates.contains(ip)
                && listener
                    .task
                    .as_ref()
                    .is_some_and(|task| !task.is_finished())
        });
        self.failed_binds.retain(|(ip, _)| candidates.contains(ip));
        for &ip in candidates {
            if self.http.contains_key(&ip) {
                continue;
            }
            let result = match TcpListener::bind((ip, self.http_port)).await {
                Ok(listener) => serve_http(listener, self.app.clone()),
                Err(error) => Err(error),
            };
            if let Some(listener) = self.track_bind(ip, self.http_port, result) {
                self.http.insert(ip, listener);
            }
        }

        let active = self.http.keys().copied().collect::<BTreeSet<_>>();
        self.https.retain(|ip, listener| {
            active.contains(ip)
                && listener
                    .task
                    .as_ref()
                    .is_some_and(|task| !task.is_finished())
        });
        if !active.is_subset(&self.certificate_addresses) {
            match self.reload_certificate(&active).await {
                Ok(()) => self.certificate_addresses = active.clone(),
                Err(error) => eprintln!("更新手机 HTTPS 证书失败：{error:#}"),
            }
        }
        for &ip in &active {
            if !self.certificate_addresses.contains(&ip) || self.https.contains_key(&ip) {
                continue;
            }
            let result = match TcpListener::bind((ip, self.https_port)).await {
                Ok(listener) => serve_https(listener, self.app.clone(), self.tls.clone()),
                Err(error) => Err(error),
            };
            if let Some(listener) = self.track_bind(ip, self.https_port, result) {
                self.https.insert(ip, listener);
            }
        }
        runtime.set_listening_addresses(&active, self.https.keys().copied(), self.https_port);
    }

    /// Android's process network binding only affects sockets created after
    /// the binding changes. Recreate non-loopback listeners when a VPN or LAN
    /// interface appears/disappears so inbound HTTP/WS traffic follows the
    /// newly selected physical network.
    async fn restart_network_listeners(&mut self) {
        let local = Ipv4Addr::LOCALHOST;
        let mut stopping = Vec::new();
        let http = std::mem::take(&mut self.http);
        for (ip, listener) in http {
            if ip == local {
                self.http.insert(ip, listener);
            } else {
                stopping.push(listener);
            }
        }
        let https = std::mem::take(&mut self.https);
        for (ip, listener) in https {
            if ip == local {
                self.https.insert(ip, listener);
            } else {
                stopping.push(listener);
            }
        }
        self.failed_binds.retain(|(ip, _)| *ip == local);
        for mut listener in stopping {
            listener.handle.graceful_shutdown(Some(SHUTDOWN_TIMEOUT));
            if let Some(task) = listener.task.take() {
                let _ = task.await;
            }
        }
    }

    fn track_bind(
        &mut self,
        ip: Ipv4Addr,
        port: u16,
        result: io::Result<Listener>,
    ) -> Option<Listener> {
        match result {
            Ok(listener) => {
                self.failed_binds.remove(&(ip, port));
                Some(listener)
            }
            Err(error) => {
                if self.failed_binds.insert((ip, port)) {
                    eprintln!("监听 {ip}:{port} 失败，将重试：{error}");
                }
                None
            }
        }
    }

    async fn reload_certificate(&self, addresses: &BTreeSet<Ipv4Addr>) -> anyhow::Result<()> {
        let data_dir = self.tls_data_dir.clone();
        let addresses = addresses.iter().copied().collect::<Vec<_>>();
        let material =
            tokio::task::spawn_blocking(move || mobile_tls::ensure(&data_dir, &addresses))
                .await??;
        self.tls
            .reload_from_pem_file(material.server_certificate_path, material.server_key_path)
            .await?;
        Ok(())
    }
}

fn needs_network_listener_restart(
    runtime: &runtime::NodeRuntime,
    previous: &BTreeSet<Ipv4Addr>,
    current: &BTreeSet<Ipv4Addr>,
    last_network_rebind_generation: u64,
) -> bool {
    runtime.is_android_profile()
        && (previous != current
            || runtime.network_rebind_generation() != last_network_rebind_generation)
}

fn serve_http(listener: TcpListener, app: Router) -> io::Result<Listener> {
    let address = listener.local_addr()?;
    let server = axum_server::from_tcp(listener.into_std()?)?;
    let handle = axum_server::Handle::new();
    let future = server
        .handle(handle.clone())
        .serve(app.into_make_service_with_connect_info::<SocketAddr>());
    Ok(serve(address, handle, future))
}

fn serve_https(listener: TcpListener, app: Router, config: RustlsConfig) -> io::Result<Listener> {
    let address = listener.local_addr()?;
    let server = axum_server::from_tcp_rustls(listener.into_std()?, config)?;
    let handle = axum_server::Handle::new();
    let future = server
        .handle(handle.clone())
        .serve(app.into_make_service_with_connect_info::<SocketAddr>());
    Ok(serve(address, handle, future))
}

fn serve(
    address: SocketAddr,
    handle: axum_server::Handle<SocketAddr>,
    future: impl Future<Output = io::Result<()>> + Send + 'static,
) -> Listener {
    let task = tokio::spawn(async move {
        if let Err(error) = future.await {
            eprintln!("监听 {address} 已停止：{error}");
        }
    });
    Listener {
        handle,
        task: Some(task),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::NodeInfo;
    use crate::runtime::NodeProfile;

    #[tokio::test]
    async fn new_interface_is_retried_served_and_removed_without_restarting() {
        let local = Ipv4Addr::LOCALHOST;
        let Some(ip) = runtime::bind_addresses()
            .into_iter()
            .find(|ip| !ip.is_loopback())
        else {
            return;
        };
        let path =
            std::env::temp_dir().join(format!("meshshare-listeners-{}", uuid::Uuid::new_v4()));
        let local_only = BTreeSet::from([local]);
        let with_network = BTreeSet::from([local, ip]);
        let (mut listeners, runtime, _) = Listeners::start(
            path.clone(),
            path.clone(),
            "test".into(),
            0,
            &local_only,
            None,
        )
        .await
        .expect("start listeners");
        listeners.reconcile(&runtime, &local_only).await;
        assert!(runtime.mobile_access_info().bootstrap_urls.is_empty());
        assert!(runtime.mobile_access_info().secure_urls.is_empty());

        let http_port = listeners.http_port;
        let https_port = listeners.https_port;
        let occupied = TcpListener::bind((ip, http_port))
            .await
            .expect("occupy the new interface's HTTP port");
        listeners.reconcile(&runtime, &with_network).await;
        assert!(runtime.node_info().addresses.is_empty());
        drop(occupied);

        let ca = std::fs::read(runtime.mobile_ca_path().expect("local CA")).expect("read local CA");
        let fingerprint = runtime.mobile_access_info().ca_fingerprint;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(3))
            .no_proxy()
            .add_root_certificate(reqwest::Certificate::from_pem(&ca).expect("parse local CA"))
            .build()
            .expect("build trust-checked client");
        listeners.reconcile(&runtime, &with_network).await;
        for base in [
            format!("http://{ip}:{http_port}"),
            format!("https://{ip}:{https_port}"),
        ] {
            let info = client
                .get(format!("{base}/api/v1/info"))
                .send()
                .await
                .expect("connect to the new interface")
                .error_for_status()
                .expect("get node info")
                .json::<NodeInfo>()
                .await
                .expect("decode node info");
            assert_eq!(info.addresses[0].address, ip.to_string());
            assert_eq!(info.mobile_addresses[0].address, ip.to_string());
        }
        assert_eq!(runtime.mobile_access_info().ca_fingerprint, fingerprint);

        listeners.reconcile(&runtime, &local_only).await;
        assert!(runtime.mobile_access_info().bootstrap_urls.is_empty());
        assert!(runtime.mobile_access_info().secure_urls.is_empty());
        assert!(!listeners.http.contains_key(&ip));
        assert!(!listeners.https.contains_key(&ip));
        client
            .get(format!("http://{local}:{http_port}/api/v1/info"))
            .send()
            .await
            .expect("local HTTP is still listening")
            .error_for_status()
            .expect("local node is still serving");
        drop(listeners);
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn android_network_changes_request_listener_restart() {
        let path = std::env::temp_dir().join(format!(
            "meshshare-listener-network-change-{}",
            uuid::Uuid::new_v4()
        ));
        let runtime = runtime::NodeRuntime::new_with_profile(
            path.clone(),
            0,
            "test".into(),
            NodeProfile::Android,
        )
        .expect("create Android runtime");
        let local = BTreeSet::from([Ipv4Addr::LOCALHOST]);
        let wifi = BTreeSet::from([Ipv4Addr::LOCALHOST, Ipv4Addr::new(192, 168, 1, 20)]);
        assert!(!needs_network_listener_restart(&runtime, &local, &local, 0));
        assert!(needs_network_listener_restart(&runtime, &local, &wifi, 0));
        assert!(!needs_network_listener_restart(&runtime, &local, &local, 0));
        runtime.request_network_rebind();
        assert!(needs_network_listener_restart(&runtime, &local, &local, 0));
        let _ = std::fs::remove_dir_all(path);
    }
}
