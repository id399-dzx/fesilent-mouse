//! 异步会话驱动：心跳收发 + 重连循环（DEVELOPMENT.md §4.4，FR-602/604/607）。
//!
//! 复用 [`crate::reconnect`] 的纯逻辑（退避/心跳/状态/原因码）；本模块负责
//! 真实定时器与连接收发。控制消息暂用单向流（持久流复用属 T-304 优化）。

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use ms_proto::{Msg, Ping, Pong};
use tokio::sync::watch;
use tokio::time::MissedTickBehavior;

use crate::quic::{Conn, Endpoint};
use crate::reconnect::{Backoff, ConnState, DisconnectReason, HeartbeatTracker};

/// 心跳间隔（FR-601：1s）。
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);
/// 单次连接尝试超时：避免连不上时长时间挂起，尽快进入退避重试。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// 退避等待，可被唤醒通知打断（FR-604）。被打断时清零退避，下次尝试无延迟。
/// 抽出为独立函数以便确定性单测。
pub async fn interruptible_backoff(backoff: &mut Backoff, wake: &tokio::sync::Notify) {
    tokio::select! {
        _ = tokio::time::sleep(backoff.next_delay()) => {}
        _ = wake.notified() => {
            backoff.force_immediate();
            tracing::info!(target: "mouseshare::net", "收到唤醒通知，立即重连");
        }
    }
}

/// 对端最小响应循环：把收到的 Ping 回成 Pong，直到连接断开。
/// 真实会话中由统一的消息分发处理（T-304），此处供心跳/测试用。
pub async fn respond_loop(conn: Conn) {
    loop {
        match conn.recv_uni().await {
            Ok(Msg::Ping(p)) => {
                let _ = conn
                    .send_uni(&Msg::from(Pong {
                        seq: p.seq,
                        t0: p.t0,
                    }))
                    .await;
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
}

/// 驱动一条已建立连接：每秒发 Ping、回应对端 Ping、按 Pong 维持心跳。
/// 返回断连原因。
async fn drive(conn: &Conn) -> DisconnectReason {
    let mut hb = HeartbeatTracker::new();
    let mut ticker = tokio::time::interval(HEARTBEAT_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut seq: u32 = 0;
    loop {
        tokio::select! {
            _ = conn.wait_closed() => return DisconnectReason::QuicError,
            _ = ticker.tick() => {
                seq = seq.wrapping_add(1);
                if conn.send_uni(&Msg::from(Ping { seq, t0: 0 })).await.is_err() {
                    return DisconnectReason::QuicError;
                }
                if hb.tick() {
                    return DisconnectReason::HeartbeatTimeout;
                }
            }
            msg = conn.recv_uni() => match msg {
                Ok(Msg::Pong(_)) => hb.on_pong(),
                Ok(Msg::Ping(p)) => {
                    let _ = conn.send_uni(&Msg::from(Pong { seq: p.seq, t0: p.t0 })).await;
                }
                // 对端睡眠预告（FR-604）：不计为错误，标记 PeerSuspend，
                // UI 显示"等待对端唤醒"，仍走重连等待对端回来
                Ok(Msg::Suspend(_)) => return DisconnectReason::PeerSuspend,
                Ok(_) => {}
                Err(_) => return DisconnectReason::QuicError,
            }
        }
    }
}

/// 重连客户端循环：连接 → 驱动 → 断连按退避重连，直到遇到不可重连原因。
/// 通过 `state_tx` 广播状态变化（UI/测试观察）。
///
/// `wake`：本机从睡眠唤醒时由电源监听 `notify_one()`，立即打断当前退避等待
/// 并清零退避，实现"唤醒后 ≤5s 恢复"（FR-604）。
pub async fn run_reconnecting_client(
    endpoint: Arc<Endpoint>,
    addr: SocketAddr,
    state_tx: watch::Sender<ConnState>,
    wake: Arc<tokio::sync::Notify>,
) {
    let mut backoff = Backoff::new();
    loop {
        let _ = state_tx.send(ConnState::Connecting);
        // 连接尝试加超时，避免连不上时挂起到 idle timeout 才返回
        match tokio::time::timeout(CONNECT_TIMEOUT, endpoint.connect(addr)).await {
            Ok(Ok(conn)) => {
                backoff.reset();
                let _ = state_tx.send(ConnState::Connected);
                let reason = drive(&conn).await;
                tracing::warn!(target: "mouseshare::net", ?reason, "连接断开");
                if !reason.should_reconnect() {
                    break;
                }
            }
            Ok(Err(e)) => {
                tracing::debug!(target: "mouseshare::net", %e, "连接尝试失败，将退避重试");
            }
            Err(_) => {
                tracing::debug!(target: "mouseshare::net", "连接尝试超时，将退避重试");
            }
        }
        let _ = state_tx.send(ConnState::Reconnecting);
        interruptible_backoff(&mut backoff, &wake).await;
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[tokio::test]
    async fn 唤醒打断退避并清零() {
        let mut backoff = Backoff::new();
        // 把退避推到较大值（多次失败后会到秒级）
        for _ in 0..5 {
            backoff.next_delay();
        }
        let wake = tokio::sync::Notify::new();
        wake.notify_one(); // 预置唤醒：等待应被立即打断

        let t = std::time::Instant::now();
        interruptible_backoff(&mut backoff, &wake).await;
        assert!(t.elapsed() < Duration::from_millis(200), "应被唤醒立即打断");
        // 打断后退避被清零，下次延迟为 0
        assert_eq!(backoff.next_delay(), Duration::ZERO);
    }

    #[tokio::test]
    async fn 无唤醒时正常等待退避() {
        let mut backoff = Backoff::new(); // 首次 0.5s
        let wake = tokio::sync::Notify::new();
        let t = std::time::Instant::now();
        interruptible_backoff(&mut backoff, &wake).await;
        assert!(
            t.elapsed() >= Duration::from_millis(450),
            "无唤醒应正常等满退避"
        );
    }
}
