//! macOS「辅助功能」授权检测与引导（FR-803，H-1）。
//!
//! 输入捕获（CGEventTap）与注入需「系统设置 → 隐私与安全性 → 辅助功能」勾选本应用。
//! `AXIsProcessTrusted` 查询当前是否已授权；带 prompt 选项的查询会把本应用加入授权
//! 列表并弹系统提示，便于用户直接在列表里勾选。

use std::ffi::c_void;

type CFStringRef = *const c_void;
type CFDictionaryRef = *const c_void;
type CFBooleanRef = *const c_void;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: CFDictionaryRef) -> bool;
    static kAXTrustedCheckOptionPrompt: CFStringRef;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFBooleanTrue: CFBooleanRef;
    fn CFDictionaryCreate(
        allocator: *const c_void,
        keys: *const *const c_void,
        values: *const *const c_void,
        num_values: isize,
        key_callbacks: *const c_void,
        value_callbacks: *const c_void,
        // 真实签名含两个 callback 结构指针，传 null 用默认（不保留/释放，键值为静态常量）
    ) -> CFDictionaryRef;
    fn CFRelease(cf: *const c_void);
}

/// 当前进程是否已获辅助功能授权。
pub fn is_trusted() -> bool {
    // SAFETY: 无参数的纯查询，无副作用
    unsafe { AXIsProcessTrusted() }
}

/// 触发系统授权提示：把本应用加入「辅助功能」列表并弹出系统提示，随后打开设置页，
/// 用户只需在列表里勾选本应用即可。
pub fn prompt_and_open_settings() {
    // SAFETY: 用静态 CFString 键 + kCFBooleanTrue 值构一个单元素字典；键值为框架静态
    // 常量，使用默认（null）回调安全；查询本身仅触发系统提示，无内存所有权问题。
    unsafe {
        let keys = [kAXTrustedCheckOptionPrompt];
        let values = [kCFBooleanTrue];
        let opts = CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            1,
            std::ptr::null(),
            std::ptr::null(),
        );
        if !opts.is_null() {
            let _ = AXIsProcessTrustedWithOptions(opts);
            CFRelease(opts);
        }
    }
    // 直接打开「隐私与安全性 → 辅助功能」设置页
    let _ = std::process::Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        .spawn();
}

#[cfg(test)]
mod tests {
    use super::*;

    // 验证 AXIsProcessTrusted FFI 在真机不崩、返回 bool。沙箱无 AX 环境，标 ignore。
    #[ignore = "需 macOS GUI 会话；非沙箱本机：cargo test -p ms-platform-macos -- --ignored is_trusted"]
    #[test]
    fn is_trusted_可调用() {
        let t = is_trusted();
        println!("辅助功能授权: {t}");
    }
}
