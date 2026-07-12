//! Windows 键鼠捕获（低级钩子 WH_KEYBOARD_LL/WH_MOUSE_LL；DEVELOPMENT.md §3.1）。
//!
//! 仅在 Windows 编译。专用线程装钩子并跑消息循环；grab=true 时钩子返回 1 吞掉事件。
//! 鼠标移动：grab 期间把光标重置到屏幕中心，用与中心的偏移作相对位移（FPS 式相对
//! 捕获），非 grab 期间用与上次位置的差。
//!
//! 注：钩子被系统静默摘除时的 Raw Input 参照源看门狗（TECH-REVIEW A-3）作为后续硬化，
//! 当前实现先保证基础捕获可用。

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::JoinHandle;

use ms_platform::{EventSink, InputCapture, InputEvent, PlatformError, Result};

use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetCursorPos, GetMessageW, GetSystemMetrics, HHOOK,
    KBDLLHOOKSTRUCT, MSG, MSLLHOOKSTRUCT, PostThreadMessageW, SM_CXSCREEN, SM_CYSCREEN,
    SetCursorPos, SetWindowsHookExW, ShowCursor, TranslateMessage, UnhookWindowsHookEx,
    WH_KEYBOARD_LL, WH_MOUSE_LL, WM_QUIT,
};

use crate::event_decode::{key_down, mouse_button, wheel, wm};
use crate::keymap::{scancode_to_hid, vk_modifier_to_hid};

const LLKHF_EXTENDED: u32 = 0x01;
const HC_ACTION: i32 = 0;

thread_local! {
    static CTX: RefCell<Option<HookCtx>> = const { RefCell::new(None) };
}

struct HookCtx {
    sink: EventSink,
    grab: Arc<AtomicBool>,
    /// grab 刚开启的一帧标志：建立位移基准，不产生跳变位移。
    grab_reset: Arc<AtomicBool>,
    last_mouse: Option<(i32, i32)>,
    center: (i32, i32),
}

impl HookCtx {
    fn grabbed(&self) -> bool {
        self.grab.load(Ordering::Acquire)
    }
}

/// 键盘钩子回调。
unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION {
        // SAFETY: 低级键盘钩子 lparam 指向有效 KBDLLHOOKSTRUCT
        let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let message = wparam.0 as u32;
        let extended = (kb.flags.0 & LLKHF_EXTENDED) != 0;
        // 修饰键优先用 vkCode（左右区分可靠），其余键用 scanCode+extended
        if let Some(down) = key_down(message)
            && let Some(hid) = vk_modifier_to_hid(kb.vkCode)
                .or_else(|| scancode_to_hid(kb.scanCode as u16, extended))
        {
            let swallow = CTX.with_borrow_mut(|c| {
                if let Some(ctx) = c.as_mut() {
                    (ctx.sink)(InputEvent::Key { key: hid, down });
                    ctx.grabbed()
                } else {
                    false
                }
            });
            if swallow {
                return LRESULT(1);
            }
        }
    }
    // SAFETY: 透传给后续钩子
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// 鼠标钩子回调。
unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION {
        // SAFETY: 低级鼠标钩子 lparam 指向有效 MSLLHOOKSTRUCT
        let ms = unsafe { &*(lparam.0 as *const MSLLHOOKSTRUCT) };
        let message = wparam.0 as u32;
        let swallow = CTX.with_borrow_mut(|c| {
            let Some(ctx) = c.as_mut() else {
                return false;
            };
            let grabbed = ctx.grabbed();
            if message == wm::MOUSEMOVE {
                let (x, y) = (ms.pt.x, ms.pt.y);
                if grabbed {
                    // REMOTE：FPS 式相对捕获——用「与上一帧基准的差值」算位移，再把
                    // 光标拉回中心并以**真实落点**为下一帧基准。关键：不能假设
                    // SetCursorPos 精确落在 center——非整数 DPI 缩放（如 2194×1234）下
                    // 它有取整误差，旧实现按 (x-center) 算位移会让该误差每帧累积，
                    // 光标飞到右下角。读回真实落点当基准即可消除累积（本次修复）。
                    let first = ctx.grab_reset.swap(false, Ordering::AcqRel);
                    if !first && let Some((lx, ly)) = ctx.last_mouse {
                        let dx = (x - lx) as f64;
                        let dy = (y - ly) as f64;
                        if dx != 0.0 || dy != 0.0 {
                            (ctx.sink)(InputEvent::MouseMove {
                                dx,
                                dy,
                                ax: x as f64,
                                ay: y as f64,
                            });
                        }
                    }
                    // SAFETY: 拉回中心并读回真实落点；GetCursorPos 失败回退到 center
                    unsafe {
                        let _ = SetCursorPos(ctx.center.0, ctx.center.1);
                        let mut p = POINT::default();
                        ctx.last_mouse = if GetCursorPos(&mut p).is_ok() {
                            Some((p.x, p.y))
                        } else {
                            Some(ctx.center)
                        };
                    }
                    return true; // 吞掉，本机光标不动
                } else {
                    let (dx, dy) = match ctx.last_mouse {
                        Some((lx, ly)) => ((x - lx) as f64, (y - ly) as f64),
                        None => (0.0, 0.0),
                    };
                    // LOCAL：始终上报绝对位置供边缘判定（即便 delta 为 0）
                    (ctx.sink)(InputEvent::MouseMove {
                        dx,
                        dy,
                        ax: x as f64,
                        ay: y as f64,
                    });
                    ctx.last_mouse = Some((x, y));
                    return false;
                }
            }
            if let Some((btn, down)) = mouse_button(message, ms.mouseData) {
                (ctx.sink)(InputEvent::Button { btn, down });
                return grabbed;
            }
            if let Some((dx, dy, precise)) = wheel(message, ms.mouseData) {
                (ctx.sink)(InputEvent::Scroll { dx, dy, precise });
                return grabbed;
            }
            false
        });
        if swallow {
            return LRESULT(1);
        }
    }
    // SAFETY: 透传给后续钩子
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

