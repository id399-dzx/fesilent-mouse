//! macOS 键鼠捕获：CGEventTap（defaultTap，可拦截）+ 专用线程 run loop
//! （DEVELOPMENT.md §3.2 注意 1；TECH-REVIEW A-2）。
//!
//! 仅在 macOS 编译。FFI 字段提取后交给 [`crate::event_decode`] 纯逻辑解码，
//! 两种 tap 禁用事件经 [`crate::breaker`] 熔断后自动重启。
//!
//! 运行需"辅助功能"授权（H-1）：未授权时 `CGEventTapCreate` 返回 NULL，
//! 系统弹出授权窗，本函数返回 `PermissionDenied`。

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Instant;

use ms_platform::synthetic::is_own_event;
use ms_platform::{EventSink, InputCapture, PlatformError, Result};

use crate::breaker::DisableBreaker;
use crate::event_decode::{self, Decoded, RawFields, ev_type};

// ───────────────────────── FFI 类型与常量 ─────────────────────────

type CFTypeRef = *const c_void;
type CFMachPortRef = *mut c_void;
type CFRunLoopRef = *mut c_void;
type CFRunLoopSourceRef = *mut c_void;
type CFAllocatorRef = *const c_void;
type CFStringRef = *const c_void;
type CGEventRef = *mut c_void;
type CGEventTapProxy = *mut c_void;
type CGEventMask = u64;
type CFIndex = isize;
type TapCallback = extern "C" fn(CGEventTapProxy, u32, CGEventRef, *mut c_void) -> CGEventRef;

#[repr(C)]
struct CGPoint {
    x: f64,
    y: f64,
}

// CGEventField（CGEventTypes.h）
const FIELD_KEYCODE: u32 = 9;
const FIELD_BUTTON_NUMBER: u32 = 3;
const FIELD_DELTA_X: u32 = 4;
const FIELD_DELTA_Y: u32 = 5;
const FIELD_SCROLL_AXIS1: u32 = 11;
const FIELD_SCROLL_AXIS2: u32 = 12;
const FIELD_SCROLL_CONTINUOUS: u32 = 88;
const FIELD_SCROLL_FIXED_AXIS1: u32 = 93;
const FIELD_SCROLL_FIXED_AXIS2: u32 = 94;
const FIELD_SOURCE_USER_DATA: u32 = 42; // kCGEventSourceUserData

// CGEventTap 位置/放置/选项
const HID_EVENT_TAP: u32 = 0; // kCGHIDEventTap
const HEAD_INSERT: u32 = 0; // kCGHeadInsertEventTap
const OPTION_DEFAULT: u32 = 0; // kCGEventTapOptionDefault（可拦截）

const HID_DISPLAY: u32 = 0; // kCGNullDirectDisplay 不适用；隐藏光标用主显示 0

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: CGEventMask,
        callback: TapCallback,
        user_info: *mut c_void,
    ) -> CFMachPortRef;
    fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
    fn CGEventGetDoubleValueField(event: CGEventRef, field: u32) -> f64;
    fn CGEventGetFlags(event: CGEventRef) -> u64;
    fn CGEventGetLocation(event: CGEventRef) -> CGPoint;
    fn CGAssociateMouseAndMouseCursorPosition(connected: bool) -> i32;
    fn CGDisplayHideCursor(display: u32) -> i32;
    fn CGDisplayShowCursor(display: u32) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFMachPortCreateRunLoopSource(
        allocator: CFAllocatorRef,
        port: CFMachPortRef,
        order: CFIndex,
    ) -> CFRunLoopSourceRef;
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopRemoveSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopRunInMode(
        mode: CFStringRef,
        seconds: f64,
        return_after_source_handled: bool,
    ) -> i32;
    fn CFRunLoopStop(rl: CFRunLoopRef);
    fn CFMachPortInvalidate(port: CFMachPortRef);
    fn CFRelease(cf: CFTypeRef);
    static kCFRunLoopCommonModes: CFStringRef;
    static kCFRunLoopDefaultMode: CFStringRef;
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

/// 关心的事件掩码：键盘 + flagsChanged + 各类鼠标 + 滚轮。
/// tap 禁用事件无视掩码总会送达。
fn events_mask() -> CGEventMask {
    let types = [
        ev_type::KEY_DOWN,
        ev_type::KEY_UP,
        ev_type::FLAGS_CHANGED,
        ev_type::LEFT_MOUSE_DOWN,
        ev_type::LEFT_MOUSE_UP,
        ev_type::RIGHT_MOUSE_DOWN,
        ev_type::RIGHT_MOUSE_UP,
        ev_type::MOUSE_MOVED,
        ev_type::LEFT_MOUSE_DRAGGED,
        ev_type::RIGHT_MOUSE_DRAGGED,
        ev_type::OTHER_MOUSE_DOWN,
        ev_type::OTHER_MOUSE_UP,
        ev_type::OTHER_MOUSE_DRAGGED,
        ev_type::SCROLL_WHEEL,
    ];
    types.iter().fold(0u64, |m, t| m | (1u64 << t))
}

