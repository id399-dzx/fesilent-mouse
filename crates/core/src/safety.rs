//! 安全回退事务（TECH-REVIEW A-1 / B-2，最高优先级安全措施）。
//!
//! 两个故障场景共用同一段"固定顺序、不可重排"的回退代码：
//! - **权限被撤销**（macOS 运行中关闭辅助功能）：注入已失效但 grab 仍在拦截，
//!   若不处理用户整机键鼠冻结（Deskflow #9562）。
//! - **连接断开**（FR-605）：必须立即把控制权还给本机。
//!
//! 顺序契约（不可调整）：
//!   ① 解除 grab（停止拦截本机输入）
//!   ② 解除 confine（恢复本机光标移动）
//!   ③ release_all（释放所有按下键，防卡键 FR-305）
//!   ④ 按场景收尾：权限撤销→停止捕获(tap)；断连→保留捕获等待重连
//!   ⑤ 通知（reason code）

use ms_platform::{InputCapture, InputInject};

/// 回退场景，决定步骤④的收尾动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackKind {
    /// macOS 权限被撤销：捕获已不可信，必须停止 tap。
    PermissionRevoked,
    /// 连接断开：捕获保留（继续运行以待重连），仅交还控制权。
    Disconnected,
}

/// 执行安全回退。各步骤"尽力而为"：单步失败记录但不中断后续步骤
/// （回退本身绝不能因一处失败而半途而废）。
///
/// `notify` 在最后调用，参数为本次回退场景，供上层更新 UI / 打 reason code。
pub fn safe_fallback(
    capture: &mut dyn InputCapture,
    inject: &mut dyn InputInject,
    kind: FallbackKind,
    notify: &mut dyn FnMut(FallbackKind),
) {
    // ① 解除 grab
    if let Err(e) = capture.set_grab(false) {
        tracing::error!(target: "mouseshare::core", %e, "安全回退：解除 grab 失败");
    }
    // ② 解除 confine（恢复本机光标）
    if let Err(e) = capture.confine_cursor(false) {
        tracing::error!(target: "mouseshare::core", %e, "安全回退：解除 confine 失败");
    }
    // ③ 释放所有按键
    if let Err(e) = inject.release_all() {
        tracing::error!(target: "mouseshare::core", %e, "安全回退：release_all 失败");
    }
    // ④ 场景收尾
    if kind == FallbackKind::PermissionRevoked
        && let Err(e) = capture.stop()
    {
        tracing::error!(target: "mouseshare::core", %e, "安全回退：停止捕获失败");
    }
    // ⑤ 通知
    tracing::warn!(target: "mouseshare::core", ?kind, "已执行安全回退");
    notify(kind);
}

/// 权限撤销的边沿检测器（纯逻辑，便于单测）。
///
/// 看门狗每 2s 喂入一次"当前是否仍被授权"，仅在"曾授权→现未授权"的下降沿
/// 返回 true（触发一次回退），避免在持续未授权状态下反复触发。
#[derive(Debug, Default)]
pub struct RevocationDetector {
    was_trusted: bool,
}

impl RevocationDetector {
    /// 初始处于已授权（捕获启动的前提即已授权）。
    pub fn new() -> Self {
        Self { was_trusted: true }
    }

    /// 喂入一次采样；返回 true 表示发生了撤销下降沿，应触发安全回退。
    pub fn tick(&mut self, trusted_now: bool) -> bool {
        let fired = self.was_trusted && !trusted_now;
        self.was_trusted = trusted_now;
        fired
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)] // 测试断言用，非产品代码
    use super::*;
    use ms_platform::stub::{StubCapture, StubInject};
    use std::sync::{Arc, Mutex};

    /// 记录调用顺序的 mock（trait 要求 Send，故用 Arc<Mutex>）。
    #[derive(Clone, Default)]
    struct Recorder(Arc<Mutex<Vec<&'static str>>>);

    impl Recorder {
        fn push(&self, s: &'static str) {
            self.0.lock().expect("mutex poisoned").push(s);
        }
        fn snapshot(&self) -> Vec<&'static str> {
            self.0.lock().expect("mutex poisoned").clone()
        }
    }

    struct MockCapture {
        rec: Recorder,
    }
    struct MockInject {
        rec: Recorder,
    }

    impl InputCapture for MockCapture {
        fn start(&mut self, _sink: ms_platform::EventSink, _grab: bool) -> ms_platform::Result<()> {
            Ok(())
        }
        fn set_grab(&mut self, _grab: bool) -> ms_platform::Result<()> {
            self.rec.push("set_grab(false)");
            Ok(())
        }
        fn stop(&mut self) -> ms_platform::Result<()> {
            self.rec.push("capture.stop");
            Ok(())
        }
        fn confine_cursor(&mut self, _on: bool) -> ms_platform::Result<()> {
            self.rec.push("confine(false)");
            Ok(())
        }
    }

    impl InputInject for MockInject {
        fn key(&mut self, _key: ms_platform::HidKey, _down: bool) -> ms_platform::Result<()> {
            Ok(())
        }
        fn mouse_move_to(&mut self, _x: f64, _y: f64) -> ms_platform::Result<()> {
            Ok(())
        }
        fn mouse_button(
            &mut self,
            _btn: ms_platform::Button,
            _down: bool,
        ) -> ms_platform::Result<()> {
            Ok(())
        }
        fn scroll(&mut self, _dx: f64, _dy: f64, _precise: bool) -> ms_platform::Result<()> {
            Ok(())
        }
        fn release_all(&mut self) -> ms_platform::Result<()> {
            self.rec.push("release_all");
            Ok(())
        }
    }

    #[test]
    fn 权限撤销回退顺序固定() {
        let rec = Recorder::default();
        let mut cap = MockCapture { rec: rec.clone() };
        let mut inj = MockInject { rec: rec.clone() };
        let mut fired = None;
        safe_fallback(
            &mut cap,
            &mut inj,
            FallbackKind::PermissionRevoked,
            &mut |k| fired = Some(k),
        );
        assert_eq!(
            rec.snapshot(),
            vec![
                "set_grab(false)",
                "confine(false)",
                "release_all",
                "capture.stop"
            ]
        );
        assert_eq!(fired, Some(FallbackKind::PermissionRevoked));
    }

    #[test]
    fn 断连回退不停止捕获() {
        let rec = Recorder::default();
        let mut cap = MockCapture { rec: rec.clone() };
        let mut inj = MockInject { rec: rec.clone() };
        safe_fallback(&mut cap, &mut inj, FallbackKind::Disconnected, &mut |_| {});
        // 断连场景保留捕获：无 capture.stop
        assert_eq!(
            rec.snapshot(),
            vec!["set_grab(false)", "confine(false)", "release_all"]
        );
    }

    #[test]
    fn 回退能驱动真实stub不panic() {
        // 用平台层 stub（方法返回 Unimplemented）验证"尽力而为"不因 Err 中断
        let mut cap = StubCapture;
        let mut inj = StubInject;
        let mut count = 0;
        safe_fallback(
            &mut cap,
            &mut inj,
            FallbackKind::PermissionRevoked,
            &mut |_| count += 1,
        );
        assert_eq!(count, 1, "通知必达，即使前序步骤全部 Err");
    }

    #[test]
    fn 撤销边沿只触发一次() {
        let mut d = RevocationDetector::new();
        assert!(!d.tick(true)); // 仍授权
        assert!(d.tick(false)); // 下降沿，触发
        assert!(!d.tick(false)); // 持续未授权，不重复触发
        assert!(!d.tick(true)); // 恢复授权
        assert!(d.tick(false)); // 再次撤销，再触发
    }
}
