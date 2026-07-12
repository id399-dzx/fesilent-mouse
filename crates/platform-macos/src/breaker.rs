//! tap 禁用熔断器（TECH-REVIEW A-2）。
//!
//! tap 被系统禁用时正常做法是立即 `CGEventTapEnable(true)` 重启；但若回调本身
//! 阻塞会陷入"禁用→重启→再禁用"死循环。本熔断器统计滑动窗口内的禁用次数，
//! 超阈值即跳闸，调用方停止重启并上报错误（DisconnectReason::TapDisabledLoop）。
//!
//! 用 u64 毫秒时间戳而非 Instant，便于确定性单测。

use std::collections::VecDeque;

pub struct DisableBreaker {
    window_ms: u64,
    max: usize,
    hits: VecDeque<u64>,
}

impl DisableBreaker {
    /// 默认：60 秒窗口内最多容忍 3 次禁用（DEVELOPMENT.md §3.2 注意 1）。
    pub fn new() -> Self {
        Self::with(60_000, 3)
    }

    pub fn with(window_ms: u64, max: usize) -> Self {
        Self {
            window_ms,
            max,
            hits: VecDeque::new(),
        }
    }

    /// 记录一次发生在 `now_ms` 的禁用事件。
    /// 返回 `true` 表示仍应重启 tap；`false` 表示已跳闸，应停止重启并上报。
    pub fn record(&mut self, now_ms: u64) -> bool {
        // 移除窗口外的旧记录
        let cutoff = now_ms.saturating_sub(self.window_ms);
        while let Some(&front) = self.hits.front() {
            if front < cutoff {
                self.hits.pop_front();
            } else {
                break;
            }
        }
        self.hits.push_back(now_ms);
        self.hits.len() <= self.max
    }

    /// 是否已跳闸。
    pub fn tripped(&self) -> bool {
        self.hits.len() > self.max
    }
}

impl Default for DisableBreaker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 窗口内未超阈值持续重启() {
        let mut b = DisableBreaker::with(1000, 3);
        assert!(b.record(0));
        assert!(b.record(100));
        assert!(b.record(200)); // 第 3 次，仍 <= max
        assert!(!b.tripped());
    }

    #[test]
    fn 窗口内超阈值跳闸() {
        let mut b = DisableBreaker::with(1000, 3);
        b.record(0);
        b.record(100);
        b.record(200);
        assert!(!b.record(300)); // 第 4 次，跳闸
        assert!(b.tripped());
    }

    #[test]
    fn 旧记录滑出窗口后恢复() {
        let mut b = DisableBreaker::with(1000, 3);
        b.record(0);
        b.record(100);
        b.record(200);
        // 时间推进到旧记录全部滑出窗口
        assert!(b.record(2000)); // 仅剩这一条在窗口内
        assert!(!b.tripped());
    }
}
