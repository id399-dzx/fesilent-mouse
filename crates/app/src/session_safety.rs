//! 同步平台动作与最终清理集中在此；网络等待不能挡住本机解除输入拦截。

use ms_core::control::Action;
use ms_core::engine::{EngineOut, Inject};
use ms_platform::{Button, HidKey, InputCapture, InputInject, Rect};

pub fn valid_screen(rect: Rect) -> bool {
    [rect.x, rect.y, rect.w, rect.h]
        .into_iter()
        .all(f64::is_finite)
        && rect.x.abs() <= 100_000.0
        && rect.y.abs() <= 100_000.0
        && rect.w >= 2.0
        && rect.h >= 2.0
        && rect.w <= 100_000.0
        && rect.h <= 100_000.0
}

pub fn apply_platform(
    out: &EngineOut,
    capture: &mut dyn InputCapture,
    inject: &mut dyn InputInject,
) -> Result<u64, String> {
    for action in &out.actions {
        match action {
            Action::SetGrab(on) => capture.set_grab(*on),
            Action::SetCursorVisible(visible) => capture.confine_cursor(!visible),
            _ => Ok(()),
        }
        .map_err(|e| format!("输入捕获操作失败：{e}"))?;
    }
    let mut keys = 0;
    for cmd in &out.inject {
        match *cmd {
            Inject::MoveTo { x, y } => inject.mouse_move_to(x, y),
            Inject::Key { hid, down } => {
                let result = inject.key(HidKey(hid), down);
                if result.is_ok() {
                    keys += 1;
                }
                result
            }
            Inject::Button { btn, down } => inject.mouse_button(
                match btn {
                    1 => Button::Right,
                    2 => Button::Middle,
                    3 => Button::X1,
                    4 => Button::X2,
                    _ => Button::Left,
                },
                down,
            ),
            Inject::Scroll { dx, dy, precise } => inject.scroll(dx as f64, dy as f64, precise),
            Inject::ReleaseAll => inject.release_all(),
        }
        .map_err(|e| format!("输入注入失败（锁屏或UAC桌面请在本机操作）：{e}"))?;
    }
    Ok(keys)
}

/// TECH-REVIEW A-1/B-2：解除grab→释放所有输入→恢复光标→停止捕获；失败也继续其余步骤。
pub fn cleanup(capture: &mut dyn InputCapture, inject: &mut dyn InputInject) -> Vec<String> {
    let mut errors = Vec::new();
    for result in [
        capture.set_grab(false),
        inject.release_all(),
        capture.confine_cursor(false),
        capture.stop(),
    ] {
        if let Err(error) = result {
            errors.push(error.to_string());
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use ms_platform::{EventSink, PlatformError};
    use std::sync::{Arc, Mutex};
    struct Capture(Arc<Mutex<Vec<&'static str>>>);
    impl InputCapture for Capture {
        fn start(&mut self, _: EventSink, _: bool) -> ms_platform::Result<()> {
            Ok(())
        }
        fn set_grab(&mut self, _: bool) -> ms_platform::Result<()> {
            self.0.lock().unwrap().push("ungrab");
            Err(PlatformError::Api("test".into()))
        }
        fn confine_cursor(&mut self, _: bool) -> ms_platform::Result<()> {
            self.0.lock().unwrap().push("cursor");
            Ok(())
        }
        fn stop(&mut self) -> ms_platform::Result<()> {
            self.0.lock().unwrap().push("stop");
            Ok(())
        }
    }
    struct InjectMock(Arc<Mutex<Vec<&'static str>>>);
    impl InputInject for InjectMock {
        fn key(&mut self, _: HidKey, _: bool) -> ms_platform::Result<()> {
            Ok(())
        }
        fn mouse_move_to(&mut self, _: f64, _: f64) -> ms_platform::Result<()> {
            Ok(())
        }
        fn mouse_button(&mut self, _: Button, _: bool) -> ms_platform::Result<()> {
            Ok(())
        }
        fn scroll(&mut self, _: f64, _: f64, _: bool) -> ms_platform::Result<()> {
            Ok(())
        }
        fn release_all(&mut self) -> ms_platform::Result<()> {
            self.0.lock().unwrap().push("release");
            Ok(())
        }
    }
    #[test]
    fn 清理在失败后继续且顺序固定() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut cap = Capture(calls.clone());
        let mut inj = InjectMock(calls.clone());
        assert_eq!(cleanup(&mut cap, &mut inj).len(), 1);
        assert_eq!(
            *calls.lock().unwrap(),
            ["ungrab", "release", "cursor", "stop"]
        );
    }
    #[test]
    fn 非有限和过渡屏幕尺寸不能进入会话() {
        for (w, h) in [
            (0.0, 900.0),
            (1.0, 1.0),
            (f64::NAN, 900.0),
            (f64::INFINITY, 900.0),
        ] {
            assert!(!valid_screen(Rect {
                x: 0.0,
                y: 0.0,
                w,
                h
            }));
        }
        assert!(valid_screen(Rect {
            x: -1920.0,
            y: -200.0,
            w: 3840.0,
            h: 2160.0
        }));
    }
}
