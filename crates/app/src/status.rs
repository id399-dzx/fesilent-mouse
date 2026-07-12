//! 连接质量共享状态（T-604，FR-606）。
//!
//! daemon 后台线程在心跳里写入（已连/RTT/累计断连次数），托盘事件循环每秒读出并
//! 刷新一个禁用的状态菜单项。用无锁原子量，跨线程读写无需加锁。

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// RTT 未知哨兵（如尚未连上或刚断开）。
const RTT_UNKNOWN: u32 = u32::MAX;

pub struct ConnStatus {
    connected: AtomicBool,
    /// 平滑 RTT（毫秒）；[`RTT_UNKNOWN`] 表示未知。
    rtt_ms: AtomicU32,
    /// 本次进程生命周期内累计断连次数。
    disconnects: AtomicU32,
}

impl ConnStatus {
    pub fn new() -> Self {
        Self {
            connected: AtomicBool::new(false),
            rtt_ms: AtomicU32::new(RTT_UNKNOWN),
            disconnects: AtomicU32::new(0),
        }
    }

    /// 标记连接状态。断开时 RTT 复位为未知。
    pub fn set_connected(&self, on: bool) {
        self.connected.store(on, Ordering::Relaxed);
        if !on {
            self.rtt_ms.store(RTT_UNKNOWN, Ordering::Relaxed);
        }
    }

    pub fn set_rtt_ms(&self, ms: u32) {
        self.rtt_ms.store(ms, Ordering::Relaxed);
    }

    /// 累计一次断连（仅真断连调用，用户主动暂停不计）。
    pub fn inc_disconnect(&self) {
        self.disconnects.fetch_add(1, Ordering::Relaxed);
    }

    pub fn connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    pub fn rtt_ms(&self) -> Option<u32> {
        let v = self.rtt_ms.load(Ordering::Relaxed);
        (v != RTT_UNKNOWN).then_some(v)
    }

    pub fn disconnects(&self) -> u32 {
        self.disconnects.load(Ordering::Relaxed)
    }

    /// 托盘状态行文字。
    pub fn summary(&self) -> String {
        if self.connected() {
            match self.rtt_ms() {
                Some(ms) => format!("● 已连接 · RTT {ms}ms · 断连 {}", self.disconnects()),
                None => format!("● 已连接 · RTT -- · 断连 {}", self.disconnects()),
            }
        } else {
            format!("○ 未连接 · 断连 {}", self.disconnects())
        }
    }
}

impl Default for ConnStatus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 未连接时摘要无rtt() {
        let s = ConnStatus::new();
        assert!(!s.connected());
        assert_eq!(s.rtt_ms(), None);
        assert_eq!(s.summary(), "○ 未连接 · 断连 0");
    }

    #[test]
    fn 已连接显示rtt与断连数() {
        let s = ConnStatus::new();
        s.set_connected(true);
        s.set_rtt_ms(12);
        assert_eq!(s.rtt_ms(), Some(12));
        assert_eq!(s.summary(), "● 已连接 · RTT 12ms · 断连 0");
    }

    #[test]
    fn 断开复位rtt并累计断连() {
        let s = ConnStatus::new();
        s.set_connected(true);
        s.set_rtt_ms(20);
        s.inc_disconnect();
        s.set_connected(false);
        assert_eq!(s.rtt_ms(), None, "断开后 RTT 应未知");
        assert_eq!(s.disconnects(), 1);
        assert_eq!(s.summary(), "○ 未连接 · 断连 1");
    }
}
