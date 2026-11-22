use super::*;
use parking_lot::Mutex;
use std::path::PathBuf;
use tokio::sync::Semaphore;

#[derive(Default)]
struct HostState {
    key: Option<Arc<PrivateKey>>,
    listener: Option<CancellationToken>,
}

pub(super) struct HostAccess {
    data_dir: PathBuf,
    // 凭据库与签名文件操作串行执行。网络线程只读取已验证的内存状态。
    edit: Mutex<()>,
    state: Mutex<HostState>,
    pub(super) sockets: Arc<Semaphore>,
}

impl HostAccess {
    pub(super) fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            edit: Mutex::new(()),
            state: Mutex::default(),
            sockets: Arc::new(Semaphore::new(16)),
        }
    }

    pub(super) fn accept(&self) -> Option<(Arc<PrivateKey>, CancellationToken)> {
        let state = self.state.lock();
        let listener = state
            .listener
            .as_ref()
            .filter(|token| !token.is_cancelled())?;
        Some((state.key.clone()?, listener.child_token()))
    }

    #[cfg(test)]
    pub(super) fn inject_key(&self, key: PrivateKey) {
        self.state.lock().key = Some(Arc::new(key));
    }
}

impl SshService {
    pub(crate) async fn host_settings(self: &Arc<Self>, enabled: Option<bool>) -> Result<bool> {
        let service = self.clone();
        tokio::task::spawn_blocking(move || service.configure_host(enabled)).await?
    }

    fn configure_host(&self, requested: Option<bool>) -> Result<bool> {
        let host = &self.host;
        let _edit = host.edit.lock();
        let (key, current) = {
            let state = host.state.lock();
            (state.key.clone(), state.listener.is_some())
        };
        let (key, current) = match key {
            Some(key) => (key, current),
            None => {
                let Some((key, enabled)) =
                    trust::load(&host.data_dir, &self.node_id, requested == Some(true))?
                else {
                    return Ok(false);
                };
                (Arc::new(key), enabled)
            }
        };
        let enabled = requested.unwrap_or(current);
        if enabled != current {
            trust::save(&host.data_dir, &key, enabled)?;
        }
        let mut state = host.state.lock();
        state.key = Some(key.clone());
        if enabled && state.listener.is_none() {
            state.listener = Some(self.stopped.child_token());
            self.host_fingerprint.send_replace(Some(
                key.public_key().fingerprint(HashAlg::Sha256).to_string(),
            ));
        } else if !enabled {
            if let Some(listener) = state.listener.take() {
                listener.cancel();
                self.host_fingerprint.send_replace(None);
            }
        }
        Ok(enabled)
    }
}
