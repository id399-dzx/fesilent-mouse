//! 连接质量共享状态（T-604，FR-606）。
//!
//! daemon 后台线程在心跳里写入（已连/RTT/累计断连次数），托盘事件循环每秒读出并
//! 刷新状态。频繁更新的数值使用原子量，低频错误文案使用 Mutex。

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use ms_core::control::ControlState;

/// RTT 未知哨兵（如尚未连上或刚断开）。
const RTT_UNKNOWN: u32 = u32::MAX;

/// 真实连接阶段；监听等待、首次核对与重连不能都显示成「正在连接」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ConnectionPhase {
    Idle,
    Discovering,
    Listening,
    Connecting,
    Pairing,
    Connected,
    Reconnecting,
    PermissionRequired,
    Failed,
}

/// 只有实际 TLS 连接产生的配对请求才会显示；不推测对端系统或授权状态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingRequest {
    pub sas: String,
    pub address: String,
}

struct PendingPairing {
    request: PairingRequest,
    decision: Option<bool>,
}

pub struct ConnStatus {
    connected: AtomicBool,
    /// 平滑 RTT（毫秒）；[`RTT_UNKNOWN`] 表示未知。
    rtt_ms: AtomicU32,
    /// 本次进程生命周期内累计断连次数。
    disconnects: AtomicU32,
    last_error: Mutex<Option<String>>,
    phase: AtomicU8,
    control_state: AtomicU8,
    peer_address: Mutex<Option<String>>,
    interactive_pairing: AtomicBool,
    pairing: Mutex<Option<PendingPairing>>,
    peer_fingerprint: Mutex<Option<[u8; 32]>>,
    retry_at: Mutex<Option<Instant>>,
    locked: AtomicBool,
    lock_request: AtomicU8,
    add_device: AtomicBool,
}

impl ConnStatus {
    pub fn new() -> Self {
        Self {
            connected: AtomicBool::new(false),
            rtt_ms: AtomicU32::new(RTT_UNKNOWN),
            disconnects: AtomicU32::new(0),
            last_error: Mutex::new(None),
            phase: AtomicU8::new(ConnectionPhase::Idle as u8),
            control_state: AtomicU8::new(0),
            peer_address: Mutex::new(None),
            interactive_pairing: AtomicBool::new(false),
            pairing: Mutex::new(None),
            peer_fingerprint: Mutex::new(None),
            retry_at: Mutex::new(None),
            locked: AtomicBool::new(false),
            lock_request: AtomicU8::new(0),
            add_device: AtomicBool::new(false),
        }
    }

    /// 标记连接状态。断开时 RTT 复位为未知。
    pub fn set_connected(&self, on: bool) {
        if on {
            *self.retry_at.lock().unwrap_or_else(|e| e.into_inner()) = None;
            self.clear_error();
            self.phase
                .store(ConnectionPhase::Connected as u8, Ordering::Relaxed);
        }
        self.connected.store(on, Ordering::Relaxed);
        if !on {
            *self
                .peer_fingerprint
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = None;
            self.locked.store(false, Ordering::Release);
            self.rtt_ms.store(RTT_UNKNOWN, Ordering::Relaxed);
            self.control_state.store(0, Ordering::Relaxed);
            *self.peer_address.lock().unwrap_or_else(|e| e.into_inner()) = None;
            self.clear_pairing();
            // 收尾不能把 Failed/PermissionRequired 擦成 Idle，界面还需展示恢复入口。
            if !matches!(
                self.phase(),
                ConnectionPhase::Failed | ConnectionPhase::PermissionRequired
            ) {
                self.set_phase(ConnectionPhase::Idle);
            }
        }
    }

    pub fn phase(&self) -> ConnectionPhase {
        match self.phase.load(Ordering::Relaxed) {
            1 => ConnectionPhase::Discovering,
            2 => ConnectionPhase::Listening,
            3 => ConnectionPhase::Connecting,
            4 => ConnectionPhase::Pairing,
            5 => ConnectionPhase::Connected,
            6 => ConnectionPhase::Reconnecting,
            7 => ConnectionPhase::PermissionRequired,
            8 => ConnectionPhase::Failed,
            _ => ConnectionPhase::Idle,
        }
    }

    pub(crate) fn set_phase(&self, phase: ConnectionPhase) {
        self.phase.store(phase as u8, Ordering::Relaxed);
    }

    /// 当前控制权直接来自引擎，不通过按键活动或窗口焦点猜测。
    pub fn control_state(&self) -> ControlState {
        match self.control_state.load(Ordering::Relaxed) {
            1 => ControlState::Remote,
            2 => ControlState::Controlled,
            _ => ControlState::Local,
        }
    }

    pub(crate) fn set_control_state(&self, state: ControlState) {
        let value = match state {
            ControlState::Local => 0,
            ControlState::Remote => 1,
            ControlState::Controlled => 2,
        };
        self.control_state.store(value, Ordering::Relaxed);
    }

