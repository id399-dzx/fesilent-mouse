//! CGEvent 字段 → 中立 `InputEvent` 的**纯解码逻辑**（DEVELOPMENT.md §3.2）。
//!
//! 与 FFI 完全解耦：FFI 回调只负责用 `CGEventGetIntegerValueField` 等取出原始
//! 字段填进 [`RawFields`]，再交给这里解码。如此可在任何平台对解码规则做单测，
//! 不依赖辅助功能权限（真实 tap 运行验证归 H-1）。

use ms_platform::{Button, InputEvent};

use crate::keymap::vk_to_hid;

/// CGEventType 原始值（Quartz Event Services）。
pub mod ev_type {
    pub const LEFT_MOUSE_DOWN: u32 = 1;
    pub const LEFT_MOUSE_UP: u32 = 2;
    pub const RIGHT_MOUSE_DOWN: u32 = 3;
    pub const RIGHT_MOUSE_UP: u32 = 4;
    pub const MOUSE_MOVED: u32 = 5;
    pub const LEFT_MOUSE_DRAGGED: u32 = 6;
    pub const RIGHT_MOUSE_DRAGGED: u32 = 7;
    pub const KEY_DOWN: u32 = 10;
    pub const KEY_UP: u32 = 11;
    pub const FLAGS_CHANGED: u32 = 12;
    pub const SCROLL_WHEEL: u32 = 22;
    pub const OTHER_MOUSE_DOWN: u32 = 25;
    pub const OTHER_MOUSE_UP: u32 = 26;
    pub const OTHER_MOUSE_DRAGGED: u32 = 27;
    /// 回调超时被禁用（TECH-REVIEW A-2）
    pub const TAP_DISABLED_BY_TIMEOUT: u32 = 0xFFFF_FFFE;
    /// 睡眠/唤醒等用户输入导致被禁用（TECH-REVIEW A-2）
    pub const TAP_DISABLED_BY_USER_INPUT: u32 = 0xFFFF_FFFF;
}

/// 设备相关修饰键标志位（NX_DEVICE*KEYMASK），用于在 flagsChanged 事件中
/// 区分左右修饰键的按下/抬起（仅凭 keycode 无法知道方向，需查标志位）。
mod dev_mask {
    pub const L_CTRL: u64 = 0x0000_0001;
    pub const L_SHIFT: u64 = 0x0000_0002;
    pub const R_SHIFT: u64 = 0x0000_0004;
    pub const L_CMD: u64 = 0x0000_0008;
    pub const R_CMD: u64 = 0x0000_0010;
    pub const L_ALT: u64 = 0x0000_0020;
    pub const R_ALT: u64 = 0x0000_0040;
    pub const R_CTRL: u64 = 0x0000_2000;
    /// CapsLock（kCGEventFlagMaskAlphaShift）
    pub const ALPHA_SHIFT: u64 = 0x0001_0000;
}

/// FFI 回调从 CGEvent 取出的原始字段。
#[derive(Debug, Clone, Copy, Default)]
pub struct RawFields {
    pub keycode: u16,
    pub flags: u64,
    pub delta_x: f64,
    pub delta_y: f64,
    /// 绝对光标位置（CGEventGetLocation，全局点坐标）
    pub abs_x: f64,
    pub abs_y: f64,
    /// kCGMouseEventButtonNumber（other mouse 用）
    pub button_number: i64,
    /// 垂直滚动（kCGScrollWheelEventDeltaAxis1，向上为正）
    pub scroll_axis1: f64,
    /// 横向滚动（kCGScrollWheelEventDeltaAxis2，向右为正）
    pub scroll_axis2: f64,
    /// 是否连续（触控板/精密滚轮）→ precise
    pub scroll_continuous: bool,
}

/// 解码结果。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Decoded {
    /// 正常输入事件
    Input(InputEvent),
    /// tap 被系统禁用，需 CGEventTapEnable 重启
    TapDisabled { by_timeout: bool },
    /// 无需转发（未知键、无意义事件）
    Ignore,
}

