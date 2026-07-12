//! Windows 扫描码（PS/2 Set 1）↔ USB HID Usage ID 双向映射（DEVELOPMENT.md §3.3）。
//!
//! 纯数据模块，不依赖系统 API，任何平台可单测。
//! 低级键盘钩子 `KBDLLHOOKSTRUCT` 提供 scanCode + 扩展标志（LLKHF_EXTENDED）；
//! 扩展键（右 Ctrl/Alt、方向、导航、Win 等）与主键区扫描码会重叠，靠扩展标志区分。
//! 注入端 `SendInput` 用 `KEYEVENTF_SCANCODE`（扩展键加 `KEYEVENTF_EXTENDEDKEY`）。

use ms_platform::HidKey;

/// (扫描码, 是否扩展键, HID Usage ID)。
#[rustfmt::skip]
const TABLE: &[(u16, bool, u16)] = &[
    // ── 主键区（非扩展）──
    (0x01, false, 0x29), // Esc
    (0x02, false, 0x1E), // 1
    (0x03, false, 0x1F), // 2
    (0x04, false, 0x20), // 3
    (0x05, false, 0x21), // 4
    (0x06, false, 0x22), // 5
    (0x07, false, 0x23), // 6
    (0x08, false, 0x24), // 7
    (0x09, false, 0x25), // 8
    (0x0A, false, 0x26), // 9
    (0x0B, false, 0x27), // 0
    (0x0C, false, 0x2D), // -
    (0x0D, false, 0x2E), // =
    (0x0E, false, 0x2A), // Backspace
    (0x0F, false, 0x2B), // Tab
    (0x10, false, 0x14), // Q
    (0x11, false, 0x1A), // W
    (0x12, false, 0x08), // E
    (0x13, false, 0x15), // R
    (0x14, false, 0x17), // T
    (0x15, false, 0x1C), // Y
    (0x16, false, 0x18), // U
    (0x17, false, 0x0C), // I
    (0x18, false, 0x12), // O
    (0x19, false, 0x13), // P
    (0x1A, false, 0x2F), // [
    (0x1B, false, 0x30), // ]
    (0x1C, false, 0x28), // Enter
    (0x1D, false, 0xE0), // LCtrl
    (0x1E, false, 0x04), // A
    (0x1F, false, 0x16), // S
    (0x20, false, 0x07), // D
    (0x21, false, 0x09), // F
    (0x22, false, 0x0A), // G
    (0x23, false, 0x0B), // H
    (0x24, false, 0x0D), // J
    (0x25, false, 0x0E), // K
    (0x26, false, 0x0F), // L
    (0x27, false, 0x33), // ;
    (0x28, false, 0x34), // '
    (0x29, false, 0x35), // `
    (0x2A, false, 0xE1), // LShift
    (0x2B, false, 0x31), // backslash
    (0x2C, false, 0x1D), // Z
    (0x2D, false, 0x1B), // X
    (0x2E, false, 0x06), // C
    (0x2F, false, 0x19), // V
    (0x30, false, 0x05), // B
    (0x31, false, 0x11), // N
    (0x32, false, 0x10), // M
    (0x33, false, 0x36), // ,
    (0x34, false, 0x37), // .
    (0x35, false, 0x38), // /
    (0x36, false, 0xE5), // RShift
    (0x37, false, 0x55), // Kp*
    (0x38, false, 0xE2), // LAlt
    (0x39, false, 0x2C), // Space
    (0x3A, false, 0x39), // CapsLock
    (0x3B, false, 0x3A), // F1
    (0x3C, false, 0x3B), // F2
    (0x3D, false, 0x3C), // F3
    (0x3E, false, 0x3D), // F4
    (0x3F, false, 0x3E), // F5
    (0x40, false, 0x3F), // F6
    (0x41, false, 0x40), // F7
    (0x42, false, 0x41), // F8
    (0x43, false, 0x42), // F9
    (0x44, false, 0x43), // F10
    (0x45, false, 0x53), // NumLock
    (0x46, false, 0x47), // ScrollLock
    (0x47, false, 0x5F), // Kp7
    (0x48, false, 0x60), // Kp8
    (0x49, false, 0x61), // Kp9
    (0x4A, false, 0x56), // Kp-
    (0x4B, false, 0x5C), // Kp4
    (0x4C, false, 0x5D), // Kp5
    (0x4D, false, 0x5E), // Kp6
    (0x4E, false, 0x57), // Kp+
    (0x4F, false, 0x59), // Kp1
    (0x50, false, 0x5A), // Kp2
    (0x51, false, 0x5B), // Kp3
    (0x52, false, 0x62), // Kp0
    (0x53, false, 0x63), // Kp.
    (0x56, false, 0x64), // ISO backslash (102 键)
    (0x57, false, 0x44), // F11
    (0x58, false, 0x45), // F12
    // ── 扩展键（E0 前缀，ext=true）──
    (0x1C, true, 0x58), // KpEnter
    (0x1D, true, 0xE4), // RCtrl
    (0x35, true, 0x54), // Kp/
    (0x38, true, 0xE6), // RAlt
    (0x47, true, 0x4A), // Home
    (0x48, true, 0x52), // Up
    (0x49, true, 0x4B), // PageUp
    (0x4B, true, 0x50), // Left
    (0x4D, true, 0x4F), // Right
    (0x4F, true, 0x4D), // End
    (0x50, true, 0x51), // Down
    (0x51, true, 0x4E), // PageDown
    (0x52, true, 0x49), // Insert
    (0x53, true, 0x4C), // Delete
    (0x5B, true, 0xE3), // LWin → LGUI
    (0x5C, true, 0xE7), // RWin → RGUI
    (0x5D, true, 0x65), // Menu/Apps
];

