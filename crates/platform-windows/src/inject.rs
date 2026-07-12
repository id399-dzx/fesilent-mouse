//! Windows 键鼠注入（SendInput；DEVELOPMENT.md §3.1，TECH-REVIEW C-2）。
//!
//! 仅在 Windows 编译。键盘用 scancode（KEYEVENTF_SCANCODE，扩展键加 EXTENDEDKEY）；
//! 鼠标绝对移动用 ABSOLUTE|VIRTUALDESK + 0–65535 归一化（多显示器混合 DPI）。
//! 按下状态跟踪保证 release_all 防卡键（FR-305）。

use std::collections::BTreeSet;

use ms_platform::{Button, HidKey, InputInject, Result};

use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSE_EVENT_FLAGS,
    MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN,
    MOUSEEVENTF_XUP, MOUSEINPUT, SendInput,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use crate::event_decode::normalize_virtual_desktop;
use crate::keymap::hid_to_scancode;

const WHEEL_DELTA: i32 = 120;
const XBUTTON1: i32 = 1;
const XBUTTON2: i32 = 2;

pub struct WinInject {
    keys: BTreeSet<u16>,
    buttons: BTreeSet<u8>,
}

impl WinInject {
    pub fn new() -> Self {
        Self {
            keys: BTreeSet::new(),
            buttons: BTreeSet::new(),
        }
    }
}

impl Default for WinInject {
    fn default() -> Self {
        Self::new()
    }
}

fn send(inputs: &[INPUT]) {
    // SAFETY: inputs 为有效 INPUT 切片，cbSize 取结构体大小
    unsafe {
        SendInput(inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

fn key_input(scancode: u16, extended: bool, down: bool) -> INPUT {
    let mut flags = KEYEVENTF_SCANCODE;
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if !down {
        flags |= KEYEVENTF_KEYUP;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: Default::default(),
                wScan: scancode,
                dwFlags: KEYBD_EVENT_FLAGS(flags.0),
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn mouse_input(flags: MOUSE_EVENT_FLAGS, dx: i32, dy: i32, mouse_data: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: mouse_data as u32,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn button_code(btn: Button) -> u8 {
    match btn {
        Button::Left => 0,
        Button::Right => 1,
        Button::Middle => 2,
        Button::X1 => 3,
        Button::X2 => 4,
    }
}

/// 按键对应的 down/up 标志与 X 键数据。
fn button_flags(btn: Button, down: bool) -> (MOUSE_EVENT_FLAGS, i32) {
    match btn {
        Button::Left => (
            if down {
                MOUSEEVENTF_LEFTDOWN
            } else {
                MOUSEEVENTF_LEFTUP
            },
            0,
        ),
        Button::Right => (
            if down {
                MOUSEEVENTF_RIGHTDOWN
            } else {
                MOUSEEVENTF_RIGHTUP
            },
            0,
        ),
        Button::Middle => (
            if down {
                MOUSEEVENTF_MIDDLEDOWN
            } else {
                MOUSEEVENTF_MIDDLEUP
            },
            0,
        ),
        Button::X1 => (
            if down {
                MOUSEEVENTF_XDOWN
            } else {
                MOUSEEVENTF_XUP
            },
            XBUTTON1,
        ),
        Button::X2 => (
            if down {
                MOUSEEVENTF_XDOWN
            } else {
                MOUSEEVENTF_XUP
            },
            XBUTTON2,
        ),
    }
}

impl InputInject for WinInject {
    fn key(&mut self, key: HidKey, down: bool) -> Result<()> {
        match hid_to_scancode(key) {
            Some((sc, ext)) => {
                send(&[key_input(sc, ext, down)]);
                if down {
                    self.keys.insert(key.0);
                } else {
                    self.keys.remove(&key.0);
                }
                Ok(())
            }
            None => {
                tracing::debug!(target: "mouseshare::win", %key, "无对应扫描码，丢弃注入");
                Ok(())
            }
        }
    }

    fn mouse_move_to(&mut self, x: f64, y: f64) -> Result<()> {
        // SAFETY: 查询虚拟桌面尺寸
        let (vx, vy, vw, vh) = unsafe {
            (
                GetSystemMetrics(SM_XVIRTUALSCREEN) as f64,
                GetSystemMetrics(SM_YVIRTUALSCREEN) as f64,
                GetSystemMetrics(SM_CXVIRTUALSCREEN) as f64,
                GetSystemMetrics(SM_CYVIRTUALSCREEN) as f64,
            )
        };
        let (nx, ny) = normalize_virtual_desktop(x, y, vx, vy, vw, vh);
        send(&[mouse_input(
            MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
            nx,
            ny,
            0,
        )]);
        Ok(())
    }

    fn mouse_button(&mut self, btn: Button, down: bool) -> Result<()> {
        let (flags, data) = button_flags(btn, down);
        send(&[mouse_input(flags, 0, 0, data)]);
        if down {
            self.buttons.insert(button_code(btn));
        } else {
            self.buttons.remove(&button_code(btn));
        }
        Ok(())
    }

    fn scroll(&mut self, dx: f64, dy: f64, _precise: bool) -> Result<()> {
        if dy != 0.0 {
            send(&[mouse_input(
                MOUSEEVENTF_WHEEL,
                0,
                0,
                (dy * WHEEL_DELTA as f64).round() as i32,
            )]);
        }
        if dx != 0.0 {
            send(&[mouse_input(
                MOUSEEVENTF_HWHEEL,
                0,
                0,
                (dx * WHEEL_DELTA as f64).round() as i32,
            )]);
        }
        Ok(())
    }

    fn release_all(&mut self) -> Result<()> {
        let keys: Vec<u16> = self.keys.iter().copied().collect();
        for hid in keys {
            if let Some((sc, ext)) = hid_to_scancode(HidKey(hid)) {
                send(&[key_input(sc, ext, false)]);
            }
        }
        self.keys.clear();
        let btns: Vec<u8> = self.buttons.iter().copied().collect();
        for code in btns {
            if let Some(btn) = code_button(code) {
                let (flags, data) = button_flags(btn, false);
                send(&[mouse_input(flags, 0, 0, data)]);
            }
        }
        self.buttons.clear();
        Ok(())
    }
}

fn code_button(code: u8) -> Option<Button> {
    Some(match code {
        0 => Button::Left,
        1 => Button::Right,
        2 => Button::Middle,
        3 => Button::X1,
        4 => Button::X2,
        _ => return None,
    })
}
