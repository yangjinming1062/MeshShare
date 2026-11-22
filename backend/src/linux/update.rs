//! Linux 系统包更新：通过 polkit 请求本机授权后调用 dpkg/rpm 安装更新包。
//!
//! 只有二进制被打包进 .deb/.rpm 时才会返回系统包类型；其它平台和介质返回 None，
//! 更新仍由 tauri updater 自己安装。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use tauri::utils::{config::BundleType, platform::bundle_type};

/// 系统包安装的结果。授权未完成不是失败，调用方需要保留待更新版本。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Installed,
    NeedsAuthorization(&'static str),
    Failed(String),
}

pub(crate) struct SystemPackage {
    installer: &'static str,
    flag: &'static str,
    extension: &'static str,
}

impl SystemPackage {
    fn from_bundle(bundle: Option<BundleType>) -> Option<Self> {
        let (installer, flag, extension) = match bundle? {
            BundleType::Deb => ("dpkg", "-i", "deb"),
            BundleType::Rpm => ("rpm", "-U", "rpm"),
            _ => return None,
        };
        Some(Self {
            installer,
            flag,
            extension,
        })
    }
}

pub(crate) fn system_package() -> Option<SystemPackage> {
    SystemPackage::from_bundle(bundle_type())
}

/// 请求系统授权并安装更新包；授权未完成时返回待授权而不是安装失败。
pub(crate) async fn install(kind: SystemPackage, bytes: Vec<u8>) -> Outcome {
    tokio::task::spawn_blocking(move || install_blocking(kind, bytes))
        .await
        .unwrap_or_else(|error| Outcome::Failed(format!("系统包安装任务失败：{error}")))
}

fn install_blocking(kind: SystemPackage, bytes: Vec<u8>) -> Outcome {
    let Some(installer) = system_binary(kind.installer) else {
        return Outcome::Failed(format!(
            "未找到 {}，无法安装系统包更新；请手动安装下载的安装包",
            kind.installer
        ));
    };
    let Some(pkexec) = system_binary("pkexec") else {
        return Outcome::Failed(
            "未找到 pkexec（polkit），无法请求系统授权安装更新；请手动安装下载的安装包".to_string(),
        );
    };
    let package_path = match write_package(&bytes, kind.extension) {
        Ok(path) => path,
        Err(error) => return Outcome::Failed(format!("写入更新包失败：{error}")),
    };
    let output = Command::new(pkexec)
        .arg(installer)
        .arg(kind.flag)
        .arg(&package_path)
        .stdin(Stdio::null())
        .output();
    let _ = std::fs::remove_file(&package_path);
    let output = match output {
        Ok(output) => output,
        Err(error) => return Outcome::Failed(format!("启动系统授权安装失败：{error}")),
    };
    package_outcome(output.status.code(), &output.stderr, &output.stdout)
}

fn package_outcome(code: Option<i32>, stderr: &[u8], stdout: &[u8]) -> Outcome {
    match code {
        Some(0) => Outcome::Installed,
        Some(126) => Outcome::NeedsAuthorization("已取消系统授权；更新保留，稍后自动重试"),
        Some(127) => Outcome::NeedsAuthorization("未获得系统授权；请在本机登录桌面后重试"),
        _ => {
            let code = code.map_or_else(|| "被信号终止".to_string(), |code| code.to_string());
            Outcome::Failed(format!(
                "系统包安装失败（退出码 {code}）：{}",
                summarize_output(stderr, stdout)
            ))
        }
    }
}

fn system_binary(name: &str) -> Option<PathBuf> {
    ["/usr/bin", "/bin"]
        .into_iter()
        .map(|dir| Path::new(dir).join(name))
        .find(|path| path.is_file())
}

fn write_package(bytes: &[u8], extension: &str) -> std::io::Result<PathBuf> {
    let path = std::env::temp_dir().join(format!(
        "meshshare-update-{}.{extension}",
        uuid::Uuid::new_v4()
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path)?;
    if let Err(error) = file.write_all(bytes) {
        let _ = std::fs::remove_file(&path);
        return Err(error);
    }
    Ok(path)
}

fn summarize_output(stderr: &[u8], stdout: &[u8]) -> String {
    let stderr_lines = text_lines(stderr);
    let lines = if stderr_lines.is_empty() {
        text_lines(stdout)
    } else {
        stderr_lines
    };
    if lines.is_empty() {
        return "安装程序没有返回错误信息".to_string();
    }
    let start = lines.len().saturating_sub(3);
    truncate_chars(&lines[start..].join(" / "), 300)
}

fn text_lines(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

fn truncate_chars(text: &str, limit: usize) -> String {
    let mut truncated: String = text.chars().take(limit).collect();
    if text.chars().count() > limit {
        truncated.push('…');
    }
    truncated
}

#[cfg(test)]
mod tests {
    use super::{package_outcome, summarize_output, Outcome, SystemPackage};
    use tauri::utils::config::BundleType;

    #[test]
    fn deb_and_rpm_map_to_the_matching_system_installer() {
        let deb = SystemPackage::from_bundle(Some(BundleType::Deb)).expect("deb");
        assert_eq!(deb.flag, "-i");
        assert_eq!(deb.extension, "deb");
        assert_eq!(deb.installer, "dpkg");
        let rpm = SystemPackage::from_bundle(Some(BundleType::Rpm)).expect("rpm");
        assert_eq!(rpm.flag, "-U");
        assert_eq!(rpm.extension, "rpm");
        assert_eq!(rpm.installer, "rpm");
        assert!(SystemPackage::from_bundle(Some(BundleType::AppImage)).is_none());
        assert!(SystemPackage::from_bundle(Some(BundleType::Msi)).is_none());
        assert!(SystemPackage::from_bundle(None).is_none());
    }

    #[test]
    fn authorization_exit_codes_are_not_install_failures() {
        let outcome = |code| package_outcome(code, b"", b"");
        assert_eq!(outcome(Some(0)), Outcome::Installed);
        assert!(
            matches!(outcome(Some(126)), Outcome::NeedsAuthorization(reason) if reason.contains("已取消"))
        );
        assert!(
            matches!(outcome(Some(127)), Outcome::NeedsAuthorization(reason) if reason.contains("未获得"))
        );
        assert!(matches!(outcome(Some(1)), Outcome::Failed(reason) if reason.contains("退出码 1")));
        assert!(matches!(outcome(None), Outcome::Failed(reason) if reason.contains("被信号终止")));
    }

    #[test]
    fn output_summary_keeps_the_last_lines_and_truncates() {
        assert_eq!(summarize_output(b"", b""), "安装程序没有返回错误信息");
        assert_eq!(
            summarize_output(b"dpkg: error: a\n\n dpkg: error: b \n", b"stdout"),
            "dpkg: error: a / dpkg: error: b"
        );
        assert_eq!(
            summarize_output(b"", b"stdout 1\nstdout 2"),
            "stdout 1 / stdout 2"
        );
        let long = "x".repeat(400);
        let summary = summarize_output(long.as_bytes(), b"");
        assert_eq!(summary.chars().count(), 301);
        assert!(summary.ends_with('…'));
    }
}
