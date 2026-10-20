//! 用户重复启动时，把前台激活权限交给已运行实例。

/// 用户刚启动的副实例可以将自己的前台权限转给同一用户的主实例。
/// 仅在校验本机实例记录后调用；失败时仍发送激活请求，由系统决定焦点。
pub fn allow_existing_instance_foreground(process_id: u32) -> bool {
    if process_id == 0 {
        return false;
    }
    // SAFETY: Win32只接收进程ID标量，不传指针、不持有句柄；失败返回错误。
    unsafe { windows::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow(process_id).is_ok() }
}

/// GUI 子系统没有控制台，启动阶段错误用系统对话框给出可见原因。
pub fn show_startup_error(message: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    use windows::core::{PCWSTR, w};
    let text: Vec<u16> = message
        .encode_utf16()
        .filter(|value| *value != 0)
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: text在调用期间保持有效且以NUL结尾，标题为静态宽字符串；系统不保留指针。
    unsafe {
        let _ = MessageBoxW(
            None,
            PCWSTR(text.as_ptr()),
            w!("Fesilent Mouse"),
            MB_OK | MB_ICONERROR,
        );
    }
}