/// 把某个修饰键 keycode 映射到它对应的设备标志位。非修饰键返回 None。
fn modifier_device_mask(keycode: u16) -> Option<u64> {
    Some(match keycode {
        0x3B => dev_mask::L_CTRL,
        0x38 => dev_mask::L_SHIFT,
        0x3C => dev_mask::R_SHIFT,
        0x37 => dev_mask::L_CMD,
        0x36 => dev_mask::R_CMD,
        0x3A => dev_mask::L_ALT,
        0x3D => dev_mask::R_ALT,
        0x3E => dev_mask::R_CTRL,
        0x39 => dev_mask::ALPHA_SHIFT, // CapsLock：按 flags 反映锁定态
        _ => return None,
    })
}

/// 解码一个原始事件。`etype` 为 CGEventType 原始值。
pub fn decode(etype: u32, f: &RawFields) -> Decoded {
    match etype {
        ev_type::KEY_DOWN | ev_type::KEY_UP => match vk_to_hid(f.keycode) {
            Some(key) => Decoded::Input(InputEvent::Key {
                key,
                down: etype == ev_type::KEY_DOWN,
            }),
            None => Decoded::Ignore,
        },
        ev_type::FLAGS_CHANGED => match (modifier_device_mask(f.keycode), vk_to_hid(f.keycode)) {
            (Some(mask), Some(key)) => {
                // 标志位现在为 1 → 该修饰键处于按下/锁定，否则为抬起
                let down = (f.flags & mask) != 0;
                Decoded::Input(InputEvent::Key { key, down })
            }
            _ => Decoded::Ignore,
        },
        ev_type::MOUSE_MOVED
        | ev_type::LEFT_MOUSE_DRAGGED
        | ev_type::RIGHT_MOUSE_DRAGGED
        | ev_type::OTHER_MOUSE_DRAGGED => Decoded::Input(InputEvent::MouseMove {
            dx: f.delta_x,
            dy: f.delta_y,
            ax: f.abs_x,
            ay: f.abs_y,
        }),
        ev_type::LEFT_MOUSE_DOWN => button(Button::Left, true),
        ev_type::LEFT_MOUSE_UP => button(Button::Left, false),
        ev_type::RIGHT_MOUSE_DOWN => button(Button::Right, true),
        ev_type::RIGHT_MOUSE_UP => button(Button::Right, false),
        ev_type::OTHER_MOUSE_DOWN => other_button(f.button_number, true),
        ev_type::OTHER_MOUSE_UP => other_button(f.button_number, false),
        ev_type::SCROLL_WHEEL => Decoded::Input(InputEvent::Scroll {
            // 横向放 dx、纵向放 dy；CG 向上为正，内部约定保持原值由注入端统一
            dx: f.scroll_axis2,
            dy: f.scroll_axis1,
            precise: f.scroll_continuous,
        }),
        ev_type::TAP_DISABLED_BY_TIMEOUT => Decoded::TapDisabled { by_timeout: true },
        ev_type::TAP_DISABLED_BY_USER_INPUT => Decoded::TapDisabled { by_timeout: false },
        _ => Decoded::Ignore,
    }
}

fn button(btn: Button, down: bool) -> Decoded {
    Decoded::Input(InputEvent::Button { btn, down })
}

