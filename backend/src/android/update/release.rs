use anyhow::{bail, ensure, Result};
use semver::Version;
use serde::{Deserialize, Serialize};

const MAX_APK_SIZE: u64 = 512 * 1024 * 1024;

#[derive(Debug, Deserialize)]
pub(super) struct Manifest {
    version: Version,
    android: Artifact,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub(super) struct Release {
    pub version: Version,
    pub apk: Artifact,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub(super) struct Artifact {
    pub size: u64,
    pub sha256: String,
    pub version_code: u64,
}

impl Manifest {
    pub fn select(self, current: &Version, installed_code: u64) -> Result<Option<Release>> {
        let release = Release {
            version: self.version,
            apk: self.android,
        };
        Ok(release
            .is_newer_than(current, installed_code)?
            .then_some(release))
    }
}

impl Release {
    pub fn is_newer_than(&self, current: &Version, installed_code: u64) -> Result<bool> {
        let version = &self.version;
        ensure!(
            version.pre.is_empty() && version.build.is_empty(),
            "更新版本不是稳定版"
        );
        if version <= current {
            return Ok(false);
        }
        let code = version
            .major
            .checked_mul(1_000_000)
            .and_then(|major| major.checked_add(version.minor.checked_mul(1_000)?))
            .and_then(|major_minor| major_minor.checked_add(version.patch));
        if version.minor >= 1000
            || version.patch >= 1000
            || self.apk.version_code <= installed_code
            || self.apk.version_code > 2_100_000_000
            || code != Some(self.apk.version_code)
        {
            bail!("更新包的 Android versionCode 无效");
        }
        ensure!(
            (1..=MAX_APK_SIZE).contains(&self.apk.size)
                && self.apk.sha256.len() == 64
                && self.apk.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "更新包的大小或 SHA-256 无效"
        );
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn android_metadata_preserves_the_existing_desktop_manifest() {
        let manifest = json!({
            "version": "1.2.3",
            "notes": "release notes",
            "platforms": {
                "windows-x86_64-nsis": {"url": "https://example.org/setup.exe", "signature": "sig"},
            },
            "android": {"version_code": 1002003, "size": 42, "sha256": "ab".repeat(32)},
        });
        #[cfg(not(target_os = "android"))]
        serde_json::from_value::<tauri_plugin_updater::RemoteRelease>(manifest.clone()).unwrap();
        let release = serde_json::from_value::<Manifest>(manifest)
            .unwrap()
            .select(&Version::new(1, 2, 2), 1002002)
            .unwrap()
            .unwrap();
        assert_eq!(release.apk.version_code, 1_002_003);
        assert_eq!(release.version, Version::new(1, 2, 3));
    }

    #[test]
    fn android_selects_only_supported_newer_releases() {
        let installed = Version::new(1, 2, 3);
        assert!(serde_json::from_value::<Manifest>(json!({"version": "1.2.4"})).is_err());
        let current = serde_json::from_value::<Manifest>(json!({
            "version": "1.2.3",
            "android": {"version_code": 1002003, "size": 42, "sha256": "ab".repeat(32)},
        }))
        .unwrap();
        assert!(current.select(&installed, 1_002_003).unwrap().is_none());

        for (version, code, size, hash) in [
            ("1.2.4-rc.1", 1_002_004, 42, "ab".repeat(32)),
            ("1.2.4", 1_002_003, 42, "ab".repeat(32)),
            ("1.2.4", 1_002_005, 42, "ab".repeat(32)),
            ("1.2.4", 1_002_004, 0, "ab".repeat(32)),
            ("1.2.4", 1_002_004, 42, "invalid".to_string()),
        ] {
            let manifest = serde_json::from_value::<Manifest>(json!({
                "version": version,
                "android": {"version_code": code, "size": size, "sha256": hash},
            }))
            .unwrap();
            assert!(manifest.select(&installed, 1_002_003).is_err());
        }
    }
}
