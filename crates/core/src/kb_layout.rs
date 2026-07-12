//! 键盘布局一致性检测（TECH-REVIEW B-3，FR-306 边界）。
//!
//! v1.0 按物理位置转发键码，要求两端键盘布局一致（如都是 QWERTY），否则
//! AZERTY/Dvorak/死键布局下会输出错字。配对握手交换布局标识（Hello.kb_layout），
//! 不一致时进入警告状态，由设置页常驻提示（T-601 消费），但不阻断使用。

/// 布局一致性状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutStatus {
    /// 两端布局一致，按位置转发安全。
    Consistent,
    /// 两端布局不同：记录双方标识供 UI 提示。
    Inconsistent { local: String, peer: String },
}

impl LayoutStatus {
    pub fn is_warning(&self) -> bool {
        matches!(self, LayoutStatus::Inconsistent { .. })
    }
}

/// 比较本端与对端布局标识。空串视为未知，按一致处理（不误报）。
pub fn check_layout(local: &str, peer: &str) -> LayoutStatus {
    if local.is_empty() || peer.is_empty() || local == peer {
        LayoutStatus::Consistent
    } else {
        LayoutStatus::Inconsistent {
            local: local.to_string(),
            peer: peer.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 相同布局一致() {
        let s = check_layout("com.apple.keylayout.ABC", "com.apple.keylayout.ABC");
        assert_eq!(s, LayoutStatus::Consistent);
        assert!(!s.is_warning());
    }

    #[test]
    fn 不同布局产生警告并保留双方标识() {
        let s = check_layout("00000409", "0000040c"); // Win US vs Win 法语
        assert!(s.is_warning());
        match s {
            LayoutStatus::Inconsistent { local, peer } => {
                assert_eq!(local, "00000409");
                assert_eq!(peer, "0000040c");
            }
            _ => panic!("应为不一致"),
        }
    }

    #[test]
    fn 未知布局不误报() {
        assert_eq!(
            check_layout("", "com.apple.keylayout.ABC"),
            LayoutStatus::Consistent
        );
        assert_eq!(check_layout("00000409", ""), LayoutStatus::Consistent);
    }
}