// ───────────────────────── 回调上下文 ─────────────────────────

/// 仅在 tap 线程内访问（回调非重入），故内部用 `RefCell`/`Cell` 即可。
struct Context {
    sink: RefCell<EventSink>,
    breaker: RefCell<DisableBreaker>,
    grab: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    port: CFMachPortRef,
    start: Instant,
}

/// 提取原始字段。
///
/// # Safety
/// `event` 必须是回调收到的有效 CGEventRef，且仅在回调内使用。
unsafe fn extract_fields(event: CGEventRef, etype: u32) -> RawFields {
    let mut f = RawFields::default();
    match etype {
        ev_type::KEY_DOWN | ev_type::KEY_UP | ev_type::FLAGS_CHANGED => {
            // SAFETY: event 有效，字段号为常量合法值
            f.keycode = unsafe { CGEventGetIntegerValueField(event, FIELD_KEYCODE) } as u16;
            // SAFETY: 同上
            f.flags = unsafe { CGEventGetFlags(event) };
        }
        ev_type::MOUSE_MOVED
        | ev_type::LEFT_MOUSE_DRAGGED
        | ev_type::RIGHT_MOUSE_DRAGGED
        | ev_type::OTHER_MOUSE_DRAGGED => {
            // SAFETY: event 有效
            f.delta_x = unsafe { CGEventGetIntegerValueField(event, FIELD_DELTA_X) } as f64;
            // SAFETY: event 有效
            f.delta_y = unsafe { CGEventGetIntegerValueField(event, FIELD_DELTA_Y) } as f64;
            // SAFETY: event 有效；取全局绝对光标位置（点坐标）
            let loc = unsafe { CGEventGetLocation(event) };
            f.abs_x = loc.x;
            f.abs_y = loc.y;
        }
        ev_type::OTHER_MOUSE_DOWN | ev_type::OTHER_MOUSE_UP => {
            // SAFETY: event 有效
            f.button_number = unsafe { CGEventGetIntegerValueField(event, FIELD_BUTTON_NUMBER) };
        }
        ev_type::SCROLL_WHEEL => {
            // SAFETY: event 有效
            f.scroll_continuous =
                unsafe { CGEventGetIntegerValueField(event, FIELD_SCROLL_CONTINUOUS) } != 0;
            if f.scroll_continuous {
                // SAFETY: event 有效
                f.scroll_axis1 =
                    unsafe { CGEventGetDoubleValueField(event, FIELD_SCROLL_FIXED_AXIS1) };
                // SAFETY: event 有效
                f.scroll_axis2 =
                    unsafe { CGEventGetDoubleValueField(event, FIELD_SCROLL_FIXED_AXIS2) };
            } else {
                // SAFETY: event 有效
                f.scroll_axis1 =
                    unsafe { CGEventGetIntegerValueField(event, FIELD_SCROLL_AXIS1) } as f64;
                // SAFETY: event 有效
                f.scroll_axis2 =
                    unsafe { CGEventGetIntegerValueField(event, FIELD_SCROLL_AXIS2) } as f64;
            }
        }
        _ => {}
    }
    f
}

/// tap 回调蹦床。约束：O(1)、不阻塞（TECH-REVIEW A-2）。
extern "C" fn tap_callback(
    _proxy: CGEventTapProxy,
    etype: u32,
    event: CGEventRef,
    user: *mut c_void,
) -> CGEventRef {
    // SAFETY: user 是 start() 传入的 Box<Context> 原始指针，在 tap 存活期间有效，
    // 且回调仅在单一 run loop 线程被非重入地调用。
    let ctx = unsafe { &*(user as *const Context) };

    // 禁用通知可能没有 CGEvent，且始终必须进入恢复/熔断分支。
    if !event.is_null()
        && !matches!(
            etype,
            ev_type::TAP_DISABLED_BY_TIMEOUT | ev_type::TAP_DISABLED_BY_USER_INPUT
        )
    {
        // SAFETY: 普通回调提供有效 CGEvent；字段 42 是事件源的用户标记。
        let marker = unsafe { CGEventGetIntegerValueField(event, FIELD_SOURCE_USER_DATA) } as u64;
        if is_own_event(marker) {
            // 透传给本机 OS，但不送 sink，不让远端注入再触发本机切换/锁定快捷键。
            return event;
        }
    }

    // SAFETY: event 为回调收到的有效指针
    let fields = unsafe { extract_fields(event, etype) };

    match event_decode::decode(etype, &fields) {
        Decoded::Input(ev) => {
            if let Ok(mut sink) = ctx.sink.try_borrow_mut() {
                (sink)(ev);
            }
            if ctx.grab.load(Ordering::Acquire) {
                // grab：吞掉事件，本机不响应
                return std::ptr::null_mut();
            }
            event
        }
        Decoded::TapDisabled { by_timeout } => {
            let now_ms = ctx.start.elapsed().as_millis() as u64;
            let keep = ctx
                .breaker
                .try_borrow_mut()
                .map(|mut b| b.record(now_ms))
                .unwrap_or(false);
            tracing::warn!(
                target: "mouseshare::mac",
                by_timeout,
                keep,
                "CGEventTap 被系统禁用"
            );
            if keep {
                // SAFETY: ctx.port 为有效 tap 端口
                unsafe { CGEventTapEnable(ctx.port, true) };
            } else {
                // 频繁禁用，熔断：标记失败并停止 run loop（DisconnectReason::TapDisabledLoop）
                ctx.grab.store(false, Ordering::Release);
                ctx.failed.store(true, Ordering::Release);
                // SAFETY: 在 run loop 线程内调用，CFRunLoopStop 是线程安全的 CF 调用
                unsafe { CFRunLoopStop(CFRunLoopGetCurrent()) };
            }
            event
        }
        Decoded::Ignore => event,
    }
}

