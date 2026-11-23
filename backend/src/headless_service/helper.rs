//! systemd Accept=yes 为每次调用建立独立进程。只有该 unit 的 MainPID 能请求维护操作。
use super::release;
use anyhow::{bail, ensure, Context, Result};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::Write;
use std::os::fd::AsFd;
use std::os::unix::fs::OpenOptionsExt;
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::process::Command;

const UNIT: &str = "meshshare-headless.service";
const SOCKET: &str = "/run/meshshare-headless-control.sock";
const PACKAGE: &str = "meshshare-headless";
const HEADER_LIMIT: usize = 16384;

#[derive(Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Action {
    Enable,
    Disable,
    Restart,
    Install {
        version: Version,
        signature: String,
        size: usize,
    },
}

fn ensure_success(output: Output, action: &str) -> Result<String> {
    ensure!(
        output.status.success(),
        "{action} 失败（{}）：{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
            .chars()
            .take(512)
            .collect::<String>()
    );
    Ok(String::from_utf8(output.stdout)?.trim().into())
}

async fn short_command(mut cmd: Command, action: &str) -> Result<String> {
    let output = tokio::time::timeout(
        Duration::from_secs(15),
        cmd.stdin(Stdio::null()).kill_on_drop(true).output(),
    )
    .await
    .context("系统命令超时")??;
    ensure_success(output, action)
}

fn systemctl(operation: &str) -> Command {
    let mut cmd = Command::new("/usr/bin/systemctl");
    cmd.args(["--no-ask-password", operation, UNIT]);
    cmd
}

