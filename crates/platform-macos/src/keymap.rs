//! macOS 虚拟键码（Carbon kVK_*）↔ USB HID Usage ID（page 0x07）双向映射
//! （DEVELOPMENT.md §3.3；HID 为内部中立键码，SPEC.md §8）。
//!
//! 纯数据模块，不依赖系统 API，故不做 cfg 门控——任何平台都可跑单测。
//! 说明：
//! - Fn 键（vk 0x3F）不可被 event tap 捕获，不在表内（REQUIREMENTS §5）。
//! - CapsLock 在表内正常映射；其按下延迟特性在注入端处理（T-105）。
//! - 媒体键（音量）属 P1，先行收录三键；其余媒体键经 NX_SYSDEFINED，后续处理。

use ms_platform::HidKey;

/// (macOS 虚拟键码, HID Usage ID)。一对一，禁止重复（单测强制）。
#[rustfmt::skip]
const TABLE: &[(u16, u16)] = &[
    // ── 字母（kVK_ANSI_*）──
    (0x00, 0x04), // A
    (0x0B, 0x05), // B
    (0x08, 0x06), // C
    (0x02, 0x07), // D
    (0x0E, 0x08), // E
    (0x03, 0x09), // F
    (0x05, 0x0A), // G
    (0x04, 0x0B), // H
    (0x22, 0x0C), // I
    (0x26, 0x0D), // J
    (0x28, 0x0E), // K
    (0x25, 0x0F), // L
    (0x2E, 0x10), // M
    (0x2D, 0x11), // N
    (0x1F, 0x12), // O
    (0x23, 0x13), // P
    (0x0C, 0x14), // Q
    (0x0F, 0x15), // R
    (0x01, 0x16), // S
    (0x11, 0x17), // T
    (0x20, 0x18), // U
    (0x09, 0x19), // V
    (0x0D, 0x1A), // W
    (0x07, 0x1B), // X
    (0x10, 0x1C), // Y
    (0x06, 0x1D), // Z
    // ── 主键区数字 ──
    (0x12, 0x1E), // 1
    (0x13, 0x1F), // 2
    (0x14, 0x20), // 3
    (0x15, 0x21), // 4
    (0x17, 0x22), // 5
    (0x16, 0x23), // 6
    (0x1A, 0x24), // 7
    (0x1C, 0x25), // 8
    (0x19, 0x26), // 9
    (0x1D, 0x27), // 0
    // ── 控制与标点 ──
    (0x24, 0x28), // Return
    (0x35, 0x29), // Escape
    (0x33, 0x2A), // Delete(退格)
    (0x30, 0x2B), // Tab
    (0x31, 0x2C), // Space
    (0x1B, 0x2D), // Minus -
    (0x18, 0x2E), // Equal =
    (0x21, 0x2F), // LeftBracket [
    (0x1E, 0x30), // RightBracket ]
    (0x2A, 0x31), // Backslash \
    (0x29, 0x33), // Semicolon ;
    (0x27, 0x34), // Quote '
    (0x32, 0x35), // Grave `
    (0x2B, 0x36), // Comma ,
    (0x2F, 0x37), // Period .
    (0x2C, 0x38), // Slash /
    (0x39, 0x39), // CapsLock
    // ── 功能键 F1–F20 ──
    (0x7A, 0x3A), // F1
    (0x78, 0x3B), // F2
    (0x63, 0x3C), // F3
    (0x76, 0x3D), // F4
    (0x60, 0x3E), // F5
    (0x61, 0x3F), // F6
    (0x62, 0x40), // F7
    (0x64, 0x41), // F8
    (0x65, 0x42), // F9
    (0x6D, 0x43), // F10
    (0x67, 0x44), // F11
    (0x6F, 0x45), // F12
    (0x69, 0x68), // F13
    (0x6B, 0x69), // F14
    (0x71, 0x6A), // F15
    (0x6A, 0x6B), // F16
    (0x40, 0x6C), // F17
    (0x4F, 0x6D), // F18
    (0x50, 0x6E), // F19
    (0x5A, 0x6F), // F20
    // ── 导航区 ──
    (0x72, 0x75), // Help（旧键盘；HID Help）
    (0x73, 0x4A), // Home
    (0x74, 0x4B), // PageUp
    (0x75, 0x4C), // ForwardDelete
    (0x77, 0x4D), // End
    (0x79, 0x4E), // PageDown
    (0x7C, 0x4F), // →
    (0x7B, 0x50), // ←
    (0x7D, 0x51), // ↓
    (0x7E, 0x52), // ↑
    // ── 小键盘 ──
    (0x47, 0x53), // KeypadClear → NumLock 位
    (0x4B, 0x54), // Kp /
    (0x43, 0x55), // Kp *
    (0x4E, 0x56), // Kp -
    (0x45, 0x57), // Kp +
    (0x4C, 0x58), // Kp Enter
    (0x53, 0x59), // Kp 1
    (0x54, 0x5A), // Kp 2
    (0x55, 0x5B), // Kp 3
    (0x56, 0x5C), // Kp 4
    (0x57, 0x5D), // Kp 5
    (0x58, 0x5E), // Kp 6
    (0x59, 0x5F), // Kp 7
    (0x5B, 0x60), // Kp 8
    (0x5C, 0x61), // Kp 9
    (0x52, 0x62), // Kp 0
    (0x41, 0x63), // Kp .
    (0x51, 0x67), // Kp =
    // ── 修饰键（按物理位置；功能映射在注入端做，DEVELOPMENT §3.3）──
    (0x3B, 0xE0), // LeftControl
    (0x38, 0xE1), // LeftShift
    (0x3A, 0xE2), // LeftOption → LAlt
    (0x37, 0xE3), // LeftCommand → LGUI
    (0x3E, 0xE4), // RightControl
    (0x3C, 0xE5), // RightShift
    (0x3D, 0xE6), // RightOption → RAlt
    (0x36, 0xE7), // RightCommand → RGUI
    // ── ISO/JIS 与杂项 ──
    (0x0A, 0x64), // ISO Section → NonUS Backslash
    (0x6E, 0x65), // ContextMenu → Application
    (0x5D, 0x89), // JIS Yen → International3
    (0x5E, 0x87), // JIS Underscore → International1
    (0x5F, 0x85), // JIS KeypadComma
    (0x68, 0x90), // JIS Kana → LANG1
    (0x66, 0x91), // JIS Eisu → LANG2
    // ── 媒体键（P1 先行收录）──
    (0x48, 0x80), // VolumeUp
    (0x49, 0x81), // VolumeDown
    (0x4A, 0x7F), // Mute
];

