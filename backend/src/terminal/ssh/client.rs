use super::*;
use crate::terminal::{Input, Output, SessionIo};
use russh::keys::{Algorithm, PrivateKeyWithHashAlg, PublicKeyOrCertificate};
use russh::{client as ssh, Channel, ChannelMsg};
use tokio::sync::mpsc;

struct Controller {
    service: Arc<SshService>,
    peer: String,
    connection: CancellationToken,
}

impl ssh::Handler for Controller {
    type Error = anyhow::Error;
    async fn check_server_key(&mut self, server_key: &PublicKeyOrCertificate) -> Result<bool> {
        let PublicKeyOrCertificate::PublicKey { key, .. } = server_key else {
            return Ok(false);
        };
        ensure!(
            key.algorithm() == Algorithm::Ed25519,
            "目标终端主机密钥类型不匹配"
        );
        let pin = key.fingerprint(HashAlg::Sha256).to_string();
        let grant = self
            .service
            .authorize(Direction::Outgoing, &self.peer, &pin, &self.connection)
            .await?;
        bind_grant(grant, self.connection.clone());
        Ok(true)
    }
}

async fn upgrade(peer: &str, bases: Vec<String>) -> Result<reqwest::Upgraded> {
    let client = crate::runtime::node_http_client()
        .http1_only()
        .connect_timeout(Duration::from_secs(3))
        .build()?;
    let mut last = anyhow!("无法访问目标节点端口");
    for base in bases {
        let Some(base) = crate::runtime::normalize_peer_http_base(&base) else {
            continue;
        };
        let res = tokio::time::timeout(
            Duration::from_secs(4),
            client
                .get(format!("{base}{ROUTE}"))
                .header("x-meshshare-terminal-target", peer)
                .header("Connection", "upgrade")
                .header("Upgrade", UPGRADE)
                .send(),
        )
        .await;
        let res = match res {
            Ok(Ok(res)) => res,
            Ok(Err(error)) => {
                last = error.into();
                continue;
            }
            Err(_) => {
                last = anyhow!("连接目标终端超时");
                continue;
            }
        };
        match res.status() {
            reqwest::StatusCode::SWITCHING_PROTOCOLS => {
                ensure!(
                    res.headers()
                        .get("Upgrade")
                        .and_then(|v| v.to_str().ok())
                        .is_some_and(|v| v.eq_ignore_ascii_case(UPGRADE)),
                    "目标节点协议不匹配"
                );
                return Ok(res.upgrade().await?);
            }
            reqwest::StatusCode::FORBIDDEN => bail!("目标机尚未开启远程终端"),
            reqwest::StatusCode::SERVICE_UNAVAILABLE => {
                bail!("目标机的终端凭据不可用，请在目标机检查设置")
            }
            reqwest::StatusCode::TOO_MANY_REQUESTS => bail!("目标机的终端连接已满"),
            status => last = anyhow!("目标节点返回 {status}"),
        }
    }
    Err(last)
}

async fn accepted(channel: &mut Channel<ssh::Msg>) -> Result<()> {
    match tokio::time::timeout(Duration::from_secs(10), channel.wait()).await? {
        Some(ChannelMsg::Success) => Ok(()),
        _ => bail!("目标机未能创建交互终端，请检查目标 shell 或更新安装状态"),
    }
}

impl SshService {
    pub(crate) async fn connect(
        self: &Arc<Self>,
        peer_id: String,
        bases: Vec<String>,
        cols: u16,
        rows: u16,
    ) -> Result<SessionIo> {
        crate::terminal::validate_size(cols, rows)?;
        let key = self.identity().await?.clone();
        let connection = self.stopped.child_token();
        let transport_guard = connection.clone().drop_guard();
        let upgraded =
            tokio::time::timeout(Duration::from_secs(15), upgrade(&peer_id, bases)).await??;
        let stream = cancellable_stream(upgraded, connection.clone());
        let handler = Controller {
            service: self.clone(),
            peer: peer_id,
            connection: connection.clone(),
        };
        let config = ssh::Config {
            // russh 的配置心跳从握手开始。等待用户核对指纹期间只能发送认证消息。
            keepalive_interval: None,
            inactivity_timeout: None,
            ..Default::default()
        };
        let opening = async {
            let mut client = ssh::connect_stream(Arc::new(config), stream, handler).await?;
            let auth = client
                .authenticate_publickey(&self.node_id, PrivateKeyWithHashAlg::new(key, None))
                .await?;
            ensure!(
                auth.success(),
                "目标机拒绝终端授权，请在目标机打开配对窗口并核对设备指纹"
            );
            let mut channel = client.channel_open_session().await?;
            channel
                .request_pty(true, "xterm-256color", cols.into(), rows.into(), 0, 0, &[])
                .await?;
            accepted(&mut channel).await?;
            channel.request_shell(true).await?;
            accepted(&mut channel).await?;
            Ok::<_, anyhow::Error>((client, channel))
        };
        let (client, channel) =
            tokio::time::timeout(PAIR_WINDOW * 2 + Duration::from_secs(15), opening).await??;
        let (mut reader, writer) = channel.split();
        let (input, mut inputs) = mpsc::channel(16);
        let (output, outputs) = mpsc::channel(16);
        let cancel = CancellationToken::new();
        let closed = cancel.clone();
        tokio::spawn(async move {
            let heartbeat = async {
                loop {
                    tokio::time::sleep(KEEPALIVE).await;
                    if !matches!(
                        tokio::time::timeout(KEEPALIVE * MISSED_HEARTBEATS, client.send_ping())
                            .await,
                        Ok(Ok(()))
                    ) || client.is_closed()
                    {
                        return;
                    }
                }
            };
            let receive = async {
                loop {
                    match reader.wait().await {
                        Some(ChannelMsg::Data { data }) => output
                            .send(Output::Data(data.to_vec()))
                            .await
                            .map_err(|_| anyhow!("终端界面已关闭"))?,
                        Some(ChannelMsg::ExtendedData {
                            ext: OUTPUT_BARRIER,
                            data,
                        }) if data.as_ref() == b"\0" => {
                            writer.extended_data_bytes(OUTPUT_BARRIER, data).await?;
                        }
                        Some(ChannelMsg::Eof | ChannelMsg::Close) => {
                            return Ok::<_, anyhow::Error>("远程终端已结束")
                        }
                        None => return Ok("远程连接已断开"),
                        _ => (),
                    }
                }
            };
            let send = async {
                while let Some(msg) = inputs.recv().await {
                    match msg {
                        Input::Data(data) => writer.data_bytes(data).await?,
                        Input::Resize { cols, rows } => {
                            writer.window_change(cols.into(), rows.into(), 0, 0).await?
                        }
                    }
                }
                Ok::<_, anyhow::Error>("终端已关闭")
            };
            let result = tokio::select! {
                _ = closed.cancelled() => return,
                _ = connection.cancelled() => Ok("远程连接已关闭"),
                _ = heartbeat => Ok("远程终端心跳超时或连接已关闭"),
                reason = receive => reason,
                reason = send => reason,
            };
            // 界面消费退出通知可能受背压阻塞，先释放 SSH 链路。
            drop(transport_guard);
            let reason = result
                .map(str::to_owned)
                .unwrap_or_else(|e| format!("远程连接已断开：{e}"));
            let _ = output.send(Output::Exit { reason }).await;
        });
        Ok(SessionIo {
            input,
            output: outputs,
            cancel,
        })
    }
}