/// Windows 虚拟键码 → HID（仅修饰键）。vkCode 对左右修饰键区分明确无歧义，比
/// scanCode+extended 可靠（右 Shift 在部分键盘/驱动下 scanCode 异常，靠 vkCode 兜底）。
pub fn vk_modifier_to_hid(vk: u32) -> Option<HidKey> {
    Some(HidKey(match vk {
        0xA0 => 0xE1, // VK_LSHIFT
        0xA1 => 0xE5, // VK_RSHIFT
        0xA2 => 0xE0, // VK_LCONTROL
        0xA3 => 0xE4, // VK_RCONTROL
        0xA4 => 0xE2, // VK_LMENU（左 Alt）
        0xA5 => 0xE6, // VK_RMENU（右 Alt）
        0x5B => 0xE3, // VK_LWIN
        0x5C => 0xE7, // VK_RWIN
        _ => return None,
    }))
}

/// 扫描码 + 扩展标志 → HID。
pub fn scancode_to_hid(scancode: u16, extended: bool) -> Option<HidKey> {
    TABLE
        .iter()
        .find(|(sc, ext, _)| *sc == scancode && *ext == extended)
        .map(|(_, _, h)| HidKey(*h))
}

/// HID → (扫描码, 是否扩展键)（注入端用）。
pub fn hid_to_scancode(hid: HidKey) -> Option<(u16, bool)> {
    TABLE
        .iter()
        .find(|(_, _, h)| *h == hid.0)
        .map(|(sc, ext, _)| (*sc, *ext))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn 无重复键() {
        let mut keys = HashSet::new();
        let mut hids = HashSet::new();
        for (sc, ext, hid) in TABLE {
            assert!(keys.insert((*sc, *ext)), "(0x{sc:02X},{ext}) 重复");
            assert!(hids.insert(*hid), "HID 0x{hid:02X} 重复");
        }
    }

    #[test]
    fn 全表双向roundtrip() {
        for (sc, ext, hid) in TABLE {
            assert_eq!(scancode_to_hid(*sc, *ext), Some(HidKey(*hid)));
            assert_eq!(hid_to_scancode(HidKey(*hid)), Some((*sc, *ext)));
        }
    }

    #[test]
    fn 扩展标志区分重叠扫描码() {
        // 0x1D 非扩展=LCtrl(0xE0)，扩展=RCtrl(0xE4)
        assert_eq!(scancode_to_hid(0x1D, false), Some(HidKey(0xE0)));
        assert_eq!(scancode_to_hid(0x1D, true), Some(HidKey(0xE4)));
        // 0x48 非扩展=Kp8(0x60)，扩展=Up(0x52)
        assert_eq!(scancode_to_hid(0x48, false), Some(HidKey(0x60)));
        assert_eq!(scancode_to_hid(0x48, true), Some(HidKey(0x52)));
    }

    #[test]
    fn vk修饰键映射左右无歧义() {
        // 右 Shift 真机不工作的回归：vkCode 优先识别修饰键，左右区分准确
        assert_eq!(
            vk_modifier_to_hid(0xA1),
            Some(HidKey(0xE5)),
            "VK_RSHIFT→RShift"
        );
        assert_eq!(
            vk_modifier_to_hid(0xA0),
            Some(HidKey(0xE1)),
            "VK_LSHIFT→LShift"
        );
        assert_eq!(
            vk_modifier_to_hid(0xA2),
            Some(HidKey(0xE0)),
            "VK_LCONTROL→LCtrl"
        );
        assert_eq!(
            vk_modifier_to_hid(0xA3),
            Some(HidKey(0xE4)),
            "VK_RCONTROL→RCtrl"
        );
        assert_eq!(
            vk_modifier_to_hid(0xA4),
            Some(HidKey(0xE2)),
            "VK_LMENU→LAlt"
        );
        assert_eq!(
            vk_modifier_to_hid(0xA5),
            Some(HidKey(0xE6)),
            "VK_RMENU→RAlt"
        );
        assert_eq!(vk_modifier_to_hid(0x41), None, "非修饰键(A)无 vk 映射");
    }

    #[test]
    fn 关键键抽查() {
        assert_eq!(scancode_to_hid(0x1E, false), Some(HidKey(0x04))); // A
        assert_eq!(scancode_to_hid(0x39, false), Some(HidKey(0x2C))); // Space
        assert_eq!(hid_to_scancode(HidKey(0x4F)), Some((0x4D, true))); // Right(扩展)
        assert_eq!(scancode_to_hid(0xFF, false), None);
    }
}
