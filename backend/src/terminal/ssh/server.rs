use super::*;
use crate::runtime::NodeRuntime;
use crate::terminal::{spawn_pty, validate_size, Input, Output};
use axum::{
    extract::Request as HttpRequest,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use russh::{server as ssh, Channel, ChannelMsg};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{watch, Notify};
use tokio_util::io::InspectReader;

struct Host {
    service: Arc<SshService>,
    runtime: Arc<NodeRuntime>,
    connection: CancellationToken,
    grant: Option<CancellationToken>,
    started: watch::Sender<bool>,
    channel_opened: bool,
}

impl SshService {
    pub(crate) async fn upgrade(
        self: &Arc<Self>,
        mut req: HttpRequest,
        runtime: Arc<NodeRuntime>,
    ) -> Response {
        let headers = req.headers();
        if headers.contains_key(header::ORIGIN) {
            return StatusCode::FORBIDDEN.into_response();
        }
        if !headers
            .get(header::UPGRADE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.eq_ignore_ascii_case(UPGRADE))
            || !headers
                .get(header::CONNECTION)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|v| {
                    v.split(',')
                        .any(|s| s.trim().eq_ignore_ascii_case("upgrade"))
                })
        {
            return StatusCode::UPGRADE_REQUIRED.into_response();
        }
        if headers
            .get("x-meshshare-terminal-target")
            .and_then(|v| v.to_str().ok())
            != Some(&self.node_id)
        {
            return StatusCode::MISDIRECTED_REQUEST.into_response();
        }
        let Ok(slot) = self.sockets.clone().try_acquire_owned() else {
            return StatusCode::TOO_MANY_REQUESTS.into_response();
        };
        if self.key.get().is_none() && !self.restore_enabled {
            return StatusCode::FORBIDDEN.into_response();
        }
        let Ok(Ok(key)) = tokio::time::timeout(Duration::from_secs(8), self.identity()).await
        else {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        };
        let key = key.clone();
        let connection = {
            let access = self.access.lock();
            if self.stopped.is_cancelled() || !access.policy.enabled {
                return StatusCode::FORBIDDEN.into_response();
            }
            access.listener.child_token()
        };
        let upgrade = hyper::upgrade::on(&mut req);
        let service = self.clone();
        tokio::spawn(async move {
            let _slot = slot;
            let _close = connection.clone().drop_guard();
            let Ok(Ok(stream)) = tokio::time::timeout(Duration::from_secs(8), upgrade).await else {
                return;
            };
            let received = Arc::new(AtomicBool::new(false));
            let activity = received.clone();
            let stream = InspectReader::new(
                hyper_util::rt::TokioIo::new(stream),
                move |bytes: &[u8]| {
                    if !bytes.is_empty() {
                        activity.store(true, Ordering::Relaxed);
                    }
                },
            );
            let stream = cancellable_stream(stream, connection.clone());
            let config = ssh::Config {
                keys: vec![(*key).clone()],
                inactivity_timeout: None,
                keepalive_interval: None,
                ..Default::default()
            };
            let (started, mut state) = watch::channel(false);
            let host = Host {
                service,
                runtime,
                connection: connection.clone(),
                grant: None,
                started,
                channel_opened: false,
            };
            let Ok(Ok(mut session)) = tokio::time::timeout(
                Duration::from_secs(8),
                ssh::run_stream(Arc::new(config), stream, host),
            )
            .await
            else {
                return;
            };
            // 建立终端前限制握手和配对时间；空闲 shell 建立后只由心跳判断断线。
            tokio::select! {
                _ = &mut session => return,
                _ = connection.cancelled() => return,
                _ = tokio::time::sleep(PAIR_WINDOW * 2 + Duration::from_secs(10)) => return,
                opened = state.changed() => if opened.is_err() { return },
            }
            // 客户端定期发送加密的 SSH ping。只统计从客户端收到的流量，
            // 本机的持续输出不能让已经断线的 shell 保持活动。
            let silent = async {
                received.store(false, Ordering::Relaxed);
                let mut missed = 0;
                while missed < MISSED_HEARTBEATS {
                    tokio::time::sleep(KEEPALIVE).await;
                    if received.swap(false, Ordering::Relaxed) {
                        missed = 0;
                    } else {
                        missed += 1;
                    }
                }
            };
            tokio::select! { _ = session => (), _ = connection.cancelled() => (), _ = silent => () }
        });
        (
            StatusCode::SWITCHING_PROTOCOLS,
            [(header::CONNECTION, "upgrade"), (header::UPGRADE, UPGRADE)],
        )
            .into_response()
    }
}

impl ssh::Handler for Host {
    type Error = anyhow::Error;

