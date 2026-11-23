use anyhow::{ensure, Context, Result};
use russh::keys::{ssh_key::SshSig, Algorithm, PrivateKey};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{ErrorKind, Read},
    path::Path,
};

const FILENAME: &str = "terminal-access.json";
const MAX_BYTES: u64 = 1024 * 1024;
const NAMESPACE: &str = "meshshare-terminal-policy-v2";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    enabled: bool,
}

#[derive(Deserialize)]
struct SignedPolicy {
    policy: Policy,
    signature: String,
}

pub(crate) struct LegacyPolicy {
    pub(crate) digest: String,
    grant: Option<SshSig>,
}

impl LegacyPolicy {
    pub(crate) fn read(dir: &Path) -> Result<Option<Self>> {
        let path = dir.join(FILENAME);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).context("无法检查旧版终端授权"),
        };
        ensure!(
            metadata.is_file() && metadata.len() <= MAX_BYTES,
            "旧版终端授权文件无效"
        );
        let mut bytes = Vec::new();
        fs::File::open(path)
            .context("无法打开旧版终端授权文件")?
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .context("无法读取旧版终端授权文件")?;
        ensure!(bytes.len() as u64 <= MAX_BYTES, "旧版终端授权文件过大");
        // 只有 v2 开关的开启授权与新版范围相同。配对清单、关闭和损坏的记录都须在本机确认。
        let grant = serde_json::from_slice::<SignedPolicy>(&bytes)
            .ok()
            .filter(|stored| stored.policy.enabled)
            .and_then(|stored| SshSig::from_pem(&stored.signature).ok());
        Ok(Some(Self {
            digest: format!("{:x}", Sha256::digest(&bytes)),
            grant,
        }))
    }

    pub(crate) fn was_enabled(&self, node_id: &str) -> Result<bool> {
        self.verify_with(|| Ok(keyring::Entry::new("com.meshshare.terminal.v1", node_id)?))
    }

    fn verify_with(&self, open_entry: impl FnOnce() -> Result<keyring::Entry>) -> Result<bool> {
        let Some(signature) = &self.grant else {
            return Ok(false);
        };
        let key = PrivateKey::from_bytes(&open_entry()?.get_secret()?)?;
        ensure!(
            key.algorithm() == Algorithm::Ed25519,
            "旧版终端密钥类型不匹配"
        );
        key.public_key()
            .verify(NAMESPACE, br#"{"enabled":true}"#, signature)
            .context("旧版终端授权签名无效")?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::keys::{ssh_key::LineEnding, HashAlg};

    #[test]
    fn only_the_vault_signed_unrestricted_grant_can_migrate_automatically() {
        let dir = std::env::temp_dir().join(format!("meshshare-legacy-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(FILENAME);
        let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
        let other = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
        let credential = |key: &PrivateKey| -> Result<_> {
            let entry = keyring::Entry::new_with_credential(
                Box::<keyring::mock::MockCredential>::default(),
            );
            entry.set_secret(&key.to_bytes()?)?;
            Ok(entry)
        };
        let signed = |policy: serde_json::Value, namespace| {
            let signature = key
                .sign(
                    namespace,
                    HashAlg::Sha256,
                    &serde_json::to_vec(&policy).unwrap(),
                )
                .unwrap()
                .to_pem(LineEnding::LF)
                .unwrap();
            serde_json::json!({ "policy": policy, "signature": signature })
        };
        let read = |doc: serde_json::Value| {
            fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
            LegacyPolicy::read(&dir).unwrap().unwrap()
        };
        let grant = signed(serde_json::json!({"enabled": true}), NAMESPACE);
        let enabled = read(grant.clone());
        let start = || crate::runtime::NodeRuntime::new(dir.clone(), 0, "test".into()).unwrap();
        let runtime = start();
        assert!(runtime.ssh.host_fingerprint().is_none());
        assert!(enabled.verify_with(|| credential(&key)).unwrap());
        assert!(enabled.verify_with(|| credential(&other)).is_err());
        assert!(enabled
            .verify_with(|| anyhow::bail!("vault unavailable"))
            .is_err());
        let disabled = signed(serde_json::json!({"enabled": false}), NAMESPACE);
        let mut tampered = disabled.clone();
        tampered["policy"]["enabled"] = true.into();
        assert!(read(tampered).verify_with(|| credential(&key)).is_err());
        assert!(runtime
            .accept_terminal_migration(Some(&enabled.digest))
            .is_err());
        assert!(runtime.ssh.host_fingerprint().is_none());
        let paired = signed(
            serde_json::json!({"enabled": true, "incoming": {}, "outgoing": {}}),
            "meshshare-terminal-policy-v1",
        );
        for doc in [disabled, paired, serde_json::json!({})] {
            assert!(!read(doc)
                .verify_with(|| panic!("restricted records must not open the vault"))
                .unwrap());
        }
        let restored = read(grant);
        assert!(restored.verify_with(|| credential(&key)).unwrap());
        runtime
            .accept_terminal_migration(Some(&restored.digest))
            .unwrap();
        assert!(start().ssh.host_fingerprint().is_some());
        fs::remove_dir_all(dir).unwrap();
    }
}
