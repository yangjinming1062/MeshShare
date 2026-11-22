//! 只有本地主窗口拥有交互终端。网络入站会话由 SSH 服务独立持有。
use super::{ssh::Settings, TerminalEvent, TerminalSessions};
use crate::{runtime::NodeRuntime, ManagedNode};
use anyhow::{ensure, Context};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use tauri::ipc::{Channel, CommandArg, CommandItem, InvokeError};
use tauri::{Manager, Runtime};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub(crate) struct MainTerminal(Arc<NodeRuntime>);

impl<'de, R: Runtime> CommandArg<'de, R> for MainTerminal {
    fn from_command(command: CommandItem<'de, R>) -> Result<Self, InvokeError> {
        // Tauri 在分发应用命令前检查请求来源；终端再限制到实际发出 IPC 的 WebView。
        let webview = command.message.webview_ref();
        if webview.label() != "main" || webview.window().label() != "main" {
            return Err("终端仅在本机主窗口可用".into());
        }
        let state = webview.try_state::<ManagedNode>().ok_or("节点尚未就绪")?;
        Ok(Self(state.runtime.clone()))
    }
}

#[derive(Default)]
pub(crate) struct WindowTerminals {
    sessions: TerminalSessions,
    opening: Mutex<HashMap<String, CancellationToken>>,
}

struct PendingOpen<'a> {
    owner: &'a WindowTerminals,
    id: &'a str,
    cancel: CancellationToken,
}

impl Drop for PendingOpen<'_> {
    fn drop(&mut self) {
        let mut pending = self.owner.opening.lock();
        // 已取消的 ID 可能已被新请求复用。
        if !self.cancel.is_cancelled() {
            pending.remove(self.id);
        }
    }
}

impl WindowTerminals {
    fn begin<'a>(&'a self, id: &'a str) -> anyhow::Result<PendingOpen<'a>> {
        ensure!(Uuid::parse_str(id).is_ok(), "终端会话标识无效");
        let mut pending = self.opening.lock();
        ensure!(!pending.contains_key(id), "终端正在连接");
        let cancel = CancellationToken::new();
        pending.insert(id.to_owned(), cancel.clone());
        Ok(PendingOpen {
            owner: self,
            id,
            cancel,
        })
    }

    fn close(&self, id: &str) -> anyhow::Result<()> {
        let mut pending = self.opening.lock();
        if let Some(cancel) = pending.remove(id) {
            cancel.cancel();
        }
        self.sessions.close(id)
    }

    pub(crate) fn close_all(&self) {
        let mut pending = self.opening.lock();
        for (_, cancel) in pending.drain() {
            cancel.cancel();
        }
        self.sessions.close_all();
    }
}

fn describe(error: anyhow::Error) -> String {
    format!("{error:#}")
}

#[tauri::command]
pub(crate) async fn terminal_open(
    main: MainTerminal,
    id: String,
    peer_id: Option<String>,
    cols: u16,
    rows: u16,
    on_event: Channel<TerminalEvent>,
) -> Result<(), String> {
    open(&main.0, &id, peer_id, cols, rows, on_event)
        .await
        .map_err(describe)
}

async fn open(
    runtime: &Arc<NodeRuntime>,
    id: &str,
    peer_id: Option<String>,
    cols: u16,
    rows: u16,
    channel: Channel<TerminalEvent>,
) -> anyhow::Result<()> {
    super::validate_size(cols, rows)?;
    let activity = runtime.reserve_terminal()?;
    let pending = runtime.terminals.begin(id)?;
    let opening = async {
        if let Some(peer_id) = peer_id {
            {
                let peers = runtime.peers.read();
                let peer = peers
                    .get(&peer_id)
                    .filter(|peer| peer.online)
                    .context("目标节点已离线")?;
                ensure!(
                    peer.capabilities
                        .iter()
                        .any(|cap| cap == crate::model::TERMINAL_SSH_CAPABILITY),
                    "目标节点未提供终端能力"
                );
            }
            let bases = runtime.peer_http_bases(&peer_id);
            runtime.ssh.connect(peer_id, bases, cols, rows).await
        } else {
            let shell_activity = activity.clone();
            tokio::task::spawn_blocking(move || super::spawn_pty(cols, rows, shell_activity))
                .await?
        }
    };
    let io = tokio::select! {
        biased;
        _ = pending.cancel.cancelled() => anyhow::bail!("终端连接已取消"),
        io = opening => io?,
    };
    let _guard = runtime.terminals.opening.lock();
    ensure!(!pending.cancel.is_cancelled(), "终端连接已取消");
    runtime
        .terminals
        .sessions
        .attach(id.to_owned(), io, channel, activity)
}

#[tauri::command]
pub(crate) async fn terminal_write(
    main: MainTerminal,
    id: String,
    data: Vec<u8>,
) -> Result<(), String> {
    main.0
        .terminals
        .sessions
        .write(&id, data)
        .await
        .map_err(describe)
}

#[tauri::command]
pub(crate) async fn terminal_resize(
    main: MainTerminal,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    main.0
        .terminals
        .sessions
        .resize(&id, cols, rows)
        .await
        .map_err(describe)
}

#[tauri::command]
pub(crate) fn terminal_close(main: MainTerminal, id: String) -> Result<(), String> {
    main.0.terminals.close(&id).map_err(describe)
}

#[tauri::command]
pub(crate) fn terminal_ack(main: MainTerminal, id: String, sequence: u64) -> Result<(), String> {
    main.0
        .terminals
        .sessions
        .ack(&id, sequence)
        .map_err(describe)
}

#[tauri::command]
pub(crate) async fn terminal_settings(main: MainTerminal) -> Result<Settings, String> {
    main.0.ssh.state().await.map_err(describe)
}

#[tauri::command]
pub(crate) async fn terminal_set_enabled(main: MainTerminal, enabled: bool) -> Result<(), String> {
    main.0.ssh.set_enabled(enabled).await.map_err(describe)
}

#[tauri::command]
pub(crate) async fn terminal_pair(main: MainTerminal) -> Result<(), String> {
    main.0.ssh.begin_pairing().await.map_err(describe)
}

#[tauri::command]
pub(crate) fn terminal_decide(main: MainTerminal, id: String, allow: bool) -> Result<(), String> {
    main.0.ssh.decide(&id, allow).map_err(describe)
}

#[tauri::command]
pub(crate) async fn terminal_revoke(
    main: MainTerminal,
    direction: String,
    node_id: String,
) -> Result<(), String> {
    main.0.ssh.revoke(&direction, &node_id).map_err(describe)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_or_reloading_during_open_cannot_attach_a_session_to_a_new_page() {
        let window = WindowTerminals::default();
        let id = Uuid::new_v4().to_string();
        let first = window.begin(&id).unwrap();
        assert!(window.begin(&id).is_err());
        window.close(&id).unwrap();
        assert!(first.cancel.is_cancelled());

        let next = window.begin(&id).unwrap();
        drop(first);
        assert!(window.begin(&id).is_err());
        window.close_all();
        assert!(next.cancel.is_cancelled());
        drop(next);
        assert!(window.begin(&id).is_ok());
    }
}
