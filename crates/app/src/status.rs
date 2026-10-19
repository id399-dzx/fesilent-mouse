//! 连接质量共享状态（T-604，FR-606）。
//!
//! daemon 后台线程在心跳里写入（已连/RTT/累计断连次数），托盘事件循环每秒读出并
//! 刷新状态。频繁更新的数值使用原子量，低频错误文案使用 Mutex。

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;

/// RTT 未知哨兵（如尚未连上或刚断开）。
const RTT_UNKNOWN: u32 = u32::MAX;

pub struct ConnStatus {
    connected: AtomicBool,
    /// 平滑 RTT（毫秒）；[`RTT_UNKNOWN`] 表示未知。
    rtt_ms: AtomicU32,
    /// 本次进程生命周期内累计断连次数。
    disconnects: AtomicU32,
    last_error: Mutex<Option<String>>,
}

impl ConnStatus {
    pub fn new() -> Self {
        Self {
            connected: AtomicBool::new(false),
            rtt_ms: AtomicU32::new(RTT_UNKNOWN),
            disconnects: AtomicU32::new(0),
            last_error: Mutex::new(None),
        }
    }

    /// 标记连接状态。断开时 RTT 复位为未知。
    pub fn set_connected(&self, on: bool) {
        if on {
            self.clear_error();
        }
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

    /// 可直接显示的错误提示；daemon 只写不含路径、证书或原始输入的帮助文案。
    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn clear_error(&self) {
        *self.last_error.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    pub(crate) fn set_error(&self, message: impl Into<String>) {
        self.set_connected(false);
        *self.last_error.lock().unwrap_or_else(|e| e.into_inner()) = Some(message.into());
    }

    /// 托盘状态行文字。
    pub fn summary(&self) -> String {
        if self.connected() {
            match self.rtt_ms() {
                Some(ms) => format!("● 已连接 · RTT {ms}ms · 断连 {}", self.disconnects()),
                None => format!("● 已连接 · RTT -- · 断连 {}", self.disconnects()),
            }
        } else {
            match self.last_error() {
                Some(error) => format!("○ 连接异常 · {error}"),
                None => format!("○ 未连接 · 断连 {}", self.disconnects()),
            }
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

    #[test]
    fn 错误跨线程可读且恢复连接后清除() {
        let s = std::sync::Arc::new(ConnStatus::new());
        s.set_connected(true);
        s.set_rtt_ms(12);
        let writer = std::sync::Arc::clone(&s);
        assert!(std::thread::spawn(move || writer.set_error("请在 Mac 端启用互控")).join().is_ok());
        assert!(!s.connected());
        assert_eq!(s.rtt_ms(), None);
        assert_eq!(s.last_error().as_deref(), Some("请在 Mac 端启用互控"));
        assert!(s.summary().contains("请在 Mac 端启用互控"));
        s.set_connected(true);
        assert_eq!(s.last_error(), None);
        assert!(!s.summary().contains("连接异常"));
        s.set_error("连接中断");
        s.clear_error();
        assert_eq!(s.last_error(), None);
        assert_eq!(s.summary(), "○ 未连接 · 断连 0");
    }
}
