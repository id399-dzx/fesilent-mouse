//! 窗口显示辅助（T-611）：置顶显示但不抢键盘焦点。
//!
//! winit 的 `set_visible(true)` 在 macOS 上是 `makeKeyAndOrderFront`——窗口必成
//! key window。设置窗一显示就捕获（注入的）键盘输入：对端控制本机时按键全进面板、
//! 无法向其他应用输入。本模块用 `orderFrontRegardless` 只置顶不夺 key，用户点击
//! 面板时系统再自然给焦点。

use std::ffi::c_void;

use objc2_app_kit::NSView;

/// 把 `ns_view`（winit `AppKitWindowHandle::ns_view`，窗口内容视图指针）所在的
/// NSWindow 置顶显示，但**不**设为 key window。
///
/// 调用方约定：`ns_view` 必须是有效存活的 `NSView*`，且在主线程调用
/// （winit 窗口事件回调即主线程）。
pub fn order_front_without_focus(ns_view: *mut c_void) {
    if ns_view.is_null() {
        return;
    }
    // SAFETY: 按上述约定，ns_view 为主线程上有效的 NSView*；仅借用不转移所有权。
    let view = unsafe { &*(ns_view as *const NSView) };
    if let Some(w) = view.window() {
        w.orderFrontRegardless();
    }
}
