#[cfg(feature = "gui")]
pub(crate) mod commands;
#[cfg(not(target_os = "android"))]
mod pty;
pub(crate) mod ssh;

use anyhow::{ensure, Result};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[cfg(not(target_os = "android"))]
pub(crate) use pty::spawn_pty;

pub(crate) enum Input {
    Data(Vec<u8>),
    Resize { cols: u16, rows: u16 },
}

pub(crate) enum Output {
    Data(Vec<u8>),
    Exit {
        // 界面显示结束原因；入站 SSH 只需要 EOF。
        #[cfg_attr(not(feature = "gui"), allow(dead_code))]
        reason: String,
    },
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

#[cfg(feature = "gui")]
mod attached;
#[cfg(feature = "gui")]
pub(crate) use attached::{TerminalEvent, TerminalSessions};

#[cfg(all(feature = "gui", test, not(target_os = "android")))]
mod tests {
    use super::*;
    use std::time::Duration;
    use tauri::ipc::Channel;

    pub(super) const WINDOWS_SHELL_READY: &str = "MESH_SHELL_READY";

    // ConPTY 继承光标时会查询终端。持续应答，直到测试 shell 的启动命令输出标记。
    pub(super) async fn wait_for_windows_shell(io: &mut SessionIo) {
        let mut seen = Vec::new();
        let ready = tokio::time::timeout(Duration::from_secs(60), async {
            while let Some(message) = io.output.recv().await {
                let bytes = match message {
                    Output::Data(bytes) => bytes,
                    Output::Exit { reason } => panic!("shell exited during startup: {reason}"),
                };
                let mut started = false;
                for byte in bytes {
                    seen.push(byte);
                    if seen.ends_with(WINDOWS_SHELL_READY.as_bytes()) {
                        started = true;
                    }
                    let reply: &[u8] = if seen.ends_with(b"\x1b[6n") {
                        b"\x1b[1;1R"
                    } else if seen.ends_with(b"\x1b[c") || seen.ends_with(b"\x1b[0c") {
                        b"\x1b[?1;2c"
                    } else {
                        continue;
                    };
                    io.input.send(Input::Data(reply.to_vec())).await.unwrap();
                }
                if started {
                    return;
                }
            }
            panic!("terminal output closed during startup");
        })
        .await;
        assert!(
            ready.is_ok(),
            "waiting for {WINDOWS_SHELL_READY}: {:?}",
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
            for chunk in [
                b"\x1b[".as_slice(),
                b"6n\x1b[?9001h\x1b[?1004h\x1b]0;build > powershell.exe\x07\x1b",
                b"[c",
            ] {
                screen.send(Output::Data(chunk.to_vec())).await.unwrap();
            }
            for reply in [b"\x1b[1;1R".as_slice(), b"\x1b[?1;2c"] {
                assert!(matches!(replies.recv().await, Some(Input::Data(bytes)) if bytes == reply));
            }
            for chunk in [
                b"Windows PowerShell 5.1\r\nMESH_SHELL_".as_slice(),
                b"READY\r\n\x1b[0c",
            ] {
                screen.send(Output::Data(chunk.to_vec())).await.unwrap();
            }
            assert!(
                matches!(replies.recv().await, Some(Input::Data(bytes)) if bytes == b"\x1b[?1;2c")
            );
        };
        tokio::join!(
            async move { wait_for_windows_shell(&mut io).await },
            console
        );
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
