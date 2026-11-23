//! 只处理正式发布清单；最终安装者也要独立校验签名和 Debian 元数据。
use anyhow::{ensure, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use minisign_verify::{PublicKey, Signature};
use semver::Version;
use serde::Deserialize;
use std::collections::HashMap;

pub(super) fn endpoint() -> Result<String> {
    let config: serde_json::Value = serde_json::from_str(include_str!("../../tauri.conf.json"))?;
    let url = config
        .pointer("/plugins/updater/endpoints/0")
        .and_then(|v| v.as_str())
        .context("缺少内置稳定版地址")?;
    ensure!(
        url.starts_with("https://") && url.ends_with("/latest/download/latest.json"),
        "内置稳定版地址无效"
    );
    Ok(url.into())
}
pub(super) const MAX_DEB: usize = 256 * 1024 * 1024;
pub(super) const MAX_MANIFEST: usize = 1024 * 1024;

#[derive(Deserialize)]
struct Manifest {
    version: Version,
    platforms: HashMap<String, Asset>,
}

#[derive(Deserialize)]
pub(super) struct Asset {
    pub url: String,
    pub signature: String,
}

pub(super) fn candidate(bytes: &[u8], current: &Version) -> Result<Option<(Version, Asset)>> {
    let mut manifest: Manifest = serde_json::from_slice(bytes).context("解析稳定版更新清单")?;
    let version = manifest.version;
    ensure!(
        version.pre.is_empty() && version.build.is_empty(),
        "需要稳定版版本号"
    );
    if &version <= current {
        return Ok(None);
    }
    let Some(asset) = manifest.platforms.remove("linux-x86_64-headless-deb") else {
        // 第一份 headless 正式产物晋级前，最新稳定版只包含 GUI 安装包。
        return Ok(None);
    };
    let endpoint = endpoint()?;
    let repo = endpoint
        .strip_suffix("/latest/download/latest.json")
        .context("无效的发布地址")?;
    ensure!(
        asset.url == format!("{repo}/download/v{version}/meshshare-headless_{version}_amd64.deb"),
        "headless 包的发布地址与版本不匹配"
    );
    ensure!(asset.signature.len() <= 8192, "更新签名过长");
    Ok(Some((version, asset)))
}

fn decoded(value: &str) -> Result<String> {
    Ok(String::from_utf8(STANDARD.decode(value.trim())?)?)
}

pub(super) fn verify(bytes: &[u8], encoded_signature: &str, version: &Version) -> Result<()> {
    let config: serde_json::Value = serde_json::from_str(include_str!("../../tauri.conf.json"))?;
    let encoded_key = config
        .pointer("/plugins/updater/pubkey")
        .and_then(|key| key.as_str())
        .context("缺少内置更新公钥")?;
    verify_with_key(bytes, encoded_signature, version, encoded_key)
}

fn verify_with_key(
    bytes: &[u8],
    encoded_signature: &str,
    version: &Version,
    encoded_key: &str,
) -> Result<()> {
    let key = PublicKey::decode(&decoded(encoded_key)?)?;
    let signature = Signature::decode(&decoded(encoded_signature)?)?;
    key.verify(bytes, &signature, true)
        .context("更新包签名不匹配")?;
    let wanted_version = format!("version:{version}");
    let wanted_file = format!("file:meshshare-headless_{version}_amd64.deb");
    let comment = signature.trusted_comment();
    ensure!(
        comment.split('\t').any(|v| v == wanted_version)
            && comment.split('\t').any(|v| v == wanted_file),
        "签名中的版本或文件名不匹配"
    );
    Ok(())
}

pub(super) async fn download(
    client: &reqwest::Client,
    url: &str,
    limit: usize,
    progress: impl FnMut(u64, Option<u64>),
) -> Result<Vec<u8>> {
    let response = client.get(url).send().await?.error_for_status()?;
    bounded_body(response, limit, progress).await
}

pub(super) async fn bounded_body(
    mut response: reqwest::Response,
    limit: usize,
    mut progress: impl FnMut(u64, Option<u64>),
) -> Result<Vec<u8>> {
    let total = response.content_length();
    ensure!(
        total.is_none_or(|n| n <= limit as u64),
        "更新文件超过大小限制"
    );
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(chunk.len() <= limit - bytes.len(), "更新文件超过大小限制");
        bytes.extend_from_slice(&chunk);
        progress(bytes.len() as u64, total);
    }
    ensure!(
        total.is_none_or(|n| n == bytes.len() as u64),
        "下载长度不匹配"
    );
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signed_bytes_and_signed_version_must_both_match() {
        // Tauri signer 生成的临时测试密钥的公钥与签名。
        const PUBLIC: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IEY0RkY4NjdEN0U5RUVEMzcKUldRMzdaNStmWWIvOUJnR204VnZraHovR3F3UndqU1NZQlRGeTM5Q090T3BwQ3dvYU1TL2J6b3gK";
        const SIGNATURE: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVRMzdaNStmWWIvOUFucUhuREJsUE5rUXJSUUZVMmYzVk5ibWc3S2VvRFRNVC9rOTQyMkxEUTVSWWcrZ0JjMEQ4WHVjUDRZTGEzRTltY2dyM1FqNHRmWEtpUEJ5VFdFOFE0PQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzk1Mzk4NjE5CWZpbGU6bWVzaHNoYXJlLWhlYWRsZXNzXzIuMy40X2FtZDY0LmRlYgl2ZXJzaW9uOjIuMy40Cm10ekRjM1dVcU5zcy8wT0hISEZ4bmRENWxjb1kwZzJRUytqVDZueVc5aWxhbk9OSkdLdUZJTFRtakFZY2VEZVU4bm9NK2s0WlRUNDdIUzJCZElmbkFRPT0K";
        let version = Version::new(2, 3, 4);
        assert!(verify_with_key(b"build fixture", SIGNATURE, &version, PUBLIC).is_ok());
        assert!(verify_with_key(b"bad fixture", SIGNATURE, &version, PUBLIC).is_err());
        assert!(
            verify_with_key(b"build fixture", SIGNATURE, &Version::new(2, 3, 5), PUBLIC).is_err()
        );
        assert!(verify(b"build fixture", SIGNATURE, &version).is_err());
    }

    #[test]
    fn picks_only_the_versioned_headless_deb() {
        let current = Version::new(1, 0, 0);
        let manifest = |url: &str| {
            serde_json::json!({"version":"2.0.0","platforms":{"linux-x86_64-headless-deb":{"url":url,"signature":"abc"}}}).to_string()
        };
        let endpoint = endpoint().unwrap();
        let repo = endpoint
            .strip_suffix("/latest/download/latest.json")
            .unwrap();
        let correct = format!("{repo}/download/v2.0.0/meshshare-headless_2.0.0_amd64.deb");
        assert!(candidate(manifest(&correct).as_bytes(), &current)
            .unwrap()
            .is_some());
        assert!(
            candidate(manifest(&correct).as_bytes(), &Version::new(2, 0, 0))
                .unwrap()
                .is_none()
        );
        assert!(candidate(
            manifest(&correct.replace("meshshare-headless", "MeshShare")).as_bytes(),
            &current
        )
        .is_err());
    }
}