/// macOS 虚拟键码 → HID。未知键返回 None（调用方记日志并忽略）。
pub fn vk_to_hid(vk: u16) -> Option<HidKey> {
    TABLE
        .iter()
        .find(|(v, _)| *v == vk)
        .map(|(_, h)| HidKey(*h))
}

/// HID → macOS 虚拟键码（注入端用）。
pub fn hid_to_vk(hid: HidKey) -> Option<u16> {
    TABLE.iter().find(|(_, h)| *h == hid.0).map(|(v, _)| *v)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn 表内无重复键码() {
        let mut vks = HashSet::new();
        let mut hids = HashSet::new();
        for (vk, hid) in TABLE {
            assert!(vks.insert(*vk), "vk 0x{vk:02X} 重复");
            assert!(hids.insert(*hid), "hid 0x{hid:02X} 重复");
        }
    }

    #[test]
    fn 全表双向roundtrip() {
        for (vk, hid) in TABLE {
            assert_eq!(vk_to_hid(*vk), Some(HidKey(*hid)));
            assert_eq!(hid_to_vk(HidKey(*hid)), Some(*vk));
        }
    }

    #[test]
    fn 关键键位抽查() {
        assert_eq!(vk_to_hid(0x00), Some(HidKey(0x04))); // A
        assert_eq!(vk_to_hid(0x37), Some(HidKey(0xE3))); // LeftCommand→LGUI
        assert_eq!(vk_to_hid(0x7E), Some(HidKey(0x52))); // ↑
        assert_eq!(hid_to_vk(HidKey(0x2C)), Some(0x31)); // Space
        assert_eq!(vk_to_hid(0x3F), None); // Fn 不可映射
    }

    #[test]
    fn 覆盖率_标准全尺寸键盘主要键全部在表() {
        // ANSI 全尺寸键盘除 Fn 外的全部 vk（字母26+数字10+标点11+控制6+
        // F1-F20+导航10+小键盘18+修饰8+杂项），共 117 项
        assert!(TABLE.len() >= 117, "映射表仅 {} 项", TABLE.len());
    }
}