    async fn auth_publickey(
        &mut self,
        user: &str,
        key: ssh::VerifiedPublicKey<'_>,
    ) -> Result<ssh::Auth> {
        if key.public_key.algorithm() != russh::keys::Algorithm::Ed25519 {
            return Ok(ssh::Auth::reject());
        }
        let pin = key.public_key.fingerprint(HashAlg::Sha256).to_string();
        match self
            .service
            .authorize(Direction::Incoming, user, &pin, &self.connection)
            .await
        {
            Ok(grant) => {
                bind_grant(grant.clone(), self.connection.clone());
                self.grant = Some(grant);
                Ok(ssh::Auth::Accept)
            }
            Err(_) => Ok(ssh::Auth::reject()),
        }
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<ssh::Msg>,
        reply: ssh::ChannelOpenHandle,
        session: &mut ssh::Session,
    ) -> Result<()> {
        let Some(grant) = self.grant.clone() else {
            return Ok(());
        };
        if self.channel_opened || grant.is_cancelled() {
            return Ok(());
        }
        self.channel_opened = true;
        let runtime = self.runtime.clone();
        let handle = session.handle();
        let connection = self.connection.clone();
        let started = self.started.clone();
        reply.accept().await;
        tokio::spawn(async move {
            tokio::select! {
                result = serve_channel(channel, handle, runtime, started) => {
                    // 每条连接只承载一个终端。通道异常退出时，断开传输也能唤醒等待结束通知的对端。
                    if result.is_err() {
                        connection.cancel();
                    }
                }
                _ = connection.cancelled() => (),
                _ = grant.cancelled() => (),
            }
        });
        Ok(())
    }
}

fn size(width: u32, height: u32) -> Result<(u16, u16)> {
    let (cols, rows) = (width.try_into()?, height.try_into()?);
    validate_size(cols, rows)?;
    Ok((cols, rows))
}

async fn serve_channel(
    mut channel: Channel<ssh::Msg>,
    handle: ssh::Handle,
    runtime: Arc<NodeRuntime>,
    started: watch::Sender<bool>,
) -> Result<()> {
    let id = channel.id();
    let dimensions = match channel.wait().await {
        Some(ChannelMsg::RequestPty {
            col_width,
            row_height,
            ..
        }) => size(col_width, row_height),
        _ => Err(anyhow!("缺少 PTY 请求")),
    };
    let Ok((cols, rows)) = dimensions else {
        let _ = handle.channel_failure(id).await;
        let _ = channel.close().await;
        return Ok(());
    };
    handle
        .channel_success(id)
        .await
        .map_err(|_| anyhow!("连接已断开"))?;
    if !matches!(channel.wait().await, Some(ChannelMsg::RequestShell { .. })) {
        let _ = handle.channel_failure(id).await;
        let _ = channel.close().await;
        return Ok(());
    }
    let creation = tokio::task::spawn_blocking(move || {
        let activity = runtime.reserve_terminal()?;
        spawn_pty(cols, rows, activity)
    })
    .await?;
    let Ok(io) = creation else {
        let _ = handle.channel_failure(id).await;
        let _ = channel.close().await;
        return Ok(());
    };
    handle
        .channel_success(id)
        .await
        .map_err(|_| anyhow!("连接已断开"))?;
    started.send_replace(true);
    relay_channel(channel, handle, io).await
}

async fn relay_channel(
    channel: Channel<ssh::Msg>,
    handle: ssh::Handle,
    mut io: crate::terminal::SessionIo,
) -> Result<()> {
    let id = channel.id();
    let (mut reader, writer) = channel.split();
    let output_ack = Notify::new();
    let receive = async {
        loop {
            let message = match reader.wait().await {
                Some(ChannelMsg::Data { data }) => Input::Data(data.to_vec()),
                Some(ChannelMsg::WindowChange {
                    col_width,
                    row_height,
                    ..
                }) => {
                    let (cols, rows) = size(col_width, row_height)?;
                    Input::Resize { cols, rows }
                }
                Some(ChannelMsg::WindowAdjusted { .. }) => continue,
                Some(ChannelMsg::ExtendedData {
                    ext: OUTPUT_BARRIER,
                    data,
                }) if data.as_ref() == b"\0" => {
                    output_ack.notify_one();
                    continue;
                }
                Some(ChannelMsg::Close | ChannelMsg::Eof) | None => {
                    return Ok::<_, anyhow::Error>(())
                }
                _ => {
                    handle
                        .channel_failure(id)
                        .await
                        .map_err(|_| anyhow!("连接已断开"))?;
                    continue;
                }
            };
            // shell 退出会先关闭输入，读任务随后才排完最后一屏。迟到的按键或尺寸
            // 修改直接丢弃，继续监听 SSH 关闭通知，同时让发送任务排空 PTY 输出。
            let _ = io.input.send(message).await;
        }
    };
    let transmit = async {
        // 在 russh 的通道队列填满前停读 PTY，使 SSH 仍能处理心跳和关闭消息。
        const BATCH_CHUNKS: usize = 16;
        let mut sent = 0;
        while let Some(msg) = io.output.recv().await {
            match msg {
                Output::Data(bytes) => {
                    if sent == BATCH_CHUNKS {
                        output_ack.notified().await;
                        sent = 0;
                    }
                    writer.data_bytes(bytes).await?;
                    sent += 1;
                    if sent == BATCH_CHUNKS {
                        writer
                            .extended_data_bytes(OUTPUT_BARRIER, b"\0".as_slice())
                            .await?;
                    }
                }
                Output::Exit { .. } => break,
            }
        }
        Ok::<_, anyhow::Error>(())
    };
    let result = tokio::select! { res = receive => res, res = transmit => res };
    drop(io);
    // 先释放 PTY，再给所有退出路径发送通道结束通知；对端已离线时不等待其回复。
    let closing = tokio::time::timeout(Duration::from_secs(3), async {
        let eof = writer.eof().await;
        let close = writer.close().await;
        eof.and(close)
    })
    .await;
    result?;
    closing.context("SSH 终端关闭超时")??;
    Ok(())
}

#[cfg(test)]
mod tests;
