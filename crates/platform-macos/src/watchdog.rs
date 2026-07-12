//! 权限看门狗（TECH-REVIEW A-1）：每 2s 轮询 `AXIsProcessTrusted()`，
//! 检测到"已授权→未授权"下降沿即触发回调（上层在回调内执行 ms-core 的安全回退）。
//!
//! 仅在 macOS 编译。边沿判定逻辑复用 ms-core 的 `RevocationDetector`（已单测），
//! 本模块只负责 FFI 轮询与线程管理，真实撤销验证归 H-5。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use ms_core::safety::RevocationDetector;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

/// 轮询间隔（DEVELOPMENT.md §3.2 注意 2：2 秒）。
const POLL_INTERVAL: Duration = Duration::from_secs(2);

pub struct PermissionWatchdog {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl PermissionWatchdog {
    /// 启动看门狗。`on_revoked` 在检测到权限撤销下降沿时被调用一次
    /// （在看门狗线程上下文），上层应在其中触发安全回退。
    pub fn start<F>(on_revoked: F) -> Self
    where
        F: FnMut() + Send + 'static,
    {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let mut on_revoked = on_revoked;

        let thread = std::thread::Builder::new()
            .name("ms-mac-permwatch".into())
            .spawn(move || {
                let mut detector = RevocationDetector::new();
                while !stop_thread.load(Ordering::Acquire) {
                    // SAFETY: 纯查询，无副作用
                    let trusted = unsafe { AXIsProcessTrusted() };
                    if detector.tick(trusted) {
                        tracing::error!(
                            target: "mouseshare::mac",
                            "辅助功能权限被撤销，触发安全回退（TECH-REVIEW A-1）"
                        );
                        on_revoked();
                    }
                    std::thread::sleep(POLL_INTERVAL);
                }
            });

        match thread {
            Ok(h) => Self {
                stop,
                thread: Some(h),
            },
            Err(e) => {
                tracing::error!(target: "mouseshare::mac", %e, "权限看门狗线程创建失败");
                Self { stop, thread: None }
            }
        }
    }
}

impl Drop for PermissionWatchdog {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
    }
}
