use super::*;
use russh::keys::ssh_key::{LineEnding, SshSig};
use std::{
    fs,
    io::{ErrorKind, Read},
    path::Path,
};

const MAX_POLICY_BYTES: usize = 1024 * 1024;
const NAMESPACE: &str = "meshshare-terminal-policy-v1";
pub(super) const FILE: &str = "terminal-access.json";

#[derive(Serialize, Deserialize)]
struct SignedPolicy {
    policy: Policy,
    signature: String,
}

// 文件传输可以在用户选定的位置新建文件。只有系统凭据库中的设备私钥
// 签发的授权表才能启用终端或授权新设备。
pub(super) fn load(dir: &Path, node_id: &str) -> Result<(PrivateKey, Policy)> {
    let entry =
        keyring::Entry::new("com.meshshare.terminal.v1", node_id).context("无法访问系统凭据库")?;
    let policy_file = read_policy(&dir.join(FILE));
    let key = match entry.get_secret() {
        Ok(bytes) => PrivateKey::from_bytes(&bytes).context("系统凭据库中的终端密钥损坏")?,
        Err(keyring::Error::NoEntry) => {
            ensure!(
                matches!(&policy_file,Err(e) if e.kind() == ErrorKind::NotFound),
                "终端授权文件存在但系统凭据库中缺少对应密钥"
            );
            let key = PrivateKey::random(&mut rand::rng(), russh::keys::Algorithm::Ed25519)?;
            entry
                .set_secret(&key.to_bytes()?)
                .context("无法保存终端密钥，请解锁系统凭据库")?;
            key
        }
        Err(error) => return Err(error).context("无法读取终端密钥，请解锁系统凭据库"),
    };
    let policy = match policy_file {
        Ok(bytes) => decode(&key, &bytes)?,
        Err(e) if e.kind() == ErrorKind::NotFound => Policy::default(),
        Err(e) => return Err(e).context("无法读取终端授权"),
    };
    Ok((key, policy))
}

// 此提示只决定是否需访问系统凭据库。认证前仍须用本机私钥对应的公钥验证整个授权表。
pub(super) fn stored_host_enabled(dir: &Path) -> bool {
    let Some(signed) = read_policy(&dir.join(FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<SignedPolicy>(&bytes).ok())
    else {
        return false;
    };
    signed.policy.enabled
        && SshSig::from_pem(signed.signature)
            .is_ok_and(|signature| signature.namespace() == NAMESPACE)
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

pub(super) fn decode(key: &PrivateKey, bytes: &[u8]) -> Result<Policy> {
    let signed: SignedPolicy = serde_json::from_slice(bytes).context("终端授权文件损坏")?;
    let signature = SshSig::from_pem(signed.signature)?;
    key.public_key()
        .verify(NAMESPACE, &serde_json::to_vec(&signed.policy)?, &signature)
        .context("终端授权签名无效")?;
    Ok(signed.policy)
}

pub(super) fn save(dir: &Path, key: &PrivateKey, policy: &Policy) -> Result<()> {
    let signature = key
        .sign(NAMESPACE, HashAlg::Sha256, &serde_json::to_vec(policy)?)?
        .to_pem(LineEnding::LF)?;
    let bytes = serde_json::to_vec(&SignedPolicy {
        policy: policy.clone(),
        signature,
    })?;
    ensure!(bytes.len() <= MAX_POLICY_BYTES, "终端授权列表已满");
    crate::fsutil::atomic_write_private(&dir.join(FILE), &bytes)
}
