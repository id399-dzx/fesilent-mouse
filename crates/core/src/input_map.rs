//! 修饰键映射、全局热键、边缘触发延迟（FR-205/207/302，DEVELOPMENT.md §3.3）。
//! 均为纯逻辑（热键/边缘延迟用注入时间戳），便于确定性单测。

use std::collections::BTreeSet;

/// 真实会话使用的可配置组合键；修饰键按左右任意一侧匹配。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shortcut {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
    pub key: u16,
}

impl Shortcut {
    pub fn valid(self) -> bool {
        self.key >= 4
            && self.key < 0xE0
            && [self.ctrl, self.alt, self.shift, self.meta]
                .into_iter()
                .filter(|v| *v)
                .count()
                >= 2
    }
}

/// 固定容量按键状态，捕获消费路径不为每次按键分配内存。
pub struct ShortcutTracker {
    pressed: [bool; 256],
}

impl Default for ShortcutTracker {
    fn default() -> Self {
        Self {
            pressed: [false; 256],
        }
    }
}

impl ShortcutTracker {
    pub fn pressed_modifiers(&self) -> impl Iterator<Item = u16> + '_ {
        (0xE0..=0xE7).filter(|hid| self.pressed[*hid as usize])
    }
    pub fn update(&mut self, hid: u16, down: bool, binding: Shortcut) -> bool {
        let Some(previous) = self.pressed.get_mut(hid as usize) else {
            return false;
        };
        let repeated = *previous;
        *previous = down;
        let held = |a: usize, b: usize| self.pressed[a] || self.pressed[b];
        down && !repeated
            && hid == binding.key
            && binding.valid()
            && held(0xE0, 0xE4) == binding.ctrl
            && held(0xE2, 0xE6) == binding.alt
            && held(0xE1, 0xE5) == binding.shift
            && held(0xE3, 0xE7) == binding.meta
    }

    pub fn reset(&mut self) {
        self.pressed.fill(false);
    }
}

// HID 修饰键码（page 0x07）
const HID_LCTRL: u16 = 0xE0;
const HID_LALT: u16 = 0xE2;
const HID_LGUI: u16 = 0xE3;
const HID_RCTRL: u16 = 0xE4;
const HID_RALT: u16 = 0xE6;
const HID_RGUI: u16 = 0xE7;
// 方向键
const HID_RIGHT: u16 = 0x4F;
const HID_LEFT: u16 = 0x50;
const HID_DOWN: u16 = 0x51;
const HID_UP: u16 = 0x52;

// ───────────────────────── 修饰键映射 ─────────────────────────

/// 修饰键映射模式（FR-302），在**注入端**应用。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModifierMode {
    /// 按物理位置（默认）：Win↔Cmd、Alt↔Option 已由键码表处理，这里恒等。
    Position,
    /// 按功能：交换 Ctrl↔GUI，使 Ctrl+C 在 Mac 变 Cmd+C。
    SwapCtrlGui,
}

/// 注入前把 HID 键按模式重映射。非修饰键原样返回。
pub fn map_modifier(mode: ModifierMode, hid: u16) -> u16 {
    match mode {
        ModifierMode::Position => hid,
        ModifierMode::SwapCtrlGui => match hid {
            HID_LCTRL => HID_LGUI,
            HID_LGUI => HID_LCTRL,
            HID_RCTRL => HID_RGUI,
            HID_RGUI => HID_RCTRL,
            other => other,
        },
    }
}

// ───────────────────────── 全局热键 ─────────────────────────

/// 热键触发的切换方向（FR-207：Ctrl+Alt+方向键）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyAction {
    Left,
    Right,
    Up,
    Down,
}

/// 热键检测器：跟踪按下的键，Ctrl+Alt 同时按住时按下方向键即触发。
#[derive(Debug, Default)]
pub struct HotkeyDetector {
    pressed: BTreeSet<u16>,
}

impl HotkeyDetector {
    pub fn new() -> Self {
        Self::default()
    }

    fn ctrl_held(&self) -> bool {
        self.pressed.contains(&HID_LCTRL) || self.pressed.contains(&HID_RCTRL)
    }
    fn alt_held(&self) -> bool {
        self.pressed.contains(&HID_LALT) || self.pressed.contains(&HID_RALT)
    }

    /// 喂入一个按键事件；当 Ctrl+Alt 已按住且此刻按下方向键，返回切换动作。
    pub fn on_key(&mut self, hid: u16, down: bool) -> Option<HotkeyAction> {
        if down {
            // 在更新 pressed 之前判断修饰键是否已经按住
            let combo = self.ctrl_held() && self.alt_held();
            self.pressed.insert(hid);
            if combo {
                return match hid {
                    HID_LEFT => Some(HotkeyAction::Left),
                    HID_RIGHT => Some(HotkeyAction::Right),
                    HID_UP => Some(HotkeyAction::Up),
                    HID_DOWN => Some(HotkeyAction::Down),
                    _ => None,
                };
            }
        } else {
            self.pressed.remove(&hid);
        }
        None
    }
}

