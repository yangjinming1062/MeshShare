use super::*;
use anyhow::Context;
use russh::keys::ssh_key::{LineEnding, SshSig};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::{ErrorKind, Read},
    path::Path,
};

const MAX_POLICY_BYTES: usize = 1024 * 1024;
const NAMESPACE: &str = "meshshare-terminal-policy-v2";
const LEGACY_NAMESPACE: &str = "meshshare-terminal-policy-v1";
const FILE: &str = "terminal-access.json";

#[derive(Serialize, Deserialize)]
struct SignedPolicy<P> {
    policy: P,
    signature: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    enabled: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PairedPolicy {
    enabled: bool,
    incoming: BTreeMap<String, String>,
    outgoing: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Stored {
    Current(SignedPolicy<Policy>),
    Paired(SignedPolicy<PairedPolicy>),
}

pub(super) fn load(
    dir: &Path,
    node_id: &str,
    enabling: bool,
) -> Result<Option<(PrivateKey, bool)>> {
    load_with_key(dir, enabling, |has_policy| {
        let entry = keyring::Entry::new("com.meshshare.terminal.v1", node_id)
            .context("无法访问系统凭据库")?;
        host_key(&entry, has_policy)
    })
}

fn load_with_key(
    dir: &Path,
    enabling: bool,
    get_key: impl FnOnce(bool) -> Result<PrivateKey>,
) -> Result<Option<(PrivateKey, bool)>> {
    let policy = match read_policy(&dir.join(FILE)) {
        Ok(bytes) => Some(serde_json::from_slice::<Stored>(&bytes).context("终端授权文件损坏")?),
        Err(e) if e.kind() == ErrorKind::NotFound => None,
        Err(e) => return Err(e).context("无法读取终端授权"),
    };
    // 关闭状态可以直接保持。开启接入或迁移文件之前，必须用同一份策略核验签名。
    let stored_enabled = match &policy {
        Some(Stored::Current(signed)) => signed.policy.enabled,
        Some(Stored::Paired(signed)) => signed.policy.enabled,
        None => false,
    };
    if !enabling && !stored_enabled {
        return Ok(None);
    }
    let key = get_key(policy.is_some())?;
    let enabled = match policy {
        Some(stored) => restore(dir, &key, stored)?,
        None => false,
    };
    Ok(Some((key, enabled)))
}

fn host_key(entry: &keyring::Entry, has_policy: bool) -> Result<PrivateKey> {
    let key = match entry.get_secret() {
        Ok(bytes) => PrivateKey::from_bytes(&bytes).context("系统凭据库中的终端密钥损坏")?,
        Err(keyring::Error::NoEntry) => {
            ensure!(!has_policy, "终端授权文件存在但系统凭据库中缺少对应密钥");
            let key = PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519)?;
            entry
                .set_secret(&key.to_bytes()?)
                .context("无法保存终端密钥，请解锁系统凭据库")?;
            key
        }
        Err(error) => return Err(error).context("无法读取终端密钥，请解锁系统凭据库"),
    };
    ensure!(
        key.algorithm() == Algorithm::Ed25519,
        "系统凭据库中的终端密钥类型不匹配"
    );
    Ok(key)
}

fn read_policy(path: &Path) -> std::io::Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.len() > MAX_POLICY_BYTES as u64 {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "无效的终端授权文件",
        ));
    }
    let file = fs::File::open(path)?;
    let mut bytes = Vec::new();
    file.take(MAX_POLICY_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_POLICY_BYTES {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "终端授权文件过大",
        ));
    }
    Ok(bytes)
}

fn verify<P: Serialize>(key: &PrivateKey, signed: &SignedPolicy<P>, namespace: &str) -> Result<()> {
    let signature = SshSig::from_pem(&signed.signature)?;
    key.public_key()
        .verify(namespace, &serde_json::to_vec(&signed.policy)?, &signature)
        .context("终端授权签名无效")
}

fn restore(dir: &Path, key: &PrivateKey, stored: Stored) -> Result<bool> {
    match stored {
        Stored::Current(signed) => {
            verify(key, &signed, NAMESPACE)?;
            Ok(signed.policy.enabled)
        }
        Stored::Paired(signed) => {
            verify(key, &signed, LEGACY_NAMESPACE)?;
            save(dir, key, signed.policy.enabled)?;
            Ok(signed.policy.enabled)
        }
    }
}