pub struct WinCapture {
    grab: Arc<AtomicBool>,
    grab_reset: Arc<AtomicBool>,
    thread_id: Arc<AtomicU32>,
    thread: Option<JoinHandle<()>>,
    confined: bool,
}

impl WinCapture {
    pub fn new() -> Self {
        Self {
            grab: Arc::new(AtomicBool::new(false)),
            grab_reset: Arc::new(AtomicBool::new(false)),
            thread_id: Arc::new(AtomicU32::new(0)),
            thread: None,
            confined: false,
        }
    }
}

impl Default for WinCapture {
    fn default() -> Self {
        Self::new()
    }
}

impl InputCapture for WinCapture {
    fn start(&mut self, sink: EventSink, grab: bool) -> Result<()> {
        if self.thread.is_some() {
            return Err(PlatformError::Api("捕获已在运行".into()));
        }
        self.grab.store(grab, Ordering::Release);
        self.grab_reset.store(grab, Ordering::Release);
        let grab_flag = Arc::clone(&self.grab);
        let grab_reset_flag = Arc::clone(&self.grab_reset);
        let tid_slot = Arc::clone(&self.thread_id);
        let (tx, rx) = std::sync::mpsc::channel::<std::result::Result<(), String>>();

        let handle = std::thread::Builder::new()
            .name("ms-win-capture".into())
            .spawn(move || {
                // SAFETY: 取模块句柄；低级钩子可传 NULL 模块
                let hmod: HINSTANCE = match unsafe { GetModuleHandleW(None) } {
                    Ok(h) => h.into(),
                    Err(e) => {
                        let _ = tx.send(Err(format!("GetModuleHandleW 失败: {e}")));
                        return;
                    }
                };
                // SAFETY: 屏幕尺寸查询
                let center = unsafe {
                    (
                        GetSystemMetrics(SM_CXSCREEN) / 2,
                        GetSystemMetrics(SM_CYSCREEN) / 2,
                    )
                };

                CTX.with_borrow_mut(|c| {
                    *c = Some(HookCtx {
                        sink,
                        grab: grab_flag,
                        grab_reset: grab_reset_flag,
                        last_mouse: None,
                        center,
                    });
                });

                // SAFETY: 安装两个低级钩子
                let kbd = unsafe {
                    SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), Some(hmod), 0)
                };
                let mouse =
                    unsafe { SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), Some(hmod), 0) };
                let (kbd, mouse): (HHOOK, HHOOK) = match (kbd, mouse) {
                    (Ok(k), Ok(m)) => (k, m),
                    _ => {
                        let _ = tx.send(Err("SetWindowsHookExW 失败".into()));
                        return;
                    }
                };

                tid_slot.store(current_thread_id(), Ordering::Release);
                let _ = tx.send(Ok(()));
                tracing::info!(target: "mouseshare::win", "低级钩子已安装");

                // 消息循环（钩子必须有消息循环才会被回调）
                let mut msg = MSG::default();
                // SAFETY: 标准消息循环，GetMessageW 返回 0(WM_QUIT)/-1(错误) 时退出
                unsafe {
                    while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }

                // SAFETY: 卸载钩子
                unsafe {
                    let _ = UnhookWindowsHookEx(kbd);
                    let _ = UnhookWindowsHookEx(mouse);
                }
                CTX.with_borrow_mut(|c| *c = None);
                tracing::info!(target: "mouseshare::win", "捕获线程退出");
            })
            .map_err(PlatformError::Io)?;

        match rx.recv() {
            Ok(Ok(())) => {
                self.thread = Some(handle);
                Ok(())
            }
            Ok(Err(msg)) => {
                let _ = handle.join();
                Err(PlatformError::Api(msg))
            }
            Err(_) => Err(PlatformError::Api("捕获线程未就绪即退出".into())),
        }
    }

    fn set_grab(&mut self, grab: bool) -> Result<()> {
        self.grab.store(grab, Ordering::Release);
        if grab {
            // 进入 grab：要求钩子下一帧重建位移基准，避免从本地旧位置跳变
            self.grab_reset.store(true, Ordering::Release);
        }
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        let tid = self.thread_id.load(Ordering::Acquire);
        if tid != 0 {
            // SAFETY: 向捕获线程投递 WM_QUIT 结束其消息循环
            unsafe {
                let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
            }
        }
        if let Some(h) = self.thread.take() {
            let _ = h.join();
        }
        Ok(())
    }

    fn confine_cursor(&mut self, on: bool) -> Result<()> {
        if on == self.confined {
            return Ok(());
        }
        // SAFETY: 显示/隐藏光标计数
        unsafe {
            ShowCursor(!on);
        }
        self.confined = on;
        Ok(())
    }
}

impl Drop for WinCapture {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn current_thread_id() -> u32 {
    // SAFETY: 无副作用查询当前线程 ID
    unsafe { windows::Win32::System::Threading::GetCurrentThreadId() }
}