// ───────────────────────── 边缘触发延迟 ─────────────────────────

/// 边缘停留触发器（FR-205）：光标须在边缘停留 `dwell_ms` 才切换；
/// 可选"按住某键才允许切换"防误触。用毫秒时间戳，确定性可测。
#[derive(Debug)]
pub struct EdgeTrigger {
    dwell_ms: u64,
    require_key: bool,
    since: Option<u64>,
    fired: bool,
}

impl EdgeTrigger {
    pub fn new(dwell_ms: u64, require_key: bool) -> Self {
        Self {
            dwell_ms,
            require_key,
            since: None,
            fired: false,
        }
    }

    /// 更新状态，返回本次是否应触发切换（仅在满足停留时长的那一刻返回一次 true）。
    pub fn update(&mut self, at_edge: bool, key_held: bool, now_ms: u64) -> bool {
        let allowed = at_edge && (!self.require_key || key_held);
        if !allowed {
            self.since = None;
            self.fired = false;
            return false;
        }
        let start = *self.since.get_or_insert(now_ms);
        if !self.fired && now_ms.saturating_sub(start) >= self.dwell_ms {
            self.fired = true;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 位置模式恒等() {
        assert_eq!(map_modifier(ModifierMode::Position, HID_LCTRL), HID_LCTRL);
        assert_eq!(map_modifier(ModifierMode::Position, 0x04), 0x04);
    }

    #[test]
    fn 功能模式交换ctrl与gui() {
        assert_eq!(map_modifier(ModifierMode::SwapCtrlGui, HID_LCTRL), HID_LGUI);
        assert_eq!(map_modifier(ModifierMode::SwapCtrlGui, HID_LGUI), HID_LCTRL);
        assert_eq!(map_modifier(ModifierMode::SwapCtrlGui, HID_RCTRL), HID_RGUI);
        // 非 Ctrl/GUI 不变（如 Shift、字母）
        assert_eq!(map_modifier(ModifierMode::SwapCtrlGui, 0xE1), 0xE1);
        assert_eq!(map_modifier(ModifierMode::SwapCtrlGui, 0x04), 0x04);
    }

    #[test]
    fn 热键ctrl_alt加方向触发() {
        let mut h = HotkeyDetector::new();
        assert_eq!(h.on_key(HID_LCTRL, true), None);
        assert_eq!(h.on_key(HID_LALT, true), None);
        assert_eq!(h.on_key(HID_RIGHT, true), Some(HotkeyAction::Right));
        assert_eq!(h.on_key(HID_LEFT, true), Some(HotkeyAction::Left));
    }

    #[test]
    fn 缺修饰键不触发() {
        let mut h = HotkeyDetector::new();
        h.on_key(HID_LCTRL, true); // 只按 Ctrl，没按 Alt
        assert_eq!(h.on_key(HID_RIGHT, true), None);
    }

    #[test]
    fn 释放修饰键后不再触发() {
        let mut h = HotkeyDetector::new();
        h.on_key(HID_LCTRL, true);
        h.on_key(HID_LALT, true);
        h.on_key(HID_LALT, false); // 松开 Alt
        assert_eq!(h.on_key(HID_UP, true), None);
    }

    #[test]
    fn 边缘停留满时长才触发一次() {
        let mut t = EdgeTrigger::new(200, false);
        assert!(!t.update(true, false, 1000)); // 刚到边缘
        assert!(!t.update(true, false, 1100)); // 才 100ms
        assert!(t.update(true, false, 1200)); // 满 200ms → 触发
        assert!(!t.update(true, false, 1300)); // 已触发，不重复
    }

    #[test]
    fn 离开边缘重置计时() {
        let mut t = EdgeTrigger::new(200, false);
        t.update(true, false, 1000);
        assert!(!t.update(false, false, 1100)); // 离开边缘
        // 重新到边缘，计时从头
        assert!(!t.update(true, false, 1150));
        assert!(!t.update(true, false, 1300)); // 距重新开始才 150ms
        assert!(t.update(true, false, 1360)); // 满 200ms
    }

    #[test]
    fn 要求按键时未按住不触发() {
        let mut t = EdgeTrigger::new(0, true);
        assert!(!t.update(true, false, 1000), "未按住要求键不切换");
        assert!(
            t.update(true, true, 1000),
            "按住要求键且在边缘立即切换(dwell=0)"
        );
    }
}