pub(super) fn save(dir: &Path, key: &PrivateKey, enabled: bool) -> Result<()> {
    let policy = Policy { enabled };
    let signature = key
        .sign(NAMESPACE, HashAlg::Sha256, &serde_json::to_vec(&policy)?)?
        .to_pem(LineEnding::LF)?;
    let bytes = serde_json::to_vec(&SignedPolicy { policy, signature })?;
    crate::fsutil::atomic_write_private(&dir.join(FILE), &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_saved_disabled_switch_restarts_without_a_system_credential() {
        let account = format!("meshshare-disabled-{}", uuid::Uuid::new_v4());
        let dir = std::env::temp_dir().join(&account);
        fs::create_dir_all(&dir).unwrap();
        let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
        save(&dir, &key, false).unwrap();
        let original = fs::read(dir.join(FILE)).unwrap();

        let service = Arc::new(SshService::new(&dir, account).unwrap());
        assert!(!service.host_settings(None).await.unwrap());
        assert!(!service.host_settings(Some(false)).await.unwrap());
        assert!(service.host_fingerprint().is_none());
        assert!(service.host.accept().is_none());
        assert_eq!(fs::read(dir.join(FILE)).unwrap(), original);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn verified_migration_preserves_both_switch_values_and_rejects_tampering() {
        let dir =
            std::env::temp_dir().join(format!("meshshare-ssh-policy-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join(FILE);
        let entry =
            keyring::Entry::new_with_credential(Box::new(keyring::mock::MockCredential::default()));
        let load =
            |enabling| load_with_key(&dir, enabling, |has_policy| host_key(&entry, has_policy));
        let restore_bytes =
            |key: &PrivateKey, bytes: &[u8]| restore(&dir, key, serde_json::from_slice(bytes)?);
        let read_disabled = || {
            assert!(load_with_key(&dir, false, |_| panic!(
                "closed policies must not open the vault"
            ))
            .unwrap()
            .is_none());
        };
        read_disabled();
        let (key, initial) = load(true).unwrap().unwrap();
        assert!(!initial);
        let other = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519).unwrap();
        for enabled in [true, false] {
            // 旧版本实际签名的字段和值，独立于迁移读取器的结构定义。
            let policy = serde_json::json!({
                "enabled": enabled,
                "incoming": {"computer": "old-client-pin"},
                "outgoing": {"target": "old-host-pin"}
            });
            let signature = key
                .sign(
                    LEGACY_NAMESPACE,
                    HashAlg::Sha256,
                    &serde_json::to_vec(&policy).unwrap(),
                )
                .unwrap()
                .to_pem(LineEnding::LF)
                .unwrap();
            let legacy = serde_json::to_string(&SignedPolicy { policy, signature }).unwrap();
            fs::write(&file, &legacy).unwrap();
            if !enabled {
                read_disabled();
            }
            assert!(restore_bytes(&other, legacy.as_bytes()).is_err());
            assert!(
                restore_bytes(&key, legacy.replace("old-host-pin", "changed").as_bytes()).is_err()
            );
            assert_eq!(fs::read(&file).unwrap(), legacy.as_bytes());
            // 开启的旧策略在启动时迁移，关闭的旧策略到用户主动开启时才验证并迁移。
            assert_eq!(load(!enabled).unwrap().unwrap().1, enabled);
            let migrated = fs::read(&file).unwrap();
            let saved: serde_json::Value = serde_json::from_slice(&migrated).unwrap();
            assert_eq!(saved["policy"], serde_json::json!({"enabled": enabled}));
            if !enabled {
                read_disabled();
            }
            assert_eq!(load(true).unwrap().unwrap().1, enabled);
            assert!(restore_bytes(&other, &migrated).is_err());
            let mut forged = saved;
            forged["policy"]["enabled"] = (!enabled).into();
            let forged = serde_json::to_vec(&forged).unwrap();
            fs::write(&file, &forged).unwrap();
            if enabled {
                read_disabled();
            } else {
                assert!(load(false).is_err());
            }
            assert!(load(true).is_err());
            assert_eq!(fs::read(&file).unwrap(), forged);
        }
        entry.delete_credential().unwrap();
        assert!(load(true).is_err());
        assert!(matches!(entry.get_secret(), Err(keyring::Error::NoEntry)));
        fs::write(&file, b"{invalid").unwrap();
        assert!(load_with_key(&dir, false, |_| panic!("invalid policy opened the vault")).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    #[ignore = "requires an unlocked native desktop credential store"]
    async fn native_vault_restores_host_key_and_signed_terminal_switch() {
        struct Fixture {
            account: String,
            dir: std::path::PathBuf,
        }
        impl Drop for Fixture {
            fn drop(&mut self) {
                if let Ok(entry) = keyring::Entry::new("com.meshshare.terminal.v1", &self.account) {
                    let _ = entry.delete_credential();
                }
                let _ = fs::remove_dir_all(&self.dir);
            }
        }
        let account = format!("temporary-vault-test-{}", uuid::Uuid::new_v4());
        let fixture = Fixture {
            dir: std::env::temp_dir().join(&account),
            account,
        };
        fs::create_dir_all(&fixture.dir).unwrap();
        let start = || Arc::new(SshService::new(&fixture.dir, fixture.account.clone()).unwrap());
        let first = start();
        assert!(first.host_settings(Some(true)).await.unwrap());
        let restored = start();
        assert!(restored.host_settings(None).await.unwrap());
        assert_eq!(first.host_fingerprint(), restored.host_fingerprint());
        assert_ne!(first.client_fingerprint(), restored.client_fingerprint());
        assert!(!restored.host_settings(Some(false)).await.unwrap());
        assert!(!start().host_settings(None).await.unwrap());
        keyring::Entry::new("com.meshshare.terminal.v1", &fixture.account)
            .unwrap()
            .delete_credential()
            .unwrap();
        let lost = start();
        assert!(!lost.host_settings(None).await.unwrap());
        assert!(lost.host_settings(Some(true)).await.is_err());
        assert!(lost.host_fingerprint().is_none());
    }
}