/// kCGMouseEventButtonNumber：2=中键，3=侧键1(后退)，4=侧键2(前进)。
fn other_button(num: i64, down: bool) -> Decoded {
    match num {
        2 => button(Button::Middle, down),
        3 => button(Button::X1, down),
        4 => button(Button::X2, down),
        _ => Decoded::Ignore,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields() -> RawFields {
        RawFields::default()
    }

    #[test]
    fn 普通按键_down_up() {
        let f = RawFields {
            keycode: 0x00,
            ..fields()
        }; // A
        assert_eq!(
            decode(ev_type::KEY_DOWN, &f),
            Decoded::Input(InputEvent::Key {
                key: ms_platform::HidKey(0x04),
                down: true
            })
        );
        assert_eq!(
            decode(ev_type::KEY_UP, &f),
            Decoded::Input(InputEvent::Key {
                key: ms_platform::HidKey(0x04),
                down: false
            })
        );
    }

    #[test]
    fn 未知键忽略() {
        let f = RawFields {
            keycode: 0x3F,
            ..fields()
        }; // Fn
        assert_eq!(decode(ev_type::KEY_DOWN, &f), Decoded::Ignore);
    }

    #[test]
    fn 左右command靠标志位区分按下抬起() {
        // 左 Cmd 按下：flags 含 L_CMD 位
        let down = RawFields {
            keycode: 0x37,
            flags: dev_mask::L_CMD,
            ..fields()
        };
        assert_eq!(
            decode(ev_type::FLAGS_CHANGED, &down),
            Decoded::Input(InputEvent::Key {
                key: ms_platform::HidKey(0xE3),
                down: true
            })
        );
        // 左 Cmd 抬起：flags 不含该位
        let up = RawFields {
            keycode: 0x37,
            flags: 0,
            ..fields()
        };
        assert_eq!(
            decode(ev_type::FLAGS_CHANGED, &up),
            Decoded::Input(InputEvent::Key {
                key: ms_platform::HidKey(0xE3),
                down: false
            })
        );
        // 右 Cmd 与左 Cmd 互不干扰：仅左位为 1 时，右 Cmd 事件判为抬起
        let r = RawFields {
            keycode: 0x36,
            flags: dev_mask::L_CMD,
            ..fields()
        };
        assert_eq!(
            decode(ev_type::FLAGS_CHANGED, &r),
            Decoded::Input(InputEvent::Key {
                key: ms_platform::HidKey(0xE7),
                down: false
            })
        );
    }

    #[test]
    fn 鼠标移动与拖拽都产出相对位移() {
        let f = RawFields {
            delta_x: 3.0,
            delta_y: -2.0,
            abs_x: 100.0,
            abs_y: 200.0,
            ..fields()
        };
        for t in [
            ev_type::MOUSE_MOVED,
            ev_type::LEFT_MOUSE_DRAGGED,
            ev_type::OTHER_MOUSE_DRAGGED,
        ] {
            assert_eq!(
                decode(t, &f),
                Decoded::Input(InputEvent::MouseMove {
                    dx: 3.0,
                    dy: -2.0,
                    ax: 100.0,
                    ay: 200.0
                })
            );
        }
    }

    #[test]
    fn 三类鼠标键() {
        assert_eq!(
            decode(ev_type::LEFT_MOUSE_DOWN, &fields()),
            Decoded::Input(InputEvent::Button {
                btn: Button::Left,
                down: true
            })
        );
        let mid = RawFields {
            button_number: 2,
            ..fields()
        };
        assert_eq!(
            decode(ev_type::OTHER_MOUSE_DOWN, &mid),
            Decoded::Input(InputEvent::Button {
                btn: Button::Middle,
                down: true
            })
        );
        let x1 = RawFields {
            button_number: 3,
            ..fields()
        };
        assert_eq!(
            decode(ev_type::OTHER_MOUSE_UP, &x1),
            Decoded::Input(InputEvent::Button {
                btn: Button::X1,
                down: false
            })
        );
    }

    #[test]
    fn 滚轮精密标志() {
        let f = RawFields {
            scroll_axis1: 5.0,
            scroll_axis2: -1.0,
            scroll_continuous: true,
            ..fields()
        };
        assert_eq!(
            decode(ev_type::SCROLL_WHEEL, &f),
            Decoded::Input(InputEvent::Scroll {
                dx: -1.0,
                dy: 5.0,
                precise: true
            })
        );
    }

    #[test]
    fn 两种tap禁用都被识别() {
        assert_eq!(
            decode(ev_type::TAP_DISABLED_BY_TIMEOUT, &fields()),
            Decoded::TapDisabled { by_timeout: true }
        );
        assert_eq!(
            decode(ev_type::TAP_DISABLED_BY_USER_INPUT, &fields()),
            Decoded::TapDisabled { by_timeout: false }
        );
    }
}
