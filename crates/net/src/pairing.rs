//! 配对与信任：SAS 短认证串 + 信任列表 pinning（DEVELOPMENT.md §4.5；FR-103/702/703）。
//!
//! 首次配对（防中间人）：TLS 握手后双端各自用「导出的 TLS 会话密钥 + 双方证书
//! 指纹」计算同一个 6 位 SAS 显示给用户，两端一致且用户确认才写入信任列表。
//! 之后每次连接校验对端指纹必须在信任列表中（pinning），否则拒绝。

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// TLS 密钥导出标签（RFC 5705）。两端必须一致。
pub const SAS_EXPORT_LABEL: &[u8] = b"MOUSESHARE-SAS-v1";
/// 导出密钥长度。
pub const SAS_EXPORT_LEN: usize = 32;

/// 计算 6 位 SAS（短认证串）。
///
/// 输入：TLS 导出的共享密钥 + 双方证书指纹。对两个指纹排序后参与哈希，
/// 保证两端（角色相反）算出**同一个**码。返回 6 位十进制零填充字符串。
pub fn compute_sas(exported: &[u8], fp_x: &[u8; 32], fp_y: &[u8; 32]) -> String {
    let (lo, hi) = if fp_x <= fp_y {
        (fp_x, fp_y)
    } else {
        (fp_y, fp_x)
    };
    let mut h = Sha256::new();
    h.update(SAS_EXPORT_LABEL);
    h.update(exported);
    h.update(lo);
    h.update(hi);
    let digest = h.finalize();
    // 取前 4 字节大端做模 1_000_000
    let n = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]);
    format!("{:06}", n % 1_000_000)
}

/// 信任列表：已配对对端的证书指纹集合（FR-104）。可序列化以持久化到 SecureStore。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TrustStore {
    /// 指纹以十六进制字符串存储，便于人读与 JSON 持久化。
    fingerprints: BTreeSet<String>,
}

impl TrustStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn hex(fp: &[u8; 32]) -> String {
        fp.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// 配对成功后加入信任（FR-103/104）。
    pub fn trust(&mut self, fp: &[u8; 32]) {
        self.fingerprints.insert(Self::hex(fp));
    }

    /// 解除配对（FR-105）。
    pub fn revoke(&mut self, fp: &[u8; 32]) -> bool {
        self.fingerprints.remove(&Self::hex(fp))
    }

    /// pinning 校验：对端指纹是否已信任（FR-702）。
    pub fn is_trusted(&self, fp: &[u8; 32]) -> bool {
        self.fingerprints.contains(&Self::hex(fp))
    }

    pub fn is_empty(&self) -> bool {
        self.fingerprints.is_empty()
    }

    pub fn len(&self) -> usize {
        self.fingerprints.len()
    }

    /// 仅用于发现时跳过已知设备；广播ID不能用于授予信任。
    pub fn contains_discovery_id(&self, id: &str) -> bool {
        id.len() == 32 && self.fingerprints.iter().any(|fp| fp.starts_with(id))
    }

    /// 序列化为字节（存入 SecureStore）。
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// 从 SecureStore 读出的字节还原；损坏则返回空信任列表。
    pub fn from_bytes(bytes: &[u8]) -> Self {
        serde_json::from_slice(bytes).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    #[test]
    fn sas_两端角色相反算出同码() {
        let secret = b"shared-tls-exported-secret-bytes";
        let a = fp(0xAA);
        let b = fp(0xBB);
        // A 端：own=a, peer=b；B 端：own=b, peer=a。顺序相反应得同码
        let sas_a = compute_sas(secret, &a, &b);
        let sas_b = compute_sas(secret, &b, &a);
        assert_eq!(sas_a, sas_b);
        assert_eq!(sas_a.len(), 6);
        assert!(sas_a.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn sas_不同密钥不同码() {
        let a = fp(1);
        let b = fp(2);
        let s1 = compute_sas(b"secret-one-aaaaaaaaaaaaaaaaaaaaaa", &a, &b);
        let s2 = compute_sas(b"secret-two-bbbbbbbbbbbbbbbbbbbbbb", &a, &b);
        assert_ne!(s1, s2, "中间人换密钥应导致 SAS 不一致（防 MITM）");
    }

    #[test]
    fn sas_不同对端不同码() {
        let secret = b"same-secret-same-secret-same-sec";
        let s1 = compute_sas(secret, &fp(1), &fp(2));
        let s2 = compute_sas(secret, &fp(1), &fp(3));
        assert_ne!(s1, s2);
    }

    #[test]
    fn 信任列表配对与pinning() {
        let mut ts = TrustStore::new();
        let peer = fp(0x42);
        assert!(!ts.is_trusted(&peer), "未配对应拒绝");
        ts.trust(&peer);
        assert!(ts.is_trusted(&peer), "配对后应放行");
        assert_eq!(ts.len(), 1);
        assert!(ts.revoke(&peer));
        assert!(!ts.is_trusted(&peer), "解除配对后应再次拒绝");
    }

    #[test]
    fn 信任列表序列化往返() {
        let mut ts = TrustStore::new();
        ts.trust(&fp(1));
        ts.trust(&fp(2));
        let restored = TrustStore::from_bytes(&ts.to_bytes());
        assert!(restored.is_trusted(&fp(1)));
        assert!(restored.is_trusted(&fp(2)));
        assert_eq!(restored.len(), 2);
    }

    #[test]
    fn 损坏字节还原为空列表() {
        let ts = TrustStore::from_bytes(b"not json at all");
        assert!(ts.is_empty());
    }
}
