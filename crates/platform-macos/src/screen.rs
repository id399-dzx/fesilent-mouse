//! macOS 屏幕信息（CoreGraphics CGDisplayBounds；DEVELOPMENT.md §3.2，FR-204）。
//!
//! 仅在 macOS 编译。v1 用主显示边界（点/逻辑坐标，与 CGEvent 鼠标坐标系一致）；
//! 多显示器并集留后续。键盘布局标识 v1 返回空（不误报；TIS 读取后续接入）。

use ms_platform::{Rect, Result, ScreenInfo, VirtualScreen};

#[repr(C)]
struct CGPoint {
    x: f64,
    y: f64,
}
#[repr(C)]
struct CGSize {
    width: f64,
    height: f64,
}
#[repr(C)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGMainDisplayID() -> u32;
    fn CGDisplayBounds(display: u32) -> CGRect;
    fn CGDisplayPixelsWide(display: u32) -> usize;
}

pub struct MacScreenInfo;

impl MacScreenInfo {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MacScreenInfo {
    fn default() -> Self {
        Self::new()
    }
}

impl ScreenInfo for MacScreenInfo {
    fn virtual_screen(&mut self) -> Result<VirtualScreen> {
        // SAFETY: 查询主显示边界（点坐标）与像素宽度，无副作用
        let (bounds, pixels_wide) = unsafe {
            let id = CGMainDisplayID();
            (CGDisplayBounds(id), CGDisplayPixelsWide(id))
        };
        let rect = Rect {
            x: bounds.origin.x,
            y: bounds.origin.y,
            w: bounds.size.width,
            h: bounds.size.height,
        };
        // 缩放比 = 物理像素宽 / 逻辑点宽（Retina 通常为 2.0）
        let scale = if bounds.size.width > 0.0 {
            (pixels_wide as f32 / bounds.size.width as f32).max(1.0)
        } else {
            1.0
        };
        Ok(VirtualScreen { rect, scale })
    }

    fn keyboard_layout(&mut self) -> Result<String> {
        Ok(String::new()) // v1：未知（不参与布局一致性误报）
    }
}
