//! Linux 系统包更新：通过 polkit 请求本机授权后调用 dpkg/rpm 安装更新包。
//!
//! 只有二进制被打包进 .deb/.rpm 时才会返回系统包类型；其它平台和介质返回 None，
//! 更新仍由 tauri updater 自己安装。

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use tauri::utils::{config::BundleType, platform::bundle_type};

/// 系统包安装的结果。授权未完成不是失败，调用方需要保留待更新版本。
pub(crate) enum Outcome {
    Installed,
    NeedsAuthorization(&'static str),
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SystemPackageKind {
    Deb,
    Rpm,
}

impl SystemPackageKind {
    fn from_bundle(bundle: Option<BundleType>) -> Option<Self> {
        match bundle? {
            BundleType::Deb => Some(Self::Deb),
            BundleType::Rpm => Some(Self::Rpm),
            _ => None,
        }
    }

    fn installer_candidates(self) -> &'static [&'static str] {
        match self {
            Self::Deb => &["/usr/bin/dpkg", "/bin/dpkg"],
            Self::Rpm => &["/usr/bin/rpm", "/bin/rpm"],
        }
    }

    fn mode(self) -> &'static str {
        match self {
            Self::Deb => "-i",
            Self::Rpm => "-U",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Deb => "deb",
            Self::Rpm => "rpm",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Deb => "dpkg",
            Self::Rpm => "rpm",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PackageStatus {
    Installed,
    Dismissed,
    NotAuthorized,
    Failed,
}

fn classify_exit_code(code: Option<i32>) -> PackageStatus {
    match code {
        Some(0) => PackageStatus::Installed,
        Some(126) => PackageStatus::Dismissed,
        Some(127) => PackageStatus::NotAuthorized,
        _ => PackageStatus::Failed,
    }
}

pub(crate) fn system_package() -> Option<SystemPackageKind> {
    SystemPackageKind::from_bundle(bundle_type())
}

/// 请求系统授权并安装更新包；授权未完成时返回待授权而不是安装失败。
pub(crate) async fn install(kind: SystemPackageKind, bytes: Vec<u8>) -> Outcome {
    tokio::task::spawn_blocking(move || install_blocking(kind, bytes))
        .await
        .unwrap_or_else(|error| Outcome::Failed(format!("系统包安装任务失败：{error}")))
}

fn install_blocking(kind: SystemPackageKind, bytes: Vec<u8>) -> Outcome {
    let Some(installer) = first_file(kind.installer_candidates()) else {
        return Outcome::Failed(format!(
            "未找到 {}，无法安装系统包更新；请手动安装下载的安装包",
            kind.label()
        ));
    };
    let Some(pkexec) = first_file(&["/usr/bin/pkexec", "/bin/pkexec"]) else {
        return Outcome::Failed(
            "未找到 pkexec（polkit），无法请求系统授权安装更新；请手动安装下载的安装包".to_string(),
        );
    };
    let package_path = match write_package(&bytes, kind.extension()) {
        Ok(path) => path,
        Err(error) => return Outcome::Failed(format!("写入更新包失败：{error}")),
    };
    let output = Command::new(pkexec)
        .arg(installer)
        .arg(kind.mode())
        .arg(&package_path)
        .stdin(Stdio::null())
        .output();
    let _ = std::fs::remove_file(&package_path);
    let output = match output {
        Ok(output) => output,
        Err(error) => return Outcome::Failed(format!("启动系统授权安装失败：{error}")),
    };
    match classify_exit_code(output.status.code()) {
        PackageStatus::Installed => Outcome::Installed,
        PackageStatus::Dismissed => {
            Outcome::NeedsAuthorization("已取消系统授权；更新保留，稍后自动重试")
        }
        PackageStatus::NotAuthorized => {
            Outcome::NeedsAuthorization("未获得系统授权；请在本机登录桌面后重试")
        }
        PackageStatus::Failed => {
            let code = output
                .status
                .code()
                .map_or_else(|| "被信号终止".to_string(), |code| code.to_string());
            Outcome::Failed(format!(
                "系统包安装失败（退出码 {code}）：{}",
                summarize_output(&output.stderr, &output.stdout)
            ))
        }
    }
}

fn first_file(candidates: &[&str]) -> Option<PathBuf> {
    candidates
        .iter()
        .map(PathBuf::from)
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
    use super::{classify_exit_code, summarize_output, PackageStatus, SystemPackageKind};
    use tauri::utils::config::BundleType;

    #[test]
    fn deb_and_rpm_map_to_the_matching_system_installer() {
        let deb = SystemPackageKind::from_bundle(Some(BundleType::Deb)).expect("deb");
        assert_eq!(deb.mode(), "-i");
        assert_eq!(deb.extension(), "deb");
        assert_eq!(deb.label(), "dpkg");
        let rpm = SystemPackageKind::from_bundle(Some(BundleType::Rpm)).expect("rpm");
        assert_eq!(rpm.mode(), "-U");
        assert_eq!(rpm.extension(), "rpm");
        assert_eq!(rpm.label(), "rpm");
        assert!(SystemPackageKind::from_bundle(Some(BundleType::AppImage)).is_none());
        assert!(SystemPackageKind::from_bundle(Some(BundleType::Msi)).is_none());
        assert!(SystemPackageKind::from_bundle(None).is_none());
    }

    #[test]
    fn authorization_exit_codes_are_not_install_failures() {
        assert_eq!(classify_exit_code(Some(0)), PackageStatus::Installed);
        assert_eq!(classify_exit_code(Some(126)), PackageStatus::Dismissed);
        assert_eq!(classify_exit_code(Some(127)), PackageStatus::NotAuthorized);
        assert_eq!(classify_exit_code(Some(1)), PackageStatus::Failed);
        assert_eq!(classify_exit_code(None), PackageStatus::Failed);
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
