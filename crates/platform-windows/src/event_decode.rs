//! 低级钩子消息 → 中立事件的纯解码（DEVELOPMENT.md §3.1）。
//! 与 FFI 解耦，任何平台可单测。

use ms_platform::Button;

/// 窗口消息常量（WinUser.h）。
pub mod wm {
    pub const KEYDOWN: u32 = 0x0100;
    pub const KEYUP: u32 = 0x0101;
    pub const SYSKEYDOWN: u32 = 0x0104;
    pub const SYSKEYUP: u32 = 0x0105;
    pub const MOUSEMOVE: u32 = 0x0200;
    pub const LBUTTONDOWN: u32 = 0x0201;
    pub const LBUTTONUP: u32 = 0x0202;
    pub const RBUTTONDOWN: u32 = 0x0204;
    pub const RBUTTONUP: u32 = 0x0205;
    pub const MBUTTONDOWN: u32 = 0x0207;
    pub const MBUTTONUP: u32 = 0x0208;
    pub const MOUSEWHEEL: u32 = 0x020A;
    pub const XBUTTONDOWN: u32 = 0x020B;
    pub const XBUTTONUP: u32 = 0x020C;
    pub const MOUSEHWHEEL: u32 = 0x020E;
}

const WHEEL_DELTA: f64 = 120.0;
const XBUTTON1: u16 = 0x0001;
const XBUTTON2: u16 = 0x0002;

/// 键盘消息 → 是否按下（KEYDOWN/SYSKEYDOWN=true，UP=false，其余 None）。
pub fn key_down(message: u32) -> Option<bool> {
    match message {
        wm::KEYDOWN | wm::SYSKEYDOWN => Some(true),
        wm::KEYUP | wm::SYSKEYUP => Some(false),
        _ => None,
    }
}

/// 鼠标按键消息 + mouseData → (按键, 是否按下)。
pub fn mouse_button(message: u32, mouse_data: u32) -> Option<(Button, bool)> {
    match message {
        wm::LBUTTONDOWN => Some((Button::Left, true)),
        wm::LBUTTONUP => Some((Button::Left, false)),
        wm::RBUTTONDOWN => Some((Button::Right, true)),
        wm::RBUTTONUP => Some((Button::Right, false)),
        wm::MBUTTONDOWN => Some((Button::Middle, true)),
        wm::MBUTTONUP => Some((Button::Middle, false)),
        wm::XBUTTONDOWN | wm::XBUTTONUP => {
            let down = message == wm::XBUTTONDOWN;
            let high = (mouse_data >> 16) as u16;
            match high {
                XBUTTON1 => Some((Button::X1, down)),
                XBUTTON2 => Some((Button::X2, down)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// 滚轮消息 + mouseData → (dx, dy, precise)。高位字是有符号 wheel delta。
pub fn wheel(message: u32, mouse_data: u32) -> Option<(f64, f64, bool)> {
    let delta = ((mouse_data >> 16) as i16) as f64 / WHEEL_DELTA;
    match message {
        wm::MOUSEWHEEL => Some((0.0, delta, false)),  // 纵向
        wm::MOUSEHWHEEL => Some((delta, 0.0, false)), // 横向
        _ => None,
    }
}

/// 绝对坐标归一化到虚拟桌面 0–65535（TECH-REVIEW C-2）。
/// `(x,y)` 为内部左上原点物理像素，`(vx,vy,vw,vh)` 为虚拟桌面矩形。
pub fn normalize_virtual_desktop(x: f64, y: f64, vx: f64, vy: f64, vw: f64, vh: f64) -> (i32, i32) {
    let nx = if vw > 0.0 {
        ((x - vx) * 65535.0 / vw).round().clamp(0.0, 65535.0)
    } else {
        0.0
    };
    let ny = if vh > 0.0 {
        ((y - vy) * 65535.0 / vh).round().clamp(0.0, 65535.0)
    } else {
        0.0
    };
    (nx as i32, ny as i32)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn 虚拟桌面归一化() {
        // 单屏 1920x1080，原点 0,0
        assert_eq!(
            normalize_virtual_desktop(0.0, 0.0, 0.0, 0.0, 1920.0, 1080.0),
            (0, 0)
        );
        assert_eq!(
            normalize_virtual_desktop(1920.0, 1080.0, 0.0, 0.0, 1920.0, 1080.0),
            (65535, 65535)
        );
        // 中点
        let (mx, _) = normalize_virtual_desktop(960.0, 540.0, 0.0, 0.0, 1920.0, 1080.0);
        assert!((mx - 32768).abs() <= 1);
        // 虚拟桌面带负原点（副屏在左）：x=-1920 是虚拟桌面左边
        assert_eq!(
            normalize_virtual_desktop(-1920.0, 0.0, -1920.0, 0.0, 3840.0, 1080.0),
            (0, 0)
        );
    }

    #[test]
    fn 键盘按下抬起() {
        assert_eq!(key_down(wm::KEYDOWN), Some(true));
        assert_eq!(key_down(wm::SYSKEYDOWN), Some(true));
        assert_eq!(key_down(wm::KEYUP), Some(false));
        assert_eq!(key_down(wm::MOUSEMOVE), None);
    }

    #[test]
    fn 三键与侧键() {
        assert_eq!(mouse_button(wm::LBUTTONDOWN, 0), Some((Button::Left, true)));
        assert_eq!(mouse_button(wm::RBUTTONUP, 0), Some((Button::Right, false)));
        assert_eq!(
            mouse_button(wm::XBUTTONDOWN, (XBUTTON1 as u32) << 16),
            Some((Button::X1, true))
        );
        assert_eq!(
            mouse_button(wm::XBUTTONUP, (XBUTTON2 as u32) << 16),
            Some((Button::X2, false))
        );
    }

    #[test]
    fn 滚轮方向与符号() {
        // 向上滚一格：mouseData 高位 = +120
        let up = wheel(wm::MOUSEWHEEL, (120u32) << 16).unwrap();
        assert_eq!(up, (0.0, 1.0, false));
        // 向下滚一格：-120（i16）
        let down = wheel(wm::MOUSEWHEEL, ((-120i16) as u16 as u32) << 16).unwrap();
        assert_eq!(down, (0.0, -1.0, false));
        // 横向
        let h = wheel(wm::MOUSEHWHEEL, (120u32) << 16).unwrap();
        assert_eq!(h, (1.0, 0.0, false));
    }
}
