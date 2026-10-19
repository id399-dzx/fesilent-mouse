//! ms-platform-macos —— CoreGraphics/AppKit 平台实现（DEVELOPMENT.md §3.2）。
//! 在非 macOS 目标上编译为空 crate，保证 workspace 跨平台可构建。
//!
//! T-004 骨架阶段：Platform 工厂返回通用 stub；
//! 真实实现自 T-103（捕获）/T-105（注入）起逐能力替换。

pub mod breaker;
pub mod event_decode;
pub mod inject_state;
pub mod keymap;

#[cfg(target_os = "macos")]
pub mod autostart;

#[cfg(target_os = "macos")]
pub mod capture;

#[cfg(target_os = "macos")]
pub mod clipboard;

#[cfg(target_os = "macos")]
pub mod inject;

#[cfg(target_os = "macos")]
pub mod permission;

#[cfg(target_os = "macos")]
pub mod power_monitor;

#[cfg(target_os = "macos")]
pub mod screen;

#[cfg(target_os = "macos")]
pub mod secure_store;

#[cfg(target_os = "macos")]
pub mod watchdog;

#[cfg(target_os = "macos")]
pub mod window;

#[cfg(target_os = "macos")]
mod imp {
    use ms_platform::{
        Autostart, ClipboardAccess, InputCapture, InputInject, Platform, Result, ScreenInfo,
        SecureStore,
    };

    use crate::autostart::MacAutostart;
    use crate::capture::MacCapture;
    use crate::clipboard::MacClipboard;
    use crate::inject::MacInject;
    use crate::screen::MacScreenInfo;
    use crate::secure_store::MacSecureStore;

    pub struct MacPlatform;

    impl MacPlatform {
        pub fn new() -> Self {
            Self
        }
    }

    impl Default for MacPlatform {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Platform for MacPlatform {
        fn name(&self) -> &'static str {
            "macos"
        }
        fn input_capture(&self) -> Result<Box<dyn InputCapture>> {
            Ok(Box::new(MacCapture::new()))
        }
        fn input_inject(&self) -> Result<Box<dyn InputInject>> {
            Ok(Box::new(MacInject::new()?))
        }
        fn clipboard(&self) -> Result<Box<dyn ClipboardAccess>> {
            Ok(Box::new(MacClipboard::new()))
        }
        fn screen_info(&self) -> Result<Box<dyn ScreenInfo>> {
            Ok(Box::new(MacScreenInfo::new()))
        }
        fn autostart(&self) -> Result<Box<dyn Autostart>> {
            Ok(Box::new(MacAutostart::new()?))
        }
        fn secure_store(&self) -> Result<Box<dyn SecureStore>> {
            Ok(Box::new(MacSecureStore::new()))
        }
        fn has_input_permission(&self) -> bool {
            crate::permission::is_trusted()
        }
        fn open_input_permission_settings(&self) {
            crate::permission::prompt_and_open_settings();
        }
    }
}

#[cfg(target_os = "macos")]
pub use imp::MacPlatform;
