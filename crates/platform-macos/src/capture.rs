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
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::thread::JoinHandle;
use std::time::Instant;

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
    fn CFRunLoopRun();
    fn CFRunLoopStop(rl: CFRunLoopRef);
    fn CFRelease(cf: CFTypeRef);
    static kCFRunLoopCommonModes: CFStringRef;
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
    runloop: Arc<AtomicPtr<c_void>>,
    thread: Option<JoinHandle<()>>,
    confined: bool,
}

impl MacCapture {
    pub fn new() -> Self {
        Self {
            grab: Arc::new(AtomicBool::new(false)),
            failed: Arc::new(AtomicBool::new(false)),
            runloop: Arc::new(AtomicPtr::new(std::ptr::null_mut())),
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

        let grab_flag = Arc::clone(&self.grab);
        let failed_flag = Arc::clone(&self.failed);
        let runloop_slot = Arc::clone(&self.runloop);

        // 用 channel 把"tap 创建是否成功"回传给 start()，以便同步返回权限错误
        let (tx, rx) = std::sync::mpsc::channel::<std::result::Result<(), String>>();

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
                    let _ = tx.send(Err("CGEventTapCreate 返回 NULL（多为辅助功能未授权）".into()));
                    return;
                }
                ctx.port = port;

                // SAFETY: port 有效；创建 run loop source 并挂到当前线程 run loop
                let source = unsafe { CFMachPortCreateRunLoopSource(std::ptr::null(), port, 0) };
                // SAFETY: 获取当前线程 run loop 并存出，供 stop() 调用 CFRunLoopStop
                let rl = unsafe { CFRunLoopGetCurrent() };
                runloop_slot.store(rl, Ordering::Release);
                // SAFETY: rl/source/常量模式均有效
                unsafe {
                    CFRunLoopAddSource(rl, source, kCFRunLoopCommonModes);
                    CGEventTapEnable(port, true);
                }

                let _ = tx.send(Ok(()));
                tracing::info!(target: "mouseshare::mac", "CGEventTap 已启动");

                // SAFETY: 阻塞运行当前线程 run loop，直到 stop()/熔断调用 CFRunLoopStop
                unsafe { CFRunLoopRun() };

                // 退出清理
                // SAFETY: port/source 为本线程创建，停止后释放
                unsafe {
                    CGEventTapEnable(port, false);
                    CFRelease(source as CFTypeRef);
                    CFRelease(port as CFTypeRef);
                }
                runloop_slot.store(std::ptr::null_mut(), Ordering::Release);
                tracing::info!(target: "mouseshare::mac", "CGEventTap 线程退出");
                // ctx 在此 drop
            })
            .map_err(PlatformError::Io)?;

        match rx.recv() {
            Ok(Ok(())) => {
                self.thread = Some(handle);
                Ok(())
            }
            Ok(Err(msg)) => {
                let _ = handle.join();
                Err(PlatformError::PermissionDenied(msg))
            }
            Err(_) => Err(PlatformError::Api("捕获线程未就绪即退出".into())),
        }
    }

    fn set_grab(&mut self, grab: bool) -> Result<()> {
        self.grab.store(grab, Ordering::Release);
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        let rl = self.runloop.load(Ordering::Acquire);
        if !rl.is_null() {
            // SAFETY: CFRunLoopStop 是线程安全的 CF 调用，rl 为捕获线程的 run loop
            unsafe { CFRunLoopStop(rl) };
        }
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
        Ok(())
    }

    fn confine_cursor(&mut self, on: bool) -> Result<()> {
        // T-103 范围：解除鼠标与光标关联 + 隐藏（防止 grab 期间本机光标移动）。
        // 注：warp 到屏幕中心并按帧重置的完整实现属 T-105（TECH-REVIEW B-1）。
        if on == self.confined {
            return Ok(());
        }
        // SAFETY: 均为无参数/标量的 CoreGraphics 调用，无别名要求
        unsafe {
            if on {
                CGAssociateMouseAndMouseCursorPosition(false);
                CGDisplayHideCursor(HID_DISPLAY);
            } else {
                CGAssociateMouseAndMouseCursorPosition(true);
                CGDisplayShowCursor(HID_DISPLAY);
            }
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
