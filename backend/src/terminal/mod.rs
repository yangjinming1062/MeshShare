pub(crate) mod commands;
mod pty;
pub(crate) mod ssh;

use anyhow::{bail, ensure, Context, Result};
use parking_lot::Mutex;
use serde::Serialize;
use std::{collections::HashMap, sync::Arc};
use tauri::ipc::Channel;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

pub(crate) use pty::spawn_pty;

pub(crate) enum Input {
    Data(Vec<u8>),
    Resize { cols: u16, rows: u16 },
}

pub(crate) enum Output {
    Data(Vec<u8>),
    Exit { reason: String },
}

pub(crate) struct SessionIo {
    pub input: mpsc::Sender<Input>,
    pub output: mpsc::Receiver<Output>,
    pub cancel: CancellationToken,
}

impl Drop for SessionIo {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

pub(crate) fn validate_size(cols: u16, rows: u16) -> Result<()> {
    ensure!(
        (1..=1000).contains(&cols) && (1..=1000).contains(&rows),
        "终端尺寸超出范围"
    );
    Ok(())
}

#[derive(Serialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum TerminalEvent {
    Data { sequence: u64, data: Vec<u8> },
    Exit { reason: String },
}

struct AttachedSession {
    input: mpsc::Sender<Input>,
    cancel: CancellationToken,
    rendered: watch::Sender<u64>,
}

#[derive(Default)]
pub(crate) struct TerminalSessions {
    attached: Arc<Mutex<HashMap<String, Arc<AttachedSession>>>>,
}

impl TerminalSessions {
    pub(crate) fn attach(
        &self,
        id: String,
        mut io: SessionIo,
        channel: Channel<TerminalEvent>,
        activity: impl Send + 'static,
    ) -> Result<()> {
        let (rendered, mut receipt) = watch::channel(0u64);
        let entry = Arc::new(AttachedSession {
            input: io.input.clone(),
            cancel: io.cancel.clone(),
            rendered,
        });
        let attached = self.attached.clone();
        {
            let mut sessions = attached.lock();
            ensure!(sessions.len() < 32, "已达到终端标签上限");
            ensure!(!sessions.contains_key(&id), "终端会话已存在");
            sessions.insert(id.clone(), entry.clone());
        }
        tauri::async_runtime::spawn(async move {
            let _activity = activity;
            let mut sequence = 0;
            let reason = loop {
                let output = tokio::select! {
                    biased;
                    _ = io.cancel.cancelled() => break "会话已关闭".to_string(),
                    output = io.output.recv() => output,
                };
                match output {
                    Some(Output::Data(data)) => {
                        sequence += 1;
                        if channel
                            .send(TerminalEvent::Data { sequence, data })
                            .is_err()
                        {
                            break "终端界面已关闭".to_string();
                        }
                        // Channel 会立即排队，必须等 xterm 消费后才能继续向 WebView 发送。
                        tokio::select! {
                            _ = io.cancel.cancelled() => break "会话已关闭".to_string(),
                            result = receipt.wait_for(|seen| *seen >= sequence) => {
                                if result.is_err() { break "终端界面已关闭".to_string(); }
                            }
                        }
                    }
                    Some(Output::Exit { reason }) => break reason,
                    None => break "终端连接已断开".to_string(),
                }
            };
            let _ = channel.send(TerminalEvent::Exit { reason });
            let mut sessions = attached.lock();
            if sessions
                .get(&id)
                .is_some_and(|current| Arc::ptr_eq(current, &entry))
            {
                sessions.remove(&id);
            }
        });
        Ok(())
    }

    fn session(&self, id: &str) -> Result<Arc<AttachedSession>> {
        self.attached
            .lock()
            .get(id)
            .cloned()
            .context("终端会话已关闭")
    }

    pub(crate) async fn write(&self, id: &str, data: Vec<u8>) -> Result<()> {
        ensure!(data.len() <= 64 * 1024, "单次终端输入过长");
        self.send(id, Input::Data(data)).await
    }

    pub(crate) async fn resize(&self, id: &str, cols: u16, rows: u16) -> Result<()> {
        validate_size(cols, rows)?;
        self.send(id, Input::Resize { cols, rows }).await
    }

    async fn send(&self, id: &str, message: Input) -> Result<()> {
        let session = self.session(id)?;
        tokio::select! {
            biased;
            _ = session.cancel.cancelled() => bail!("终端会话已关闭"),
            result = session.input.send(message) => {
                result.map_err(|_| anyhow::anyhow!("终端会话已关闭"))
            }
        }
    }

