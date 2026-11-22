use super::{validate_size, Input, Output, SessionIo};
use anyhow::{Context, Result};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub(crate) fn spawn_pty(cols: u16, rows: u16, activity: impl Send + 'static) -> Result<SessionIo> {
    #[cfg(unix)]
    let mut command = CommandBuilder::new_default_prog();
    #[cfg(windows)]
    let mut command = {
        let system = std::env::var_os("SystemRoot").context("找不到 Windows 系统目录")?;
        let powershell = std::path::PathBuf::from(system)
            .join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
        CommandBuilder::new(powershell)
    };
    #[cfg(all(windows, test))]
    {
        // 回环测试使用真实 PowerShell，启动命令给无界面客户端一个确定的就绪信号。
        command.env("MESH_TEST_READY", super::tests::WINDOWS_SHELL_READY);
        command.args([
            "-NoLogo", "-NoProfile", "-NoExit", "-Command",
            "Remove-Module PSReadLine -ErrorAction SilentlyContinue; [Console]::WriteLine($env:MESH_TEST_READY)",
        ]);
    }
    command.env("TERM", "xterm-256color");
    open(command, cols, rows, activity)
}

fn size(cols: u16, rows: u16) -> Result<PtySize> {
    validate_size(cols, rows)?;
    Ok(PtySize {
        cols,
        rows,
        pixel_width: 0,
        pixel_height: 0,
    })
}

struct Shell {
    child: Box<dyn Child + Send + Sync>,
    master: Option<Box<dyn MasterPty + Send>>,
    reaped: bool,
    #[cfg(windows)]
    job: Option<std::os::windows::io::OwnedHandle>,
}

