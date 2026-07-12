//! 注入端按下状态跟踪（防卡键 FR-305 的纯逻辑）。
//!
//! 注入每个 key/button 的 down/up 时更新集合；`release_all` 据此对仍按下的键
//! 生成抬起。与 FFI 解耦，可在任何平台单测（真实注入回放归 H-5）。

use std::collections::BTreeSet;

use ms_platform::{Button, HidKey};

#[derive(Debug, Default)]
pub struct PressTracker {
    keys: BTreeSet<u16>,
    buttons: BTreeSet<u8>,
}

impl PressTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录一次按键事件。返回是否产生了状态变化（用于幂等去重，可忽略）。
    pub fn on_key(&mut self, key: HidKey, down: bool) -> bool {
        if down {
            self.keys.insert(key.0)
        } else {
            self.keys.remove(&key.0)
        }
    }

    pub fn on_button(&mut self, btn: Button, down: bool) -> bool {
        let b = button_code(btn);
        if down {
            self.buttons.insert(b)
        } else {
            self.buttons.remove(&b)
        }
    }

    /// 取出当前所有按下的键与按钮并清空（release_all 用）。
    /// 返回顺序确定（BTreeSet 升序），便于测试与可复现。
    pub fn drain_pressed(&mut self) -> (Vec<HidKey>, Vec<Button>) {
        let keys = self.keys.iter().map(|k| HidKey(*k)).collect();
        let buttons = self
            .buttons
            .iter()
            .filter_map(|b| code_button(*b))
            .collect();
        self.keys.clear();
        self.buttons.clear();
        (keys, buttons)
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty() && self.buttons.is_empty()
    }
}

/// Button → 稳定数字编码（仅内部跟踪用）。
pub fn button_code(btn: Button) -> u8 {
    match btn {
        Button::Left => 0,
        Button::Right => 1,
        Button::Middle => 2,
        Button::X1 => 3,
        Button::X2 => 4,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 按下后release_all生成抬起() {
        let mut t = PressTracker::new();
        t.on_key(HidKey(0x04), true); // A down
        t.on_key(HidKey(0xE1), true); // LShift down
        t.on_button(Button::Left, true);
        assert!(!t.is_empty());

        let (keys, buttons) = t.drain_pressed();
        assert_eq!(keys, vec![HidKey(0x04), HidKey(0xE1)]);
        assert_eq!(buttons, vec![Button::Left]);
        assert!(t.is_empty(), "drain 后必须清空");
    }

    #[test]
    fn 抬起的键不再出现在release列表() {
        let mut t = PressTracker::new();
        t.on_key(HidKey(0x04), true);
        t.on_key(HidKey(0x04), false); // 已正常抬起
        let (keys, _) = t.drain_pressed();
        assert!(keys.is_empty());
    }

    #[test]
    fn 重复按下幂等() {
        let mut t = PressTracker::new();
        assert!(t.on_key(HidKey(0x04), true));
        assert!(!t.on_key(HidKey(0x04), true)); // 第二次无变化
        let (keys, _) = t.drain_pressed();
        assert_eq!(keys, vec![HidKey(0x04)]); // 仍只需抬起一次
    }

    #[test]
    fn button编码双向一致() {
        for b in [
            Button::Left,
            Button::Right,
            Button::Middle,
            Button::X1,
            Button::X2,
        ] {
            assert_eq!(code_button(button_code(b)), Some(b));
        }
    }
}
