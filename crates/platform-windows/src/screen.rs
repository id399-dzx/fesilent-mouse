//! Windows 屏幕信息（GetSystemMetrics 虚拟桌面；DEVELOPMENT.md §3.1，FR-204）。
//! 仅在 Windows 编译。键盘布局标识 v1 返回空（不误报）。

use ms_platform::{Rect, Result, ScreenInfo, VirtualScreen};

use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

pub struct WinScreenInfo;

impl WinScreenInfo {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WinScreenInfo {
    fn default() -> Self {
        Self::new()
    }
}

impl ScreenInfo for WinScreenInfo {
    fn virtual_screen(&mut self) -> Result<VirtualScreen> {
        // SAFETY: 查询虚拟桌面矩形（物理像素），无副作用
        let (x, y, w, h) = unsafe {
            (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
            )
        };
        Ok(VirtualScreen {
            rect: Rect {
                x: x as f64,
                y: y as f64,
                w: w as f64,
                h: h as f64,
            },
            scale: 1.0,
        })
    }

    fn keyboard_layout(&mut self) -> Result<String> {
        Ok(String::new()) // v1：未知
    }
}
