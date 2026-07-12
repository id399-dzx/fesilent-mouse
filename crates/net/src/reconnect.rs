//! 心跳、断连与重连状态机（DEVELOPMENT.md §4.4，FR-6xx 核心）。
//!
//! 本模块为纯逻辑（退避序列、心跳判死、状态转移、reason code），便于确定性单测；
//! 真实异步驱动（定时器、收发 Ping/Pong、重建连接）见 [`crate::session`]。

use std::time::Duration;

/// 断连/回退原因码（SPEC.md §5）。新增只增不改。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisconnectReason {
    HeartbeatTimeout,
    QuicIdle,
    QuicError,
    PeerSuspend,
    PeerShutdown,
    LocalShutdown,
    AuthRejected,
    PermissionRevoked,
    HookLost,
    TapDisabledLoop,
    VersionMismatch,
}

impl DisconnectReason {
    /// 是否应当继续自动重连。用户/本端主动断开、认证失败、版本不兼容不重连。
    pub fn should_reconnect(self) -> bool {
        !matches!(
            self,
            DisconnectReason::LocalShutdown
                | DisconnectReason::PeerShutdown
                | DisconnectReason::AuthRejected
                | DisconnectReason::VersionMismatch
        )
    }
}

/// 连接状态机状态（DEVELOPMENT.md §4.4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnState {
    Discovering,
    Connecting,
    Connected,
    Reconnecting,
}

/// 指数退避（SPEC §2：0.5→1→2→4→8→10→10…，封顶 10s，无限重试）。
#[derive(Debug)]
pub struct Backoff {
    next_ms: u64,
    cap_ms: u64,
    start_ms: u64,
}

impl Backoff {
    pub fn new() -> Self {
        Self {
            next_ms: 500,
            cap_ms: 10_000,
            start_ms: 500,
        }
    }

    /// 取下一次退避时长并翻倍（封顶）。
    pub fn next_delay(&mut self) -> Duration {
        let cur = self.next_ms;
        self.next_ms = (self.next_ms * 2).min(self.cap_ms);
        Duration::from_millis(cur)
    }

    /// 连接成功后复位，下次断连从最短退避重新开始。
    pub fn reset(&mut self) {
        self.next_ms = self.start_ms;
    }

    /// 唤醒后立即重连（FR-604）：跳过退避，本次返回 0。
    pub fn force_immediate(&mut self) {
        self.next_ms = 0;
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new()
    }
}

/// 心跳判死跟踪（FR-601：1s 一个 Ping，连续 3 次无 Pong 判异常）。
///
/// 调用方每秒 `tick()` 一次（发一个 Ping 并推进计数）；收到 Pong 调 `on_pong()`。
/// `tick` 返回 true 表示已判定连接异常（应触发断连回退）。
#[derive(Debug)]
pub struct HeartbeatTracker {
    outstanding: u32,
    max_missed: u32,
}

impl HeartbeatTracker {
    pub fn new() -> Self {
        Self {
            outstanding: 0,
            max_missed: 3,
        }
    }

    /// 发出一个 Ping 的时刻调用：未确认计数 +1；超过阈值判死。
    pub fn tick(&mut self) -> bool {
        self.outstanding += 1;
        self.outstanding >= self.max_missed
    }

    /// 收到任意 Pong：清零未确认计数。
    pub fn on_pong(&mut self) {
        self.outstanding = 0;
    }

    pub fn outstanding(&self) -> u32 {
        self.outstanding
    }
}

impl Default for HeartbeatTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 退避序列符合spec() {
        let mut b = Backoff::new();
        let seq: Vec<u64> = (0..7).map(|_| b.next_delay().as_millis() as u64).collect();
        assert_eq!(seq, vec![500, 1000, 2000, 4000, 8000, 10000, 10000]);
    }

    #[test]
    fn 退避复位回到起点() {
        let mut b = Backoff::new();
        b.next_delay();
        b.next_delay();
        b.reset();
        assert_eq!(b.next_delay().as_millis(), 500);
    }

    #[test]
    fn 唤醒立即重连跳过退避() {
        let mut b = Backoff::new();
        b.next_delay();
        b.next_delay();
        b.force_immediate();
        assert_eq!(b.next_delay().as_millis(), 0);
    }

    #[test]
    fn 心跳连续三次无pong判死() {
        let mut h = HeartbeatTracker::new();
        assert!(!h.tick()); // 1 次未确认
        assert!(!h.tick()); // 2 次
        assert!(h.tick()); // 3 次 → 判死
    }

    #[test]
    fn 心跳收到pong清零不误判() {
        let mut h = HeartbeatTracker::new();
        h.tick();
        h.tick(); // 2 次未确认
        h.on_pong(); // 收到 Pong
        assert_eq!(h.outstanding(), 0);
        // 重新开始计数，单次丢包不误判
        assert!(!h.tick());
        assert!(!h.tick());
        assert!(h.tick());
    }

    #[test]
    fn reason是否重连分类() {
        // 不重连：本端/对端主动、认证失败、版本不兼容
        for r in [
            DisconnectReason::LocalShutdown,
            DisconnectReason::PeerShutdown,
            DisconnectReason::AuthRejected,
            DisconnectReason::VersionMismatch,
        ] {
            assert!(!r.should_reconnect(), "{r:?} 不应重连");
        }
        // 重连：心跳超时、QUIC 错误、睡眠、权限/钩子等故障
        for r in [
            DisconnectReason::HeartbeatTimeout,
            DisconnectReason::QuicIdle,
            DisconnectReason::QuicError,
            DisconnectReason::PeerSuspend,
            DisconnectReason::PermissionRevoked,
            DisconnectReason::HookLost,
            DisconnectReason::TapDisabledLoop,
        ] {
            assert!(r.should_reconnect(), "{r:?} 应重连");
        }
    }
}
