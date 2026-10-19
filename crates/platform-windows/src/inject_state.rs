//! 注入结果与待释放状态的纯逻辑，可在非 Windows 主机验证拒绝注入后的恢复行为。

use std::collections::BTreeSet;

use ms_platform::{PlatformError, Result};

pub(crate) fn check_send_count(sent: u32, requested: usize, error_code: i32) -> Result<()> {
    if sent as usize == requested {
        return Ok(());
    }
    let message = format!(
        "SendInput 仅接受 {sent}/{requested} 个事件（系统错误 {error_code}）；请检查目标窗口权限或安全桌面，普通用户进程不能向更高权限窗口注入输入"
    );
    if error_code == 5 {
        Err(PlatformError::PermissionDenied(message))
    } else {
        // UIPI 拦截时系统不一定提供错误码，不能仅按 GetLastError 判为成功。
        Err(PlatformError::Api(message))
    }
}

/// 单次失败不阻止释放其余项；仅删除确认释放成功的状态，保留失败项供后续重试。
pub(crate) fn release_pending<K: Copy + Ord>(
    pressed: &mut BTreeSet<K>,
    mut release: impl FnMut(K) -> Result<()>,
) -> Result<()> {
    let mut first_error = None;
    pressed.retain(|key| match release(*key) {
        Ok(()) => false,
        Err(error) => {
            if first_error.is_none() {
                first_error = Some(error);
            }
            true
        }
    });
    first_error.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 送入数量不足或uipi无错误码也不可报告成功() {
        assert!(check_send_count(1, 1, 5).is_ok());
        assert!(check_send_count(0, 1, 0).is_err());
        assert!(check_send_count(1, 2, 0).is_err());
        assert!(matches!(
            check_send_count(0, 1, 5),
            Err(PlatformError::PermissionDenied(_))
        ));
    }

    #[test]
    fn 释放失败保留状态且继续释放其余键后可重试() {
        let mut pressed = BTreeSet::from([4, 5, 6]);
        let mut attempted = Vec::new();
        let result = release_pending(&mut pressed, |key| {
            attempted.push(key);
            if key == 5 {
                Err(PlatformError::Api("模拟安全桌面拒绝".into()))
            } else {
                Ok(())
            }
        });
        assert!(result.is_err());
        assert_eq!(attempted, vec![4, 5, 6]);
        assert_eq!(pressed, BTreeSet::from([5]));
        assert!(release_pending(&mut pressed, |_| Ok(())).is_ok());
        assert!(pressed.is_empty());
        assert!(release_pending(&mut pressed, |_| panic!("空状态不应再注入")).is_ok());
    }
}