impl Shell {
    fn terminate(&mut self) {
        if self.reaped {
            return;
        }
        #[cfg(unix)]
        if let Some(pid) = self.child.process_id().map(|pid| pid as libc::pid_t) {
            // 前台作业可能位于自己的进程组。让交互 shell 处理挂断并通知其余受管作业。
            if let Some(group) = self
                .master
                .as_ref()
                .and_then(|master| master.process_group_leader())
            {
                if group > 1 && group != pid && group != unsafe { libc::getpgrp() } {
                    unsafe { libc::kill(-group, libc::SIGKILL) };
                }
            }
            unsafe { libc::kill(-pid, libc::SIGHUP) };
            // macOS 会等 PTY 缓冲排空才完成进程退出；先关闭主端才能回收被背压阻塞的子进程。
            drop(self.master.take());
            for _ in 0..5 {
                if matches!(self.child.try_wait(), Ok(Some(_))) {
                    self.reaped = true;
                    return;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            // 未回收的直属子进程持有该 PID，避免在它被复用后发信号。
            unsafe { libc::kill(-pid, libc::SIGKILL) };
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
        #[cfg(windows)]
        {
            drop(self.job.take());
            let _ = self.child.kill();
        }
        drop(self.master.take());
        let _ = self.child.wait();
        self.reaped = true;
    }
}

impl Drop for Shell {
    fn drop(&mut self) {
        self.terminate();
        #[cfg(windows)]
        drop(self.job.take());
    }
}

fn open(
    command: CommandBuilder,
    cols: u16,
    rows: u16,
    activity: impl Send + 'static,
) -> Result<SessionIo> {
    let pair = native_pty_system()
        .openpty(size(cols, rows)?)
        .context("无法创建伪终端")?;
    let (reader, writer) = platform::streams(pair.master.as_ref())?;
    let child = pair
        .slave
        .spawn_command(command)
        .context("无法启动 shell")?;
    drop(pair.slave);
    let mut shell = Shell {
        child,
        master: Some(pair.master),
        reaped: false,
        #[cfg(windows)]
        job: None,
    };
    #[cfg(windows)]
    {
        shell.job = Some(platform::process_job(shell.child.as_ref())?);
    }

    let (input_tx, input_rx) = mpsc::channel(8);
    let (output_tx, output_rx) = mpsc::channel(8);
    let cancel = CancellationToken::new();
    let stopping = cancel.clone();
    tokio::spawn(async move {
        let finished = CancellationToken::new();
        let mut reading = tokio::spawn(platform::read_output(
            reader,
            output_tx.clone(),
            stopping.clone(),
            finished.clone(),
        ));
        let mut read_completed = false;
        let reason = tokio::select! {
            biased;
            _ = stopping.cancelled() => "会话已关闭".to_string(),
            status = wait_shell(shell.child.as_mut()) => match status {
                Ok(status) => { shell.reaped = true; format!("Shell 已退出（{}）", status.exit_code()) },
                Err(error) => format!("读取 shell 状态失败：{error}"),
            },
            result = &mut reading => {
                read_completed = true;
                match result {
                    Ok(Ok(())) => "Shell 已关闭终端".to_string(),
                    Ok(Err(error)) => format!("读取终端失败：{error}"),
                    Err(error) => format!("终端读取任务失败：{error}"),
                }
            },
            result = handle_input(input_rx, writer, shell.master.as_deref_mut().expect("open PTY")) => {
                match result {
                    Ok(()) => "会话已关闭".to_string(),
                    Err(error) => format!("写入终端失败：{error}"),
                }
            }
        };
        // ClosePseudoConsole 可能等待其输出管道被读空；读任务仍在运行。
        finished.cancel();
        let _ = tokio::task::spawn_blocking(move || {
            drop(shell);
            drop(activity);
        })
        .await;
        if !read_completed {
            let _ = reading.await;
        }
        let _ = output_tx.send(Output::Exit { reason }).await;
    });
    Ok(SessionIo {
        input: input_tx,
        output: output_rx,
        cancel,
    })
}

async fn wait_shell(child: &mut (dyn Child + Send + Sync)) -> Result<portable_pty::ExitStatus> {
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn handle_input(
    mut input: mpsc::Receiver<Input>,
    mut writer: platform::Writer,
    master: &mut (dyn MasterPty + Send),
) -> Result<()> {
    while let Some(message) = input.recv().await {
        match message {
            Input::Data(data) => platform::write(&mut writer, data).await?,
            Input::Resize { cols, rows } => master.resize(size(cols, rows)?)?,
        }
    }
    Ok(())
}

#[cfg(unix)]
mod platform {
    use super::*;
    use std::{
        fs::File,
        io::{self, Read, Write},
        os::fd::{AsRawFd, BorrowedFd},
    };
    use tokio::io::{unix::AsyncFd, Interest};
    pub(super) type Writer = std::sync::Arc<AsyncFd<File>>;

    pub(super) fn streams(master: &dyn MasterPty) -> Result<(Writer, Writer)> {
        let fd = master.as_raw_fd().context("伪终端缺少文件描述符")?;
        let owned = unsafe { BorrowedFd::borrow_raw(fd) }.try_clone_to_owned()?;
        let flags = unsafe { libc::fcntl(owned.as_raw_fd(), libc::F_GETFL) };
        if flags == -1
            || unsafe { libc::fcntl(owned.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) }
                == -1
        {
            return Err(io::Error::last_os_error().into());
        }
        // File 独占已复制的描述符，直至 AsyncFd 注销时才会关闭。
        let io = std::sync::Arc::new(unsafe { AsyncFd::register(File::from(owned)) }?);
        Ok((io.clone(), io))
    }

    pub(super) async fn write(writer: &mut Writer, data: Vec<u8>) -> Result<()> {
        let mut bytes = data.as_slice();
        while !bytes.is_empty() {
            let n = writer
                .async_io(Interest::WRITABLE, |mut fd| fd.write(bytes))
                .await?;
            anyhow::ensure!(n != 0, "终端输入已关闭");
            bytes = &bytes[n..];
        }
        Ok(())
    }

    pub(super) async fn read_output(
        reader: Writer,
        output: mpsc::Sender<Output>,
        stop: CancellationToken,
        finished: CancellationToken,
    ) -> Result<()> {
        let mut buf = [0u8; 16 * 1024];
        loop {
            let n = tokio::select! {
                biased;
                _ = stop.cancelled() => return Ok(()),
                _ = finished.cancelled() => {
                    match reader.get_ref().read(&mut buf) {
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                        result => result,
                    }
                }
                result = reader.async_io(Interest::READABLE, |mut fd| fd.read(&mut buf)) => result,
            };
            let n = match n {
                Ok(0) => return Ok(()),
                Err(e) if e.raw_os_error() == Some(libc::EIO) => return Ok(()),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                other => other?,
            };
            tokio::select! {
                _ = stop.cancelled() => return Ok(()),
                result = output.send(Output::Data(buf[..n].to_vec())) => if result.is_err() { return Ok(()) },
            }
        }
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::io::{Read, Write};
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    type Reader = Box<dyn Read + Send>;
    pub(super) type Writer = Option<Box<dyn Write + Send>>;

    pub(super) fn streams(master: &dyn MasterPty) -> Result<(Reader, Writer)> {
        Ok((master.try_clone_reader()?, Some(master.take_writer()?)))
    }

    pub(super) fn process_job(child: &dyn Child) -> Result<OwnedHandle> {
        let process = child.as_raw_handle().context("无法获取终端进程句柄")?;
        let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw.is_null() {
            return Err(std::io::Error::last_os_error().into());
        }
        let job = unsafe { OwnedHandle::from_raw_handle(raw) };
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if unsafe {
            SetInformationJobObject(
                job.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        } == 0
            || unsafe { AssignProcessToJobObject(job.as_raw_handle(), process) } == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(job)
    }

    pub(super) async fn write(writer: &mut Writer, data: Vec<u8>) -> Result<()> {
        let mut pipe = writer.take().context("终端输入已关闭")?;
        *writer = Some(
            tokio::task::spawn_blocking(move || -> std::io::Result<_> {
                pipe.write_all(&data)?;
                Ok(pipe)
            })
            .await??,
        );
        Ok(())
    }

    pub(super) async fn read_output(
        mut reader: Reader,
        output: mpsc::Sender<Output>,
        stop: CancellationToken,
        _finished: CancellationToken,
    ) -> Result<()> {
        let runtime = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut bytes = [0u8; 16 * 1024];
            loop {
                let n = match reader.read(&mut bytes) {
                    Ok(0) => return Ok(()),
                    Ok(n) => n,
                    Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => return Ok(()),
                    Err(e) => return Err(e.into()),
                };
                // 即使界面已关闭也把 ConPTY 剩余的管道输出读完，确保系统可以释放控制台。
                if !stop.is_cancelled() {
                    runtime.block_on(async {
                        tokio::select! {
                            _ = stop.cancelled() => {},
                            _ = output.send(Output::Data(bytes[..n].to_vec())) => {},
                        }
                    });
                }
            }
        })
        .await?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn collect(io: &mut SessionIo) -> Vec<u8> {
        tokio::time::timeout(Duration::from_secs(12), async {
            let mut bytes = Vec::new();
            loop {
                match io.output.recv().await.expect("terminal exit event") {
                    Output::Data(data) => bytes.extend(data),
                    Output::Exit { .. } => return bytes,
                }
            }
        })
        .await
        .expect("shell must terminate")
    }

    #[tokio::test]
    async fn real_terminal_input_resize_and_final_output() {
        #[cfg(unix)]
        let command = CommandBuilder::new("/bin/sh");
        #[cfg(windows)]
        let command = {
            let mut cmd = CommandBuilder::new("cmd.exe");
            cmd.args(["/d", "/k", "echo", "%MESH_TEST_READY%"]);
            cmd.env(
                "MESH_TEST_READY",
                crate::terminal::tests::WINDOWS_SHELL_READY,
            );
            cmd.env("MESH_TEST_PREFIX", "MESH_");
            cmd
        };
        let mut io = open(command, 80, 24, ()).unwrap();
        #[cfg(windows)]
        crate::terminal::tests::wait_for_windows_shell(&mut io).await;
        io.input
            .send(Input::Resize { cols: 96, rows: 33 })
            .await
            .unwrap();
        #[cfg(unix)]
        let script = b"stty size; printf 'MESH_%s\\n' PTY_OK; exit 0\r".as_slice();
        #[cfg(windows)]
        let script = b"echo %MESH_TEST_PREFIX%PTY_OK&exit 0\r".as_slice();
        io.input.send(Input::Data(script.to_vec())).await.unwrap();
        let bytes = collect(&mut io).await;
        let output = String::from_utf8_lossy(&bytes);
        assert!(output.contains("MESH_PTY_OK"), "{output}");
        #[cfg(unix)]
        assert!(output.contains("33 96"), "{output}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn terminating_session_reaps_the_shell_when_output_backs_up() {
        let mut command = CommandBuilder::new("/bin/sh");
        command.args(["-c", "echo $$; exec yes"]);
        let mut io = open(command, 80, 24, ()).unwrap();
        let mut pid_line = Vec::new();
        while !pid_line.contains(&b'\n') {
            let Output::Data(bytes) =
                tokio::time::timeout(Duration::from_secs(5), io.output.recv())
                    .await
                    .unwrap()
                    .unwrap()
            else {
                panic!("missing child pid")
            };
            pid_line.extend(bytes);
        }
        let pid: i32 = std::str::from_utf8(pid_line.split(|b| *b == b'\n').next().unwrap())
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;
        io.cancel.cancel();
        collect(&mut io).await;
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }
}
