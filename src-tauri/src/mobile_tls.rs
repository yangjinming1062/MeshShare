use anyhow::{Context, Result};
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, Issuer, KeyPair,
};
use sha2::{Digest, Sha256};
use std::fs;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

const TLS_DIRECTORY: &str = "mobile-tls";
const CA_CERTIFICATE: &str = "ca.pem";
const CA_KEY: &str = "ca-key.pem";
const SERVER_CERTIFICATE: &str = "server.pem";
const SERVER_KEY: &str = "server-key.pem";
const SERVER_ADDRESSES: &str = "server-addresses.json";

#[derive(Debug, Clone)]
pub struct MobileTlsMaterial {
    pub ca_certificate_path: PathBuf,
    pub server_certificate_path: PathBuf,
    pub server_key_path: PathBuf,
    pub fingerprint: String,
}

pub fn ensure(data_dir: &Path, addresses: &[Ipv4Addr]) -> Result<MobileTlsMaterial> {
    let directory = data_dir.join(TLS_DIRECTORY);
    fs::create_dir_all(&directory).context("创建手机 HTTPS 证书目录失败")?;
    let ca_path = directory.join(CA_CERTIFICATE);
    let ca_key_path = directory.join(CA_KEY);
    let server_path = directory.join(SERVER_CERTIFICATE);
    let server_key_path = directory.join(SERVER_KEY);
    let addresses_path = directory.join(SERVER_ADDRESSES);

    let (ca_pem, ca_key_pem) = if ca_path.is_file() && ca_key_path.is_file() {
        (
            fs::read_to_string(&ca_path)?,
            fs::read_to_string(&ca_key_path)?,
        )
    } else {
        let mut params = CertificateParams::default();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        let mut distinguished_name = DistinguishedName::new();
        distinguished_name.push(DnType::CommonName, "MeshShare Local CA");
        params.distinguished_name = distinguished_name;
        let key = KeyPair::generate()?;
        let certificate = params.self_signed(&key)?;
        let certificate_pem = certificate.pem();
        let key_pem = key.serialize_pem();
        atomic_write(&ca_path, certificate_pem.as_bytes())?;
        atomic_write(&ca_key_path, key_pem.as_bytes())?;
        (certificate_pem, key_pem)
    };

    let address_values = addresses
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let previous_addresses = fs::read_to_string(&addresses_path)
        .ok()
        .and_then(|value| serde_json::from_str::<Vec<String>>(&value).ok());
    if !server_path.is_file()
        || !server_key_path.is_file()
        || previous_addresses.as_ref() != Some(&address_values)
    {
        let ca_key = KeyPair::from_pem(&ca_key_pem)?;
        let issuer = Issuer::from_ca_cert_pem(&ca_pem, ca_key)?;
        let names = if address_values.is_empty() {
            vec!["localhost".to_string()]
        } else {
            address_values.clone()
        };
        let params = CertificateParams::new(names)?;
        let server_key = KeyPair::generate()?;
        let server_certificate = params.signed_by(&server_key, &issuer)?;
        atomic_write(&server_path, server_certificate.pem().as_bytes())?;
        atomic_write(&server_key_path, server_key.serialize_pem().as_bytes())?;
        atomic_write(
            &addresses_path,
            serde_json::to_vec(&address_values)?.as_slice(),
        )?;
    }

    let ca_pem_block = pem::parse(ca_pem.as_bytes())
        .map_err(|error| anyhow::anyhow!("解析手机 CA 失败：{error}"))?;
    let fingerprint = Sha256::digest(ca_pem_block.contents())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(MobileTlsMaterial {
        ca_certificate_path: ca_path,
        server_certificate_path: server_path,
        server_key_path,
        fingerprint,
    })
}

fn atomic_write(path: &Path, contents: &[u8]) -> Result<()> {
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    fs::write(&temporary, contents)?;
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::ensure;
    use std::net::Ipv4Addr;

    #[test]
    fn certificate_material_is_reused_and_fingerprint_is_stable() {
        let path =
            std::env::temp_dir().join(format!("meshshare-mobile-tls-{}", uuid::Uuid::new_v4()));
        let first = ensure(&path, &[Ipv4Addr::LOCALHOST]).expect("create certificate material");
        let second = ensure(&path, &[Ipv4Addr::LOCALHOST]).expect("reuse certificate material");
        assert_eq!(first.fingerprint, second.fingerprint);
        assert!(first.ca_certificate_path.is_file());
        assert!(first.server_certificate_path.is_file());
        assert!(first.server_key_path.is_file());
        let _ = std::fs::remove_dir_all(path);
    }
}