pub(super) async fn enabled() -> Result<bool> {
    let mut cmd = systemctl("is-enabled");
    // 开机启用状态存储在磁盘上。即使服务账户没有系统 D-Bus 会话也可读取。
    cmd.arg("--root=/");
    let output =
        tokio::time::timeout(Duration::from_secs(5), cmd.kill_on_drop(true).output()).await??;
    match String::from_utf8_lossy(&output.stdout).trim() {
        "enabled" => Ok(true),
        "disabled" => Ok(false),
        other => bail!(
            "无法读取 systemd 开机自启状态：{other} {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
    }
}

// 前缀限定消息长度后才分配，不使用无限制的 read_line/read_to_end。
async fn read_frame(stream: &mut (impl AsyncRead + Unpin)) -> Result<Vec<u8>> {
    let len = stream.read_u32().await? as usize;
    ensure!(len <= HEADER_LIMIT, "本机控制消息过长");
    let mut bytes = vec![0; len];
    stream.read_exact(&mut bytes).await?;
    Ok(bytes)
}

async fn write_frame(stream: &mut UnixStream, bytes: &[u8]) -> Result<()> {
    ensure!(bytes.len() <= HEADER_LIMIT, "本机控制消息过长");
    stream.write_u32(bytes.len() as u32).await?;
    stream.write_all(bytes).await?;
    Ok(())
}

pub(super) async fn invoke(action: &Action, bytes: &[u8]) -> Result<()> {
    let timeout = if matches!(action, Action::Install { .. }) {
        420
    } else {
        25
    };
    tokio::time::timeout(Duration::from_secs(timeout), async {
        let mut stream = UnixStream::connect(SOCKET)
            .await
            .context("连接 systemd 控制助手")?;
        write_frame(&mut stream, &serde_json::to_vec(action)?).await?;
        stream.write_all(bytes).await?;
        stream.shutdown().await?;
        let reply = read_frame(&mut stream).await?;
        if !reply.is_empty() {
            bail!("{}", String::from_utf8_lossy(&reply));
        }
        Ok(())
    })
    .await
    .context("systemd 控制请求超时")?
}

async fn main_pid() -> Result<u32> {
    let mut cmd = systemctl("show");
    cmd.args(["--property=MainPID", "--value"]);
    Ok(short_command(cmd, "查询服务进程").await?.parse()?)
}

pub(super) async fn run() -> Result<()> {
    // stdin 已由 systemd 连接到调用者；复制描述符，避免接管进程的标准流。
    let fd = std::io::stdin().as_fd().try_clone_to_owned()?;
    let std_stream = std::os::unix::net::UnixStream::from(fd);
    std_stream.set_nonblocking(true)?;
    let mut stream = UnixStream::from_std(std_stream)?;
    let result = handle(&mut stream).await;
    let error = result
        .as_ref()
        .err()
        .map(|e| format!("{e:#}"))
        .unwrap_or_default();
    // 安装器独立于旧节点，旧进程在成功升级时会先于结果通知退出。
    let _ = write_frame(&mut stream, error.as_bytes()).await;
    result
}

async fn handle(stream: &mut UnixStream) -> Result<()> {
    let uid = unsafe { libc::geteuid() };
    ensure!(uid == 0, "控制助手需要由系统服务运行");
    let pid = main_pid().await?;
    ensure!(
        pid > 1 && stream.peer_cred()?.pid() == Some(pid as i32),
        "只接受来自当前 MeshShare 服务进程的操作"
    );
    let action: Action = serde_json::from_slice(
        &tokio::time::timeout(Duration::from_secs(5), read_frame(stream)).await??,
    )?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open("/run/meshshare-headless-control.lock")?;
    lock.try_lock().context("已有系统维护操作在执行")?;
    match action {
        Action::Enable | Action::Disable => {
            let operation = if matches!(action, Action::Enable) {
                "enable"
            } else {
                "disable"
            };
            short_command(systemctl(operation), "修改开机自启").await?;
        }
        Action::Restart => {
            // 先登记延迟任务，将确认留给仍在运行的旧 HTTP 节点。
            let mut cmd = Command::new("/usr/bin/systemd-run");
            cmd.args([
                "--collect",
                "--on-active=1s",
                "--timer-property=AccuracySec=100ms",
                "/usr/bin/systemctl",
                "restart",
                UNIT,
            ]);
            short_command(cmd, "提交重启任务").await?;
        }
        Action::Install {
            version,
            signature,
            size,
        } => {
            ensure!(size > 0 && size <= release::MAX_DEB, "更新包大小无效");
            let mut bytes = vec![0; size];
            tokio::time::timeout(Duration::from_secs(60), stream.read_exact(&mut bytes)).await??;
            install(pid, &version, &signature, &bytes).await?;
        }
    }
    Ok(())
}

async fn installed_version() -> Result<Version> {
    let mut cmd = Command::new("/usr/bin/dpkg-query");
    cmd.args(["-W", "-f=${Version}", PACKAGE]);
    Ok(Version::parse(
        &short_command(cmd, "查询已安装版本").await?,
    )?)
}

async fn install(
    submitting_pid: u32,
    version: &Version,
    signature: &str,
    bytes: &[u8],
) -> Result<()> {
    ensure!(
        version.pre.is_empty()
            && version.build.is_empty()
            && version > &installed_version().await?,
        "只能安装更新的稳定版"
    );
    release::verify(bytes, signature, version)?;
    // 安装只使用助手自己创建的文件。服务账户无法在验签后替换包。
    let mut deb = tempfile::Builder::new()
        .prefix("meshshare-headless-")
        .suffix(".deb")
        .tempfile_in("/var/cache")?;
    deb.write_all(bytes)?;
    deb.as_file().sync_all()?;
    let path = deb.path();
    let mut cmd = Command::new("/usr/bin/dpkg-deb");
    cmd.arg("--show")
        .arg("--showformat=${Package}\t${Architecture}\t${Version}")
        .arg(path);
    ensure!(
        short_command(cmd, "读取包元数据").await? == format!("{PACKAGE}\tamd64\t{version}"),
        "包名、架构或版本不匹配"
    );
    let previous_node = super::local_node().await?.2.node_id;
    // 下载与验签期间原服务若被管理员替换，新节点尚未锁住传输。
    ensure!(
        main_pid().await? == submitting_pid,
        "提交更新的服务进程已退出，请重新检查更新"
    );
    let mut cmd = Command::new("/usr/bin/dpkg");
    cmd.args(["--force-confold", "--install"])
        .arg(path)
        .env("DEBIAN_FRONTEND", "noninteractive")
        .stdin(Stdio::null())
        .stdout(Stdio::from(std::io::stderr()))
        .stderr(Stdio::inherit());
    let status = tokio::time::timeout(Duration::from_secs(240), cmd.kill_on_drop(true).status())
        .await
        .context("dpkg 安装超时，请检查软件包状态")??;
    ensure!(
        status.success(),
        "dpkg 安装失败（{status}），详情见 journal"
    );
    ensure!(
        installed_version().await? == *version,
        "dpkg 安装后版本不匹配"
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if main_pid()
            .await
            .is_ok_and(|pid| pid > 1 && pid != submitting_pid)
            && super::local_node().await.is_ok_and(|(_, _, node)| {
                node.node_id == previous_node && node.version == version.to_string()
            })
        {
            eprintln!("MeshShare headless {version} 正在运行");
            return Ok(());
        }
        ensure!(
            Instant::now() < deadline,
            "安装完成，但未检测到新版服务进程，请通过 SSH 检查 journal"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
