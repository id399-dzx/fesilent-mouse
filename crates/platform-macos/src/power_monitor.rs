//! macOS 电源事件监听（IOKit `IORegisterForSystemPower`；FR-604）。
//!
//! 仅在 macOS 编译。专用线程跑 run loop 接收睡眠/唤醒回调：睡眠前回调上层
//! （可发 Suspend 给对端），唤醒后回调上层（触发立即重连）。真实睡眠验证归 H-5。

use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::thread::JoinHandle;

type IoConnect = u32;
type IoObject = u32;
type IoService = u32;
type IONotificationPortRef = *mut c_void;
type CFRunLoopRef = *mut c_void;
type CFRunLoopSourceRef = *mut c_void;
type CFStringRef = *const c_void;
type PowerCallback = extern "C" fn(*mut c_void, IoService, u32, *mut c_void);

// IOKit 电源消息类型
const MSG_CAN_SYSTEM_SLEEP: u32 = 0xE000_0270;
const MSG_SYSTEM_WILL_SLEEP: u32 = 0xE000_0280;
const MSG_SYSTEM_HAS_POWERED_ON: u32 = 0xE000_0320;

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IORegisterForSystemPower(
        refcon: *mut c_void,
        the_port: *mut IONotificationPortRef,
        callback: PowerCallback,
        notifier: *mut IoObject,
    ) -> IoConnect;
    fn IONotificationPortGetRunLoopSource(port: IONotificationPortRef) -> CFRunLoopSourceRef;
    fn IOAllowPowerChange(connect: IoConnect, notification_id: isize) -> i32;
    fn IODeregisterForSystemPower(notifier: *mut IoObject) -> i32;
    fn IONotificationPortDestroy(port: IONotificationPortRef);
    fn IOServiceClose(connect: IoConnect) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
    fn CFRunLoopRun();
    fn CFRunLoopStop(rl: CFRunLoopRef);
    static kCFRunLoopCommonModes: CFStringRef;
}

/// 回调集合。`on_sleep`/`on_wake` 在电源监听线程上下文被调用。
struct Callbacks {
    on_sleep: Box<dyn Fn() + Send>,
    on_wake: Box<dyn Fn() + Send>,
    connect: IoConnect,
}

extern "C" fn power_callback(refcon: *mut c_void, _service: IoService, msg: u32, arg: *mut c_void) {
    // SAFETY: refcon 是注册时传入的 Box<Callbacks> 裸指针，在监听存活期间有效
    let cb = unsafe { &*(refcon as *const Callbacks) };
    match msg {
        MSG_CAN_SYSTEM_SLEEP | MSG_SYSTEM_WILL_SLEEP => {
            if msg == MSG_SYSTEM_WILL_SLEEP {
                (cb.on_sleep)();
            }
            // 必须放行电源变更，否则系统等待超时（不阻止睡眠）
            // SAFETY: connect 有效，arg 为系统传入的通知 ID
            unsafe { IOAllowPowerChange(cb.connect, arg as isize) };
        }
        MSG_SYSTEM_HAS_POWERED_ON => (cb.on_wake)(),
        _ => {}
    }
}

pub struct PowerMonitor {
    runloop: Arc<AtomicPtr<c_void>>,
    thread: Option<JoinHandle<()>>,
}

impl PowerMonitor {
    /// 启动监听。`on_sleep` 在系统将睡眠时调用，`on_wake` 在唤醒后调用。
    pub fn start<S, W>(on_sleep: S, on_wake: W) -> Self
    where
        S: Fn() + Send + 'static,
        W: Fn() + Send + 'static,
    {
        let runloop = Arc::new(AtomicPtr::new(std::ptr::null_mut()));
        let runloop_slot = Arc::clone(&runloop);

        let thread = std::thread::Builder::new()
            .name("ms-mac-power".into())
            .spawn(move || {
                let mut port: IONotificationPortRef = std::ptr::null_mut();
                let mut notifier: IoObject = 0;
                // 先用占位 refcon 注册，拿到 connect 后再填回（回调在 run loop 启动后才触发）
                let mut cbs = Box::new(Callbacks {
                    on_sleep: Box::new(on_sleep),
                    on_wake: Box::new(on_wake),
                    connect: 0,
                });
                let refcon = (&mut *cbs as *mut Callbacks) as *mut c_void;

                // SAFETY: 出参指针均有效；返回 0 表示注册失败
                let connect = unsafe {
                    IORegisterForSystemPower(refcon, &mut port, power_callback, &mut notifier)
                };
                if connect == 0 {
                    tracing::error!(target: "mouseshare::mac", "IORegisterForSystemPower 失败");
                    return;
                }
                cbs.connect = connect;

                // SAFETY: port 有效，挂到当前线程 run loop
                unsafe {
                    let source = IONotificationPortGetRunLoopSource(port);
                    let rl = CFRunLoopGetCurrent();
                    runloop_slot.store(rl, Ordering::Release);
                    CFRunLoopAddSource(rl, source, kCFRunLoopCommonModes);
                }
                tracing::info!(target: "mouseshare::mac", "电源事件监听已启动");

                // SAFETY: 阻塞运行直到 stop 调用 CFRunLoopStop
                unsafe { CFRunLoopRun() };

                // 清理
                // SAFETY: notifier/port/connect 均为注册得到的有效句柄
                unsafe {
                    IODeregisterForSystemPower(&mut notifier);
                    IONotificationPortDestroy(port);
                    IOServiceClose(connect);
                }
                runloop_slot.store(std::ptr::null_mut(), Ordering::Release);
                // cbs 在此 drop
            });

        match thread {
            Ok(h) => Self {
                runloop,
                thread: Some(h),
            },
            Err(e) => {
                tracing::error!(target: "mouseshare::mac", %e, "电源监听线程创建失败");
                Self {
                    runloop,
                    thread: None,
                }
            }
        }
    }
}

impl Drop for PowerMonitor {
    fn drop(&mut self) {
        let rl = self.runloop.load(Ordering::Acquire);
        if !rl.is_null() {
            // SAFETY: CFRunLoopStop 线程安全
            unsafe { CFRunLoopStop(rl) };
        }
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
    }
}
