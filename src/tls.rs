//! A self-signed certificate for the HTTPS listener. Some clients
//! (simple-salesforce, for one) hard-code `https://` for the instance URL.

use std::path::Path;

pub struct CertPair {
    pub cert_pem: String,
    pub key_pem: String,
}

/// Load the certificate from `dir`, or generate one and save it there.
pub fn load_or_create(dir: &Path) -> anyhow::Result<CertPair> {
    let (cert_path, key_path) = (dir.join("localforce-cert.pem"), dir.join("localforce-key.pem"));
    if let (Ok(cert_pem), Ok(key_pem)) = (std::fs::read_to_string(&cert_path), std::fs::read_to_string(&key_path)) {
        return Ok(CertPair { cert_pem, key_pem });
    }
    let key = rcgen::KeyPair::generate()?;
    let mut params = rcgen::CertificateParams::new(vec!["localhost".into(), "127.0.0.1".into()])?;
    params.distinguished_name.push(rcgen::DnType::CommonName, "localforce");
    let cert = params.self_signed(&key)?;
    let pair = CertPair { cert_pem: cert.pem(), key_pem: key.serialize_pem() };
    std::fs::create_dir_all(dir)?;
    std::fs::write(&cert_path, &pair.cert_pem)?;
    std::fs::write(&key_path, &pair.key_pem)?;
    Ok(pair)
}
