//! ms-platform-windows —— Win32 平台实现（DEVELOPMENT.md §3.1）。
//! 在非 Windows 目标上编译为空 crate，保证 workspace 跨平台可构建。
//!
//! T-004 骨架阶段：Platform 工厂返回通用 stub；
//! 真实实现自 T-107（捕获）/T-108（注入）起逐能力替换。

pub mod autostart;
pub mod clipboard_fmt;
pub mod event_decode;
pub mod keymap;

#[cfg(any(target_os = "windows", test))]
mod inject_state;

#[cfg(target_os = "windows")]
pub mod capture;

#[cfg(target_os = "windows")]
pub mod clipboard;

#[cfg(target_os = "windows")]
pub mod inject;

#[cfg(target_os = "windows")]
pub mod qos;

#[cfg(target_os = "windows")]
pub mod screen;

#[cfg(target_os = "windows")]
pub mod secure_store;

#[cfg(target_os = "windows")]
pub mod window;

/// 声明进程为 Per-Monitor v2 DPI 感知，把坐标系统一为**物理像素**。
///
/// 不这么做时坐标系会混用：低级鼠标钩子的 `MSLLHOOKSTRUCT.pt` 给物理坐标，而
/// `GetSystemMetrics`/`SetCursorPos`/`GetCursorPos` 给缩放后的逻辑坐标。两者混算
/// 会导致：① 边缘误判（屏高被当成逻辑值，物理光标没到底就触发穿越）；
/// ② grab 漂移（读回的逻辑基准与物理 pt 相减得到巨量位移，光标飞向角落）。
///
/// 必须在任何坐标/窗口 API 之前调用（main 最早）。幂等；失败仅告警。
#[cfg(target_os = "windows")]
pub fn set_process_dpi_aware() {
    use windows::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
    };
    // SAFETY: 进程级标量调用，无指针入参；重复/过晚调用只返回 Err，不影响内存安全
    unsafe {
        if let Err(e) = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) {
            tracing::warn!(
                target: "mouseshare::win",
                error = %e,
                "设置 DPI 感知失败（可能已由 manifest 设定）"
            );
        }
    }
}

/// 把标准输出/错误重定向到 `NUL`（GUI 子系统专用）。
///
/// `windows_subsystem = "windows"` 的进程不弹控制台，但也没有有效的 stdout/stderr 句柄——
/// 此时 `println!`/`eprintln!` 写无效句柄会 **panic**（"failed printing to stdout"）。
/// 本函数在 main 最早把两个标准流指向 `NUL`，使既不弹窗、也不 panic（日志走文件）。
/// 必须在任何打印之前调用；调试构建（有真实控制台）不要调用，否则看不到输出。
#[cfg(target_os = "windows")]
pub fn redirect_std_to_null() {
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_GENERIC_WRITE, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows::Win32::System::Console::{STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle};
    use windows::core::w;
    // SAFETY: 文件名为静态宽串 "NUL"，其余为标量/常量入参；成功句柄交 SetStdHandle 接管，
    // 由系统在进程退出时回收（标准流句柄无需手动 CloseHandle）。
    unsafe {
        if let Ok(h) = CreateFileW(
            w!("NUL"),
            FILE_GENERIC_WRITE.0,
            FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        ) {
            let _ = SetStdHandle(STD_OUTPUT_HANDLE, h);
            let _ = SetStdHandle(STD_ERROR_HANDLE, h);
        }
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use ms_platform::{
        Autostart, ClipboardAccess, InputCapture, InputInject, Platform, Result, ScreenInfo,
        SecureStore,
    };

    use crate::autostart::WinAutostart;
    use crate::capture::WinCapture;
    use crate::clipboard::WinClipboard;
    use crate::inject::WinInject;
    use crate::screen::WinScreenInfo;
    use crate::secure_store::WinSecureStore;

    pub struct WinPlatform;

    impl WinPlatform {
        pub fn new() -> Self {
            Self
        }
    }

    impl Default for WinPlatform {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Platform for WinPlatform {
        fn name(&self) -> &'static str {
            "windows"
        }
        fn input_capture(&self) -> Result<Box<dyn InputCapture>> {
            Ok(Box::new(WinCapture::new()))
        }
        fn input_inject(&self) -> Result<Box<dyn InputInject>> {
            Ok(Box::new(WinInject::new()))
        }
        fn clipboard(&self) -> Result<Box<dyn ClipboardAccess>> {
            Ok(Box::new(WinClipboard::new()))
        }
        fn screen_info(&self) -> Result<Box<dyn ScreenInfo>> {
            Ok(Box::new(WinScreenInfo::new()))
        }
        fn autostart(&self) -> Result<Box<dyn Autostart>> {
            Ok(Box::new(WinAutostart::new()?))
        }
        fn secure_store(&self) -> Result<Box<dyn SecureStore>> {
            Ok(Box::new(WinSecureStore::new()?))
        }
    }
}

#[cfg(target_os = "windows")]
pub use imp::WinPlatform;