    pub(crate) fn ack(&self, id: &str, sequence: u64) -> Result<()> {
        let entry = self.session(id)?;
        entry.rendered.send_if_modified(|seen| {
            if seen.checked_add(1) == Some(sequence) {
                *seen = sequence;
                true
            } else {
                false
            }
        });
        Ok(())
    }

    pub(crate) fn close(&self, id: &str) {
        if let Some(entry) = self.attached.lock().remove(id) {
            entry.cancel.cancel();
        }
    }

    pub(crate) fn close_all(&self) {
        for (_, entry) in self.attached.lock().drain() {
            entry.cancel.cancel();
        }
    }
}

impl Drop for TerminalSessions {
    fn drop(&mut self) {
        self.close_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // portable-pty 以继承光标模式创建 ConPTY。无界面的测试也要像 xterm 一样
    // 应答启动查询，等 cmd 或 PowerShell 显示提示符后再发送按键和尺寸。
    pub(super) async fn wait_for_windows_prompt(io: &mut SessionIo) {
        let mut seen = Vec::new();
        let ready = tokio::time::timeout(Duration::from_secs(12), async {
            while let Some(message) = io.output.recv().await {
                let bytes = match message {
                    Output::Data(bytes) => bytes,
                    Output::Exit { reason } => panic!("shell exited during startup: {reason}"),
                };
                for byte in bytes {
                    seen.push(byte);
                    let reply: &[u8] = if seen.ends_with(b"\x1b[6n") {
                        b"\x1b[1;1R"
                    } else if seen.ends_with(b"\x1b[c") || seen.ends_with(b"\x1b[0c") {
                        b"\x1b[?1;2c"
                    } else if byte == b'>' && !seen.ends_with(b"\x1b[>") {
                        return;
                    } else {
                        continue;
                    };
                    io.input.send(Input::Data(reply.to_vec())).await.unwrap();
                }
            }
            panic!("terminal output closed during startup");
        })
        .await;
        assert!(
            ready.is_ok(),
            "waiting for Windows prompt: {:?}",
            String::from_utf8_lossy(&seen)
        );
    }

    #[tokio::test]
    async fn headless_windows_terminal_answers_fragmented_startup_queries() {
        let (input, mut replies) = mpsc::channel(2);
        let (screen, output) = mpsc::channel(1);
        let mut io = SessionIo {
            input,
            output,
            cancel: CancellationToken::new(),
        };
        let console = async {
            for chunk in [b"\x1b[".as_slice(), b"6n\x1b", b"[c"] {
                screen.send(Output::Data(chunk.to_vec())).await.unwrap();
            }
            for reply in [b"\x1b[1;1R".as_slice(), b"\x1b[?1;2c"] {
                assert!(matches!(replies.recv().await, Some(Input::Data(bytes)) if bytes == reply));
            }
            screen
                .send(Output::Data(b"PS C:\\>".to_vec()))
                .await
                .unwrap();
        };
        tokio::join!(wait_for_windows_prompt(&mut io), console);
    }

    #[tokio::test]
    async fn output_waits_for_its_own_render_acknowledgement() {
        let sessions = TerminalSessions::default();
        let id = uuid::Uuid::new_v4().to_string();
        let (input, mut received_input) = mpsc::channel(1);
        let (output, receiver) = mpsc::channel(4);
        let cancel = CancellationToken::new();
        let io = SessionIo {
            input,
            output: receiver,
            cancel: cancel.clone(),
        };
        let (events, mut received_events) = mpsc::unbounded_channel();
        let channel = Channel::new(move |body| {
            if let tauri::ipc::InvokeResponseBody::Json(json) = body {
                events
                    .send(serde_json::from_str::<serde_json::Value>(&json).unwrap())
                    .unwrap();
            }
            Ok(())
        });
        sessions.attach(id.clone(), io, channel, ()).unwrap();
        sessions.write(&id, vec![8]).await.unwrap();
        assert!(matches!(received_input.recv().await, Some(Input::Data(data)) if data == [8]));
        for byte in [1, 2] {
            output.send(Output::Data(vec![byte])).await.unwrap();
        }
        let first = received_events.recv().await.unwrap();
        assert_eq!(first["data"], serde_json::json!([1]));
        sessions.ack(&id, 2).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(40), received_events.recv())
                .await
                .is_err()
        );
        sessions.ack(&id, 1).unwrap();
        let second = received_events.recv().await.unwrap();
        assert_eq!(second["sequence"], 2);
        sessions.close_all();
        assert_eq!(received_events.recv().await.unwrap()["type"], "exit");
        assert!(cancel.is_cancelled());
    }
}
