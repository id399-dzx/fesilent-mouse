//! macOS Keychain 安全存储（SPEC.md §4，FR-703）。
//!
//! 用 Keychain 的通用密码项（generic password）存私钥/信任列表，service 固定为
//! bundle id，account 即键名。Keychain 数据与用户账户绑定加密，明文不落盘。

use ms_platform::{PlatformError, Result, SecureStore};
use security_framework::passwords::{
    delete_generic_password, get_generic_password, set_generic_password,
};

/// Keychain service 名（SPEC §4：com.mouseshare.app）。
const SERVICE: &str = "com.mouseshare.app";

pub struct MacSecureStore;

impl MacSecureStore {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MacSecureStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SecureStore for MacSecureStore {
    fn get(&mut self, key: &str) -> Result<Option<Vec<u8>>> {
        match get_generic_password(SERVICE, key) {
            Ok(bytes) => Ok(Some(bytes)),
            // 项不存在不是错误，返回 None
            Err(e) if e.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(None),
            Err(e) => Err(PlatformError::Api(format!("Keychain 读取失败: {e}"))),
        }
    }

    fn set(&mut self, key: &str, value: &[u8]) -> Result<()> {
        set_generic_password(SERVICE, key, value)
            .map_err(|e| PlatformError::Api(format!("Keychain 写入失败: {e}")))
    }

    fn delete(&mut self, key: &str) -> Result<()> {
        match delete_generic_password(SERVICE, key) {
            Ok(()) => Ok(()),
            Err(e) if e.code() == ERR_SEC_ITEM_NOT_FOUND => Ok(()), // 已不存在视作成功
            Err(e) => Err(PlatformError::Api(format!("Keychain 删除失败: {e}"))),
        }
    }
}

/// errSecItemNotFound（Security framework 错误码）。
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    // Keychain 读写需登录钥匙串可用；ad-hoc 签名的测试二进制可能弹访问确认。
    // 沙箱内通常不可用，标 ignore；非沙箱本机实测。
    #[ignore = "需登录钥匙串；沙箱不可用，非沙箱本机实测"]
    #[test]
    fn keychain往返() {
        let mut s = MacSecureStore::new();
        let key = "test-roundtrip-key";
        let val = b"super-secret-private-key-bytes".to_vec();

        s.set(key, &val).unwrap();
        assert_eq!(s.get(key).unwrap(), Some(val));
        s.delete(key).unwrap();
        assert_eq!(s.get(key).unwrap(), None, "删除后应读不到");
        // 删除不存在的项应成功（幂等）
        s.delete(key).unwrap();
    }
}