    pub fn peer_address(&self) -> Option<String> {
        self.peer_address
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub(crate) fn set_peer_address(&self, address: String) {
        *self.peer_address.lock().unwrap_or_else(|e| e.into_inner()) = Some(address);
    }

    pub fn peer_fingerprint(&self) -> Option<[u8; 32]> {
        *self
            .peer_fingerprint
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }
    pub(crate) fn set_peer_fingerprint(&self, fp: [u8; 32]) {
        *self
            .peer_fingerprint
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(fp);
    }
    pub(crate) fn retry_after(&self, delay: Duration) {
        *self.retry_at.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now() + delay);
        self.set_phase(ConnectionPhase::Reconnecting);
    }
    pub fn retry_seconds(&self) -> Option<u64> {
        self.retry_at
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .map(|t| t.saturating_duration_since(Instant::now()).as_secs() + 1)
    }
    pub fn local_locked(&self) -> bool {
        self.locked.load(Ordering::Acquire)
    }
    pub fn request_local_lock(&self, locked: bool) {
        self.lock_request
            .store(if locked { 1 } else { 2 }, Ordering::Release);
    }
    pub(crate) fn take_lock_request(&self) -> Option<bool> {
        match self.lock_request.swap(0, Ordering::AcqRel) {
            1 => Some(true),
            2 => Some(false),
            _ => None,
        }
    }
    pub(crate) fn set_local_locked(&self, locked: bool) {
        self.locked.store(locked, Ordering::Release);
    }
    pub fn allow_add_device(&self, allowed: bool) {
        self.add_device.store(allowed, Ordering::Release);
    }
    pub(crate) fn adding_device(&self) -> bool {
        self.add_device.load(Ordering::Acquire)
    }

    /// 设置窗启用人工核对；CLI/headless 维持原有 TOFU 调试行为。
    pub fn require_pairing_confirmation(&self, required: bool) {
        self.interactive_pairing.store(required, Ordering::Release);
    }

    pub(crate) fn pairing_confirmation_required(&self) -> bool {
        self.interactive_pairing.load(Ordering::Acquire)
    }

    pub fn pending_pairing(&self) -> Option<PairingRequest> {
        self.pairing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|pending| pending.request.clone())
    }

    pub(crate) fn begin_pairing(&self, request: PairingRequest) {
        self.clear_error();
        *self.pairing.lock().unwrap_or_else(|e| e.into_inner()) = Some(PendingPairing {
            request,
            decision: None,
        });
        self.set_phase(ConnectionPhase::Pairing);
    }

    /// 首次选择生效，防止同一帧或重复点击覆盖已作出的决定。
    pub fn decide_pairing(&self, approved: bool) {
        if let Some(pending) = self
            .pairing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
            && pending.decision.is_none()
        {
            pending.decision = Some(approved);
        }
    }

    pub(crate) fn pairing_decision(&self) -> Option<bool> {
        self.pairing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|pending| pending.decision)
    }

    pub(crate) fn clear_pairing(&self) {
        *self.pairing.lock().unwrap_or_else(|e| e.into_inner()) = None;
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
        self.last_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn clear_error(&self) {
        *self.last_error.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    pub(crate) fn set_error(&self, message: impl Into<String>) {
        self.set_connected(false);
        self.set_phase(ConnectionPhase::Failed);
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
        assert!(
            std::thread::spawn(move || writer.set_error("请在 Mac 端启用互控"))
                .join()
                .is_ok()
        );
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

    #[test]
    fn 断开连接清除控制目标地址与待确认配对() {
        let status = ConnStatus::new();
        status.set_peer_address("192.168.1.20:53728".to_owned());
        status.set_connected(true);
        status.set_control_state(ControlState::Remote);
        status.begin_pairing(PairingRequest {
            sas: "123456".to_owned(),
            address: "192.168.1.20:53728".to_owned(),
        });
        status.set_connected(false);
        assert!(!status.connected());
        assert_eq!(status.control_state(), ControlState::Local);
        assert_eq!(status.peer_address(), None);
        assert_eq!(status.pending_pairing(), None);

        status.set_error("尚未授予辅助功能权限");
        status.set_phase(ConnectionPhase::PermissionRequired);
        status.set_connected(false);
        assert_eq!(status.phase(), ConnectionPhase::PermissionRequired);
        assert!(status.last_error().is_some());
    }

    #[test]
    fn 配对决定不可覆盖且新请求不复用旧决定() {
        let status = ConnStatus::new();
        assert!(!status.pairing_confirmation_required());
        status.require_pairing_confirmation(true);
        assert!(status.pairing_confirmation_required());
        let request = PairingRequest {
            sas: "654321".to_owned(),
            address: "192.168.1.20:53728".to_owned(),
        };
        status.begin_pairing(request.clone());
        assert_eq!(status.phase(), ConnectionPhase::Pairing);
        assert_eq!(status.pending_pairing(), Some(request.clone()));
        assert_eq!(status.pairing_decision(), None);
        status.decide_pairing(false);
        status.decide_pairing(true);
        assert_eq!(status.pairing_decision(), Some(false));

        status.clear_pairing();
        assert_eq!(status.pending_pairing(), None);
        status.begin_pairing(request);
        assert_eq!(status.pairing_decision(), None);
    }
}
