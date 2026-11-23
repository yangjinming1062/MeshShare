use crate::{control::ControlPlane, headless_service, listeners, runtime::NodeProfile};
use anyhow::{bail, ensure, Context, Result};
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use tokio::signal::unix::{signal, SignalKind};

const VERSION: &str = match option_env!("MESHSHARE_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};
const STATE_DIR: &str = "/var/lib/meshshare-headless";
const WEB_DIR: &str = "/usr/share/meshshare-headless/web";
pub(super) const PORT_FILE: &str = "/run/meshshare-headless/port";

fn publish_port(port: u16) -> Result<()> {
    let path = Path::new(PORT_FILE);
    let mut temporary = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    writeln!(temporary, "{port}")?;
    // SSH 用户需要读取监听端口。服务数据与密钥仍沿用单元的私有 umask。
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o644))?;
    temporary.persist(path)?;
    Ok(())
}

pub async fn run_headless() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "serve".to_owned());
    ensure!(args.next().is_none(), "多余的命令参数");
    match command.as_str() {
        "--version" | "-V" => {
            println!("meshshare-headless {VERSION}");
            Ok(())
        }
        "--help" | "-h" => {
            println!("meshshare-headless [serve|status|update|--version]");
            Ok(())
        }
        "serve" => serve().await,
        "status" | "update" | "helper" => headless_service::cli(&command).await,
        _ => bail!("未知命令：{command}"),
    }
}

async fn serve() -> Result<()> {
    // The application provides interactive terminals. Never start the network node as root.
    ensure!(
        unsafe { libc::geteuid() } != 0,
        "请使用专用非 root 账户运行节点"
    );
    let data_dir =
        std::env::var_os("MESHSHARE_DATA_DIR").map_or_else(|| STATE_DIR.into(), PathBuf::from);
    ensure!(data_dir.is_absolute(), "状态目录必须是绝对路径");
    ensure!(
        Path::new(WEB_DIR).join("index.html").is_file(),
        "未找到临时共享网页资源"
    );
    fs::create_dir_all(&data_dir)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(data_dir.join("service.lock"))?;
    lock.try_lock()
        .context("数据目录已被另一节点占用或不可加锁")?;
    let port = match std::env::var("MESHSHARE_PORT") {
        Ok(value) => value.parse().context("MESHSHARE_PORT 无效")?,
        Err(std::env::VarError::NotPresent) => crate::DEFAULT_NODE_PORT,
        Err(error) => return Err(error.into()),
    };
    let mut terminate = signal(SignalKind::terminate())?;
    let (node, _control, listener_task) = listeners::start(
        data_dir,
        WEB_DIR.into(),
        VERSION.into(),
        port,
        NodeProfile::Headless,
        |runtime| {
            let control = ControlPlane::runtime_only(runtime);
            control.start_headless();
            control
        },
    )
    .await?;
    publish_port(node.node_info().port)?;
    crate::start_workers(&node);
    eprintln!(
        "MeshShare headless {VERSION} 正在监听端口 {}",
        node.node_info().port
    );
    tokio::select! {
        _ = terminate.recv() => (),
        interrupted = tokio::signal::ctrl_c() => interrupted?,
    }
    node.stopping.store(true, Ordering::SeqCst);
    node.ssh.shutdown();
    let drain_terminals = tokio::task::spawn_blocking(move || node.wait_for_terminal_shutdown());
    tokio::try_join!(drain_terminals, listener_task)?;
    fs::remove_file(PORT_FILE).context("清理服务端口记录失败")?;
    Ok(())
}
