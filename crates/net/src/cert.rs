//! 自签 Ed25519 证书生成与指纹（DEVELOPMENT.md §4.5；FR-701/703）。
//!
//! 每端持有一张长期自签证书，QUIC/TLS 1.3 双向认证（mTLS）。对端身份由
//! 证书的 SHA-256 指纹标识，配对后 pinning（T-203）。

use rustls_pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
pub enum CertError {
    #[error("证书生成失败: {0}")]
    Generate(String),
}

/// 本机长期身份：自签证书 + 私钥（DER）。
#[derive(Clone)]
pub struct Identity {
    cert_der: Vec<u8>,
    key_der: Vec<u8>,
    fingerprint: [u8; 32],
}

impl Identity {
    /// 生成一张新的自签 Ed25519 证书。
    pub fn generate() -> Result<Self, CertError> {
        let key_pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519)
            .map_err(|e| CertError::Generate(e.to_string()))?;
        // CN 用固定串；身份由指纹而非名称决定
        let params = rcgen::CertificateParams::new(vec!["mouseshare".to_string()])
            .map_err(|e| CertError::Generate(e.to_string()))?;
        let cert = params
            .self_signed(&key_pair)
            .map_err(|e| CertError::Generate(e.to_string()))?;
        let cert_der = cert.der().to_vec();
        let key_der = key_pair.serialize_der();
        let fingerprint = fingerprint_of(&cert_der);
        Ok(Self {
            cert_der,
            key_der,
            fingerprint,
        })
    }

    /// 从持久化的 DER 还原（SecureStore 读出后，T-203）。
    pub fn from_der(cert_der: Vec<u8>, key_der: Vec<u8>) -> Self {
        let fingerprint = fingerprint_of(&cert_der);
        Self {
            cert_der,
            key_der,
            fingerprint,
        }
    }

    pub fn cert_der(&self) -> &[u8] {
        &self.cert_der
    }

    pub fn key_der(&self) -> &[u8] {
        &self.key_der
    }

    /// 本端证书的 SHA-256 指纹（配对码与 pinning 用）。
    pub fn fingerprint(&self) -> [u8; 32] {
        self.fingerprint
    }

    /// rustls 用的证书链（单证书）。
    pub fn rustls_cert_chain(&self) -> Vec<CertificateDer<'static>> {
        vec![CertificateDer::from(self.cert_der.clone())]
    }

    /// rustls 用的私钥。
    pub fn rustls_key(&self) -> PrivateKeyDer<'static> {
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key_der.clone()))
    }
}

/// 计算证书 DER 的 SHA-256 指纹。
pub fn fingerprint_of(cert_der: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(cert_der);
    h.finalize().into()
}

/// 指纹的十六进制短串（日志/调试用，取前 8 字节）。
pub fn fingerprint_hex(fp: &[u8; 32]) -> String {
    fp.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn 生成证书指纹稳定且自洽() {
        let id = Identity::generate().unwrap();
        assert_eq!(id.fingerprint(), fingerprint_of(id.cert_der()));
        assert!(!id.cert_der().is_empty());
        assert!(!id.key_der().is_empty());
    }

    #[test]
    fn 两次生成指纹不同() {
        let a = Identity::generate().unwrap();
        let b = Identity::generate().unwrap();
        assert_ne!(a.fingerprint(), b.fingerprint(), "每次生成应是独立证书");
    }

    #[test]
    fn from_der往返保持指纹() {
        let id = Identity::generate().unwrap();
        let restored = Identity::from_der(id.cert_der().to_vec(), id.key_der().to_vec());
        assert_eq!(id.fingerprint(), restored.fingerprint());
    }

    #[test]
    fn 指纹短串长度16() {
        let id = Identity::generate().unwrap();
        assert_eq!(fingerprint_hex(&id.fingerprint()).len(), 16);
    }
}
