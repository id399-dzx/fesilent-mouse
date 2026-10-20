//! Finder / Dock 重开已运行应用时通知现有设置窗口。
//!
//! 必须在主线程、winit EventLoop 创建之后安装，隔离 UI 预览不调用此入口。
//! winit 0.30.13 的扩展文档与实现不同：实现持有 WinitApplicationDelegate，
//! 替换它会破坏事件循环。这里只添加该类尚未实现的一个可选 selector，保留原
//! delegate 和全部已有方法，不交换/覆盖方法、不接管 terminate/open URL。

use std::ffi::CString;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use ms_platform::{PlatformError, Result};
use objc2::encode::Encode;
use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
use objc2::{ffi, sel};
use objc2_app_kit::NSApplication;
use objc2_foundation::MainThreadMarker;

type ReopenCallback = Box<dyn Fn() + Send + Sync + 'static>;
static CALLBACK: OnceLock<ReopenCallback> = OnceLock::new();
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// 注册原生重开通知。`callback` 应仅向 GUI 事件循环发事件，不直接操作窗口。
/// 重复安装幂等，保留第一次提供的回调；回调由主线程调用并捕获 panic。
pub fn install_reopen_handler(callback: impl Fn() + Send + Sync + 'static) -> Result<()> {
    let mtm = MainThreadMarker::new()
        .ok_or_else(|| PlatformError::Api("Mac 重开桥接必须在主线程安装".into()))?;
    if INSTALLED.load(Ordering::Acquire) {
        return Ok(());
    }
    let app = NSApplication::sharedApplication(mtm);
    let delegate = app
        .delegate()
        .ok_or_else(|| PlatformError::Api("Mac 重开桥接须在 winit 事件循环创建后安装".into()))?;
    let object: &AnyObject = (*delegate).as_ref();
    let class = object.class();
    if class.name() != c"WinitApplicationDelegate" {
        return Err(PlatformError::Api(
            "当前 Mac 应用 delegate 未识别，保留原行为，未安装重开桥接".into(),
        ));
    }
    // 先保留回调，再暴露 native 方法；安装由主线程串行执行，不存在两个安装者竞态。
    if class.instance_method(reopen_selector()).is_some() {
        return Err(PlatformError::Api(
            "Mac delegate 已有重开处理，未覆盖原实现".into(),
        ));
    }
    CALLBACK
        .set(Box::new(callback))
        .map_err(|_| PlatformError::Api("Mac 重开桥接此前初始化未完成，未重复注册".into()))?;
    register_missing_method(class)?;
    INSTALLED.store(true, Ordering::Release);
    Ok(())
}

fn reopen_selector() -> Sel {
    sel!(applicationShouldHandleReopen:hasVisibleWindows:)
}

fn register_missing_method(class: &AnyClass) -> Result<()> {
    if class.instance_method(reopen_selector()).is_some() {
        return Err(PlatformError::Api(
            "Mac delegate 已有重开处理，未覆盖原实现".into(),
        ));
    }
    // BOOL 在 Intel 和 Apple Silicon 上的 Objective-C 编码不同，由 objc2 提供
    // 对应目标编码，而非硬编码 c/B。省略参数偏移量是 runtime 接受的标准编码。
    let encoding = CString::new(format!("{}@:@{}", Bool::ENCODING, Bool::ENCODING))
        .map_err(|error| PlatformError::Api(error.to_string()))?;
    // SAFETY: 函数只通过带两个显式参数的同名 selector 调用，签名与 encoding 完全
    // 对应（self、_cmd、NSApplication*、BOOL -> BOOL）。ObjC IMP 用无参函数指针
    // 擦除类型，runtime 在派发时恢复调用约定；这里不调用这个擦除后的指针。
    let implementation: Imp = unsafe {
        std::mem::transmute::<
            extern "C-unwind" fn(*mut AnyObject, Sel, *mut AnyObject, Bool) -> Bool,
            Imp,
        >(handle_reopen)
    };
    // SAFETY: class 是 runtime 管理、进程存活期有效的类；此 API 内部同步。只添加
    // 缺失方法，不修改 ivar、delegate 对象或任何现有方法。函数与编码有效，runtime
    // 复制编码字符串；类指针转 mut 符合 Objective-C 注册 API 的签名。
    let added = unsafe {
        ffi::class_addMethod(
            std::ptr::from_ref(class).cast_mut(),
            reopen_selector(),
            implementation,
            encoding.as_ptr(),
        )
    };
    if !added.as_bool() {
        return Err(PlatformError::Api(
            "无法注册 Mac 重开处理，未替换现有方法".into(),
        ));
    }
    Ok(())
}

extern "C-unwind" fn handle_reopen(
    _delegate: *mut AnyObject,
    _selector: Sel,
    _application: *mut AnyObject,
    _has_visible_windows: Bool,
) -> Bool {
    if let Some(callback) = CALLBACK.get()
        && std::panic::catch_unwind(std::panic::AssertUnwindSafe(callback)).is_err()
    {
        tracing::error!(target: "mouseshare::mac", "Mac 重开通知回调异常，已保留现有事件循环");
    }
    // UI 通过代理事件显示/激活既有窗口，不让 AppKit 额外创建默认窗口。
    Bool::NO
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)] // 测试断言，不创建 NSApplication 或真实窗口。
    use super::*;
    use objc2::ClassType;
    use objc2::runtime::{ClassBuilder, NSObject};

    #[test]
    fn 可选重开方法只添加一次且不改已有方法() {
        let class = ClassBuilder::new(c"FesilentReopenBridgeRegistrationTest", NSObject::class())
            .expect("独立测试类名")
            .register();
        assert!(class.instance_method(reopen_selector()).is_none());
        register_missing_method(class).expect("首次添加可选方法");
        let method = class
            .instance_method(reopen_selector())
            .expect("方法已注册");
        let before = method.implementation();
        assert!(register_missing_method(class).is_err());
        let after = method.implementation();
        assert_eq!(before as usize, after as usize);
    }

    #[test]
    fn 原生重开通知回调无需创建新窗口或应用() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicUsize;
        let calls = Arc::new(AtomicUsize::new(0));
        let received = Arc::clone(&calls);
        assert!(
            CALLBACK
                .set(Box::new(move || {
                    received.fetch_add(1, Ordering::Relaxed);
                }))
                .is_ok()
        );
        for visible in [Bool::NO, Bool::YES] {
            // 回调不解引用这些参数；测试不接触 NSApplication、真实窗口或事件循环。
            let handled_by_appkit = handle_reopen(
                std::ptr::null_mut(),
                reopen_selector(),
                std::ptr::null_mut(),
                visible,
            );
            assert!(!handled_by_appkit.as_bool());
        }
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }
}
