//! Windows DPAPI 安全存储（SPEC.md §4，FR-703）。
//!
//! 用 `CryptProtectData`（DPAPI，绑定当前用户）加密私钥/信任列表后写入配置目录
//! `%APPDATA%\MouseShare\secure\<key>.bin`，读取时 `CryptUnprotectData` 解密。
//! DPAPI 密钥由系统按用户账户管理，密文拷走到别的账户/机器无法解密。

use std::path::PathBuf;

use ms_platform::{PlatformError, Result, SecureStore};

use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
use windows::core::PCWSTR;

pub struct WinSecureStore {
    dir: PathBuf,
}

impl WinSecureStore {
    pub fn new() -> Result<Self> {
        let base = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .ok_or_else(|| PlatformError::Api("APPDATA 环境变量缺失".into()))?;
        let dir = base.join("MouseShare").join("secure");
        std::fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    fn path_for(&self, key: &str) -> Result<PathBuf> {
        // 仅允许安全的键名字符，避免路径穿越
        if key.is_empty()
            || !key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(PlatformError::Api(format!("非法存储键名: {key}")));
        }
        Ok(self.dir.join(format!("{key}.bin")))
    }
}

impl SecureStore for WinSecureStore {
    fn get(&mut self, key: &str) -> Result<Option<Vec<u8>>> {
        let path = self.path_for(key)?;
        let cipher = match std::fs::read(&path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(PlatformError::Io(e)),
        };
        // SAFETY: 传入合法切片构造的 blob，输出 blob 由 DPAPI 分配，使用后 LocalFree
        let plain = unsafe { dpapi(&cipher, false)? };
        Ok(Some(plain))
    }

    fn set(&mut self, key: &str, value: &[u8]) -> Result<()> {
        let path = self.path_for(key)?;
        // SAFETY: 同上
        let cipher = unsafe { dpapi(value, true)? };
        // 原子写：先写临时文件再 rename
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &cipher)?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    fn delete(&mut self, key: &str) -> Result<()> {
        let path = self.path_for(key)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(PlatformError::Io(e)),
        }
    }
}

/// DPAPI 加/解密。`protect=true` 加密，否则解密。
///
/// # Safety
/// 调用 Win32 DPAPI；输入切片需有效，输出 blob 由系统分配并在本函数内 LocalFree。
unsafe fn dpapi(input: &[u8], protect: bool) -> Result<Vec<u8>> {
    let in_blob = CRYPT_INTEGER_BLOB {
        cbData: input.len() as u32,
        pbData: input.as_ptr() as *mut u8,
    };
    let mut out = CRYPT_INTEGER_BLOB::default();

    // SAFETY: in_blob 指向有效切片；out 接收系统分配的密文/明文
    let res = unsafe {
        if protect {
            CryptProtectData(
                &in_blob,
                PCWSTR::null(),
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        } else {
            CryptUnprotectData(
                &in_blob,
                None,
                None,
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut out,
            )
        }
    };
    res.map_err(|e| PlatformError::Api(format!("DPAPI 失败: {e}")))?;

    // SAFETY: out.pbData 指向 out.cbData 字节的有效内存（DPAPI 分配）
    let bytes = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec() };
    // SAFETY: out.pbData 由 DPAPI 经 LocalAlloc 分配，必须 LocalFree 释放
    unsafe {
        let _ = LocalFree(Some(HLOCAL(out.pbData as *mut core::ffi::c_void)));
    }
    Ok(bytes)
}
