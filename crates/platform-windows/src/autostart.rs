//! Windows 开机自启（T-604，FR-805）。
//!
//! 在 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` 写一个名为 `MouseShare`
//! 的 `REG_SZ` 值，数据为带引号的可执行路径加 `--tray`。该位置无需管理员权限、
//! 随当前用户登录时自动拉起托盘进程。删除该值即关闭自启。

use std::path::Path;

/// 生成 Run 值的命令行：`"<exe>" --tray`（路径加引号以容纳空格）。
pub fn autostart_command(exe: &Path) -> String {
    format!("\"{}\" --tray", exe.display())
}

#[cfg(target_os = "windows")]
mod imp {
    use std::path::PathBuf;

    use ms_platform::{Autostart, PlatformError, Result};
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SAM_FLAGS, REG_SZ,
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
    };
    use windows::core::{PCWSTR, w};

    use super::autostart_command;

    /// Run 子键路径。
    const RUN_SUBKEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
    /// 值名（与产品名一致）。
    const VALUE_NAME: PCWSTR = w!("FesilentMouse");

    pub struct WinAutostart {
        program: PathBuf,
    }

    impl WinAutostart {
        pub fn new() -> Result<Self> {
            let program = std::env::current_exe().map_err(PlatformError::Io)?;
            Ok(Self { program })
        }
    }

    impl Autostart for WinAutostart {
        fn set_enabled(&mut self, on: bool) -> Result<()> {
            if on {
                let hkey = open_run_key(KEY_SET_VALUE)?;
                let cmd = autostart_command(&self.program);
                // REG_SZ 需 UTF-16LE 且以 NUL 结尾
                let wide: Vec<u16> = cmd.encode_utf16().chain(std::iter::once(0)).collect();
                let bytes: Vec<u8> = wide.iter().flat_map(|u| u.to_le_bytes()).collect();
                // SAFETY: hkey 为上面成功打开的句柄；VALUE_NAME 为静态宽串；bytes 为本地切片
                let r = unsafe { RegSetValueExW(hkey, VALUE_NAME, None, REG_SZ, Some(&bytes)) };
                // SAFETY: 关闭刚打开的句柄
                unsafe {
                    let _ = RegCloseKey(hkey);
                }
                r.ok()
                    .map_err(|e| PlatformError::Api(format!("写 Run 值失败: {e}")))?;
            } else {
                let hkey = open_run_key(KEY_SET_VALUE)?;
                // SAFETY: hkey 有效；VALUE_NAME 静态宽串
                let r = unsafe { RegDeleteValueW(hkey, VALUE_NAME) };
                // SAFETY: 关闭句柄
                unsafe {
                    let _ = RegCloseKey(hkey);
                }
                if r != ERROR_FILE_NOT_FOUND {
                    // 不存在视作成功（幂等）
                    r.ok()
                        .map_err(|e| PlatformError::Api(format!("删 Run 值失败: {e}")))?;
                }
            }
            Ok(())
        }

        fn is_enabled(&mut self) -> Result<bool> {
            let hkey = open_run_key(KEY_QUERY_VALUE)?;
            let mut size: u32 = 0;
            // SAFETY: hkey 有效；仅查询大小（lpdata 传 None），size 接收字节数
            let r =
                unsafe { RegQueryValueExW(hkey, VALUE_NAME, None, None, None, Some(&mut size)) };
            // SAFETY: 关闭句柄
            unsafe {
                let _ = RegCloseKey(hkey);
            }
            Ok(r == ERROR_SUCCESS)
        }
    }

    /// 以指定权限打开（已存在的）Run 子键。
    fn open_run_key(access: REG_SAM_FLAGS) -> Result<HKEY> {
        let mut hkey = HKEY::default();
        // SAFETY: 子键为静态宽串，phkresult 指向栈上 HKEY
        let r = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, RUN_SUBKEY, Some(0), access, &mut hkey) };
        r.ok()
            .map_err(|e| PlatformError::Api(format!("打开 Run 键失败: {e}")))?;
        Ok(hkey)
    }
}

#[cfg(target_os = "windows")]
pub use imp::WinAutostart;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn 命令行带引号与tray参数() {
        let cmd = autostart_command(&PathBuf::from(
            r"C:\Program Files\MouseShare\mouseshare.exe",
        ));
        assert_eq!(
            cmd,
            r#""C:\Program Files\MouseShare\mouseshare.exe" --tray"#
        );
    }
}
