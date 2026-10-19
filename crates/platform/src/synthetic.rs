//! 本应用合成事件标记，避免接收端把自己注入的组合键再次识别为本机快捷键。
//!
//! 这不是认证或权限边界。只识别我们的固定标记；其他程序的合成/辅助输入继续正常处理。

/// 保持在 32 位范围内，同时适配 Win32 dwExtraInfo 与 CoreGraphics 的 i64 user data。
pub const OWN_EVENT_MARKER: u64 = 0x4645_534C; // "FESL"

#[inline]
pub const fn is_own_event(marker: u64) -> bool {
    marker == OWN_EVENT_MARKER
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 仅跳过自家合成事件其他辅助输入仍会被捕获() {
        assert!(is_own_event(OWN_EVENT_MARKER));
        for other in [0, 1, OWN_EVENT_MARKER ^ 1, u64::MAX] {
            assert!(!is_own_event(other));
        }
        assert_eq!(OWN_EVENT_MARKER as u32 as u64, OWN_EVENT_MARKER);
        assert_eq!(OWN_EVENT_MARKER as i64 as u64, OWN_EVENT_MARKER);
    }
}