// ───────────────────────── 对外句柄 ─────────────────────────

pub struct MacCapture {
    grab: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    stop_requested: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    confined: bool,
}

impl MacCapture {
    pub fn new() -> Self {
        Self {
            grab: Arc::new(AtomicBool::new(false)),
            failed: Arc::new(AtomicBool::new(false)),
            stop_requested: Arc::new(AtomicBool::new(false)),
            thread: None,
            confined: false,
        }
    }

    /// tap 是否因频繁禁用而熔断（供上层映射为断连原因码）。
    pub fn is_failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }
}

impl Default for MacCapture {
    fn default() -> Self {
        Self::new()
    }
}

impl InputCapture for MacCapture {
    fn start(&mut self, sink: EventSink, grab: bool) -> Result<()> {
        if self.thread.is_some() {
            return Err(PlatformError::Api("捕获已在运行".into()));
        }
        self.grab.store(grab, Ordering::Release);
        self.failed.store(false, Ordering::Release);
        self.stop_requested.store(false, Ordering::Release);

        let grab_flag = Arc::clone(&self.grab);
        let failed_flag = Arc::clone(&self.failed);
        let stop_requested = Arc::clone(&self.stop_requested);

        // 用 channel 把"tap 创建是否成功"回传给 start()，以便同步返回权限错误
        let (tx, rx) = std::sync::mpsc::channel::<Result<()>>();

        let handle = std::thread::Builder::new()
            .name("ms-mac-capture".into())
            .spawn(move || {
                // SAFETY: 仅查询授权状态，无副作用
                let trusted = unsafe { AXIsProcessTrusted() };
                if !trusted {
                    tracing::warn!(target: "mouseshare::mac", "辅助功能未授权，尝试创建 tap 将触发系统授权窗（H-1）");
                }

                // 在本线程构建 Context（含非 Send 的 port），boxed 后取裸指针传给回调
                let mut ctx = Box::new(Context {
                    sink: RefCell::new(sink),
                    breaker: RefCell::new(DisableBreaker::new()),
                    grab: grab_flag,
                    failed: failed_flag,
                    port: std::ptr::null_mut(),
                    start: Instant::now(),
                });
                let ctx_ptr = (&mut *ctx as *mut Context) as *mut c_void;

                // SAFETY: 回调与 ctx_ptr 均合法；返回 NULL 表示创建失败（多为未授权）
                let port = unsafe {
                    CGEventTapCreate(
                        HID_EVENT_TAP,
                        HEAD_INSERT,
                        OPTION_DEFAULT,
                        events_mask(),
                        tap_callback,
                        ctx_ptr,
                    )
                };
                if port.is_null() {
                    let _ = tx.send(Err(PlatformError::PermissionDenied(
                        "CGEventTapCreate 返回 NULL，请检查辅助功能授权".into(),
                    )));
                    return;
                }
                ctx.port = port;

                // SAFETY: port 有效；创建 run loop source 并挂到当前线程 run loop
                let source = unsafe { CFMachPortCreateRunLoopSource(std::ptr::null(), port, 0) };
                if source.is_null() {
                    // SAFETY: port 由本线程创建且尚未挂入 run loop。
                    unsafe {
                        CFMachPortInvalidate(port);
                        CFRelease(port as CFTypeRef);
                    }
                    let _ = tx.send(Err(PlatformError::Api("无法创建 CGEventTap run loop source".into())));
                    return;
                }
                // SAFETY: 获取当前线程 run loop；其引用不跨线程传递。
                let rl = unsafe { CFRunLoopGetCurrent() };
                // SAFETY: rl/source/常量模式均有效
                unsafe {
                    CFRunLoopAddSource(rl, source, kCFRunLoopCommonModes);
                    CGEventTapEnable(port, true);
                }

                let _ = tx.send(Ok(()));
                tracing::info!(target: "mouseshare::mac", "CGEventTap 已启动");

                while !stop_requested.load(Ordering::Acquire) && !ctx.failed.load(Ordering::Acquire) {
                    // SAFETY: source 属于当前 run loop。定时返回检查取消位，避免 start
                    // 就绪与 run loop 开始之间的 stop 丢失，也不跨线程解引用 CF 指针。
                    unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.1, false) };
                }
                ctx.grab.store(false, Ordering::Release);

                // 退出清理
                // SAFETY: port/source 为本线程创建，停止后释放
                unsafe {
                    CGEventTapEnable(port, false);
                    CFRunLoopRemoveSource(rl, source, kCFRunLoopCommonModes);
                    CFMachPortInvalidate(port);
                    CFRelease(source as CFTypeRef);
                    CFRelease(port as CFTypeRef);
                }
                tracing::info!(target: "mouseshare::mac", "CGEventTap 线程退出");
                // ctx 在此 drop
            })
            .map_err(PlatformError::Io)?;

        match rx.recv() {
            Ok(Ok(())) => {
                self.thread = Some(handle);
                Ok(())
            }
            Ok(Err(error)) => {
                let _ = handle.join();
                self.grab.store(false, Ordering::Release);
                Err(error)
            }
            Err(_) => {
                let _ = handle.join();
                self.grab.store(false, Ordering::Release);
                Err(PlatformError::Api("捕获线程未就绪即退出".into()))
            }
        }
    }

    fn set_grab(&mut self, grab: bool) -> Result<()> {
        self.grab.store(grab, Ordering::Release);
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        // TECH-REVIEW A-1/B-2：停捕获的兜底同样必须先解除拦截并恢复本机光标。
        // 跨设备 release_all 由上层安全回退事务在 stop 前执行。
        self.grab.store(false, Ordering::Release);
        let cursor_result = self.confine_cursor(false);
        self.stop_requested.store(true, Ordering::Release);
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
        cursor_result
    }

    fn check_health(&self) -> Result<()> {
        if self.is_failed() {
            return Err(PlatformError::Api(
                "CGEventTap 频繁停用，捕获已熔断（TapDisabledLoop）".into(),
            ));
        }
        if self.thread.as_ref().is_none_or(JoinHandle::is_finished) {
            return Err(PlatformError::Api("CGEventTap 捕获线程未运行".into()));
        }
        Ok(())
    }

    fn confine_cursor(&mut self, on: bool) -> Result<()> {
        // T-103 范围：解除鼠标与光标关联 + 隐藏（防止 grab 期间本机光标移动）。
        // 注：warp 到屏幕中心并按帧重置的完整实现属 T-105（TECH-REVIEW B-1）。
        if on == self.confined {
            return Ok(());
        }
        // SAFETY: 均为无参数/标量的 CoreGraphics 调用，无别名要求。
        // 恢复时两项都尝试，不能因恢复鼠标关联失败就跳过显示光标。
        let (association_error, visibility_error) = unsafe {
            if on {
                (
                    CGAssociateMouseAndMouseCursorPosition(false),
                    CGDisplayHideCursor(HID_DISPLAY),
                )
            } else {
                (
                    CGAssociateMouseAndMouseCursorPosition(true),
                    CGDisplayShowCursor(HID_DISPLAY),
                )
            }
        };
        if association_error != 0 || visibility_error != 0 {
            // 开启可能仅部分成功；保留标记，让安全回退/Drop 继续尝试恢复。
            self.confined = true;
            return Err(PlatformError::Api(format!(
                "切换光标限制失败（on={on}, association={association_error}, visibility={visibility_error}）"
            )));
        }
        self.confined = on;
        Ok(())
    }
}

impl Drop for MacCapture {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)] // 测试断言；不执行真实 tap 捕获或光标隐藏。
    use super::*;

    #[test]
    fn 停止捕获必定解除grab并允许重复停止() {
        let mut capture = MacCapture::new();
        capture.set_grab(true).expect("设置标记");
        capture.stop().expect("首次停止");
        assert!(!capture.grab.load(Ordering::Acquire));
        capture.stop().expect("重复停止");
        assert!(capture.check_health().is_err(), "停止后不能报告捕获健康");
    }

    #[test]
    fn 熔断状态由trait健康检查上报() {
        let capture = MacCapture::new();
        capture.failed.store(true, Ordering::Release);
        let error = capture.check_health().expect_err("熔断必须返回错误");
        assert!(error.to_string().contains("TapDisabledLoop"));
    }
}
