//! macOS 键鼠注入：CGEventPost（DEVELOPMENT.md §3.2）。
//!
//! 仅在 macOS 编译。专用 `CGEventSource` 并设 `LocalEventsSuppressionInterval=0`
//! 以规避 warp 后 0.25s 输入抑制（TECH-REVIEW B-1）。按下状态由
//! [`crate::inject_state::PressTracker`] 跟踪，`release_all` 据此防卡键（FR-305）。

use std::ffi::c_void;

use ms_platform::{Button, HidKey, InputInject, PlatformError, Result};

use crate::inject_state::PressTracker;
use crate::keymap::hid_to_vk;

type CGEventSourceRef = *mut c_void;
type CGEventRef = *mut c_void;
type CFTypeRef = *const c_void;

#[repr(C)]
#[derive(Clone, Copy)]
struct CGPoint {
    x: f64,
    y: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGSize {
    w: f64,
    h: f64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CGRect {
    origin: CGPoint,
    size: CGSize,
}

// CGEventSourceStateID：HID 系统状态
const SOURCE_STATE_HID: u32 = 1;
// CGEventTapLocation：HID
const POST_TAP_HID: u32 = 0;
// CGScrollEventUnit
const SCROLL_UNIT_PIXEL: u32 = 0;
const SCROLL_UNIT_LINE: u32 = 1;
// CGMouseButton
const MOUSE_BTN_LEFT: u32 = 0;
const MOUSE_BTN_RIGHT: u32 = 1;
const MOUSE_BTN_CENTER: u32 = 2;
// CGEventField
const FIELD_BUTTON_NUMBER: u32 = 3;
// kCGMouseEventClickState：点击计数（1=单击 2=双击 3=三击）。macOS 不会从合成事件
// 自动识别双击，必须显式设此字段，应用才会触发"双击打开"等行为（TECH-REVIEW B-8）。
const FIELD_CLICK_STATE: u32 = 1;
// kCGMouseEventDeltaX/Y：移动事件的相对位移。硬件事件天然携带；合成事件不设则恒 0，
// 而 macOS「边缘唤出」类 UI（自动隐藏的程序坞/全屏菜单栏）按边缘处的推挤 delta 判定，
// 收不到 delta 就永远唤不出（T-610）。
const FIELD_DELTA_X: u32 = 4;
const FIELD_DELTA_Y: u32 = 5;

/// 贴边推挤的合成 delta 量（像素/事件）：位置被 clamp 在屏幕边缘、控制端仍在推
/// （连续同位置移动）时，朝所贴边合成此推挤量，模拟硬件「顶着边继续推」。
const EDGE_NUDGE: i64 = 10;

/// 纯函数：注入移动事件应携带的 (deltaX, deltaY)。**逐轴**判定：某轴有实位移用实
/// 位移；某轴位移为 0 且位置被 clamp 钉在屏幕边（控制端仍在推，物理 delta 被 clamp
/// 吃掉）则朝所贴边合成 [`EDGE_NUDGE`]（T-610/T-615）。不能整体「有位移就全用实位移」：
/// 手推鼠标横向几乎必然抖动，若 x 有位移就跳过合成，贴底边时 deltaY 恒 0，
/// 自动隐藏的程序坞永远唤不出。`bounds` 为主屏 (x, y, w, h)；屏内静止返回 (0, 0)。
fn move_deltas(prev: (f64, f64), next: (f64, f64), bounds: (f64, f64, f64, f64)) -> (i64, i64) {
    let (bx, by, bw, bh) = bounds;
    let mut d = (
        (next.0 - prev.0).round() as i64,
        (next.1 - prev.1).round() as i64,
    );
    if d.0 == 0 {
        if next.0 <= bx {
            d.0 = -EDGE_NUDGE;
        } else if next.0 >= bx + bw - 1.0 {
            d.0 = EDGE_NUDGE;
        }
    }
    if d.1 == 0 {
        if next.1 <= by {
            d.1 = -EDGE_NUDGE;
        } else if next.1 >= by + bh - 1.0 {
            d.1 = EDGE_NUDGE;
        }
    }
    d
}

/// 双击判定：与上次同键点击的最大间隔与最大位移（超出则计数复位为单击）。
const DOUBLE_CLICK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);
const DOUBLE_CLICK_DIST: f64 = 5.0;

// CGEventFlags 修饰键掩码（CGEventTypes.h）。注入组合键/Cmd+Click 时，事件必须
// 携带这些 flags，应用才认得修饰态——否则 Ctrl+C 被当成裸 C（FR-302 实测坑）。
const FLAG_SHIFT: u64 = 0x0002_0000;
const FLAG_CONTROL: u64 = 0x0004_0000;
const FLAG_ALT: u64 = 0x0008_0000;
const FLAG_CMD: u64 = 0x0010_0000;

/// Esc 的 HID Usage ID。强制退出热键检测用（TECH-REVIEW B-9）。
const HID_ESC: u16 = 0x29;

/// 是否构成"强制退出"组合：Esc 按下且当前按住 Command+Option（mac Force Quit = ⌘⌥⎋）。
/// macOS 对**合成事件**过滤这类系统级符号热键（普通快捷键如 ⌘C 不受影响），故被控端
/// 注入 ⌘⌥⎋ 不会弹强制退出窗——检测到此组合后改用受系统认可的途径触发（见 open_force_quit）。
fn is_force_quit(hid: u16, down: bool, mod_flags: u64) -> bool {
    down && hid == HID_ESC && (mod_flags & FLAG_CMD != 0) && (mod_flags & FLAG_ALT != 0)
}

/// 触发 macOS「强制退出应用程序」面板：经 System Events 发送 ⌘⌥⎋（key code 53）。
/// System Events 的注入走系统认可路径，不被符号热键过滤（实测可弹窗）。
/// 首次触发会弹一次「MouseShare 想要控制"系统事件"」自动化授权，允许后长期生效。
fn open_force_quit() {
    let _ = std::process::Command::new("osascript")
        .args([
            "-e",
            "tell application \"System Events\" to key code 53 using {command down, option down}",
        ])
        .spawn();
}

/// HID 修饰键码 → CGEventFlags 掩码（左右同归一类；非修饰键返回 None）。
fn hid_modifier_flag(hid: u16) -> Option<u64> {
    match hid {
        0xE0 | 0xE4 => Some(FLAG_CONTROL), // L/R Ctrl
        0xE1 | 0xE5 => Some(FLAG_SHIFT),   // L/R Shift
        0xE2 | 0xE6 => Some(FLAG_ALT),     // L/R Alt(Option)
        0xE3 | 0xE7 => Some(FLAG_CMD),     // L/R GUI(Command)
        _ => None,
    }
}

// CGEventType
const EV_LEFT_DOWN: u32 = 1;
const EV_LEFT_UP: u32 = 2;
const EV_RIGHT_DOWN: u32 = 3;
const EV_RIGHT_UP: u32 = 4;
const EV_MOUSE_MOVED: u32 = 5;
const EV_OTHER_DOWN: u32 = 25;
const EV_OTHER_UP: u32 = 26;
// 拖动事件：按住键移动时必须发这些（而非 MouseMoved），否则 macOS 不识别为拖拽，
// 导致拖文件/选区等操作失效。
const EV_LEFT_DRAGGED: u32 = 6;
const EV_RIGHT_DRAGGED: u32 = 7;
const EV_OTHER_DRAGGED: u32 = 27;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventSourceCreate(state_id: u32) -> CGEventSourceRef;
    fn CGEventSourceSetLocalEventsSuppressionInterval(source: CGEventSourceRef, seconds: f64);
    fn CGEventCreateKeyboardEvent(
        source: CGEventSourceRef,
        keycode: u16,
        key_down: bool,
    ) -> CGEventRef;
    fn CGEventCreateMouseEvent(
        source: CGEventSourceRef,
        mouse_type: u32,
        pos: CGPoint,
        button: u32,
    ) -> CGEventRef;
    // 变参 C 函数：units, wheelCount, wheel1, [wheel2, ...]
    fn CGEventCreateScrollWheelEvent(
        source: CGEventSourceRef,
        units: u32,
        wheel_count: u32,
        wheel1: i32,
        ...
    ) -> CGEventRef;
    fn CGEventSetIntegerValueField(event: CGEventRef, field: u32, value: i64);
    fn CGEventSetFlags(event: CGEventRef, flags: u64);
    fn CGEventPost(tap: u32, event: CGEventRef);
    fn CGWarpMouseCursorPosition(point: CGPoint) -> i32;
    fn CGMainDisplayID() -> u32;
    fn CGDisplayBounds(display: u32) -> CGRect;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: CFTypeRef);
}

pub struct MacInject {
    source: CGEventSourceRef,
    tracker: PressTracker,
    last_pos: CGPoint,
    /// 当前按下的修饰键 flags（注入每个键/鼠标事件时带上，使组合键生效）。
    mod_flags: u64,
    /// 当前按住的拖动状态 (拖动事件类型, CG 按钮, 可选 button_number)。有值时移动发
    /// `*MouseDragged` 而非 `MouseMoved`，使 macOS 识别为拖拽（拖文件/选区）。
    drag: Option<(u32, u32, Option<i64>)>,
    /// 双击推导：上次按下的 (按钮, 时刻, 位置) 与当前连击计数（注入端按时序+位移推导，
    /// 因协议未携带 clickCount）。按下时更新，对应的抬起复用同一计数。
    last_click: Option<(Button, std::time::Instant, CGPoint)>,
    click_count: i64,
}

// CGEventSourceRef 为不可变的 CF 对象指针，注入器自身在单线程使用（核心层串行调用），
// 标注 Send 以满足 InputInject: Send。
// SAFETY: source 仅被持有它的单一 MacInject 使用，不跨线程并发访问。
unsafe impl Send for MacInject {}

impl MacInject {
    pub fn new() -> Result<Self> {
        // SAFETY: 创建 HID 状态事件源；失败返回空指针
        let source = unsafe { CGEventSourceCreate(SOURCE_STATE_HID) };
        if source.is_null() {
            return Err(PlatformError::Api("CGEventSourceCreate 失败".into()));
        }
        // SAFETY: source 有效；置 0 抑制间隔（TECH-REVIEW B-1）
        unsafe { CGEventSourceSetLocalEventsSuppressionInterval(source, 0.0) };
        Ok(Self {
            source,
            tracker: PressTracker::new(),
            last_pos: CGPoint { x: 0.0, y: 0.0 },
            mod_flags: 0,
            drag: None,
            last_click: None,
            click_count: 0,
        })
    }

    fn post_key(&self, vk: u16, down: bool) {
        // SAFETY: source 有效，vk 为合法键码；事件创建后设修饰 flags 再 post、释放
        unsafe {
            let ev = CGEventCreateKeyboardEvent(self.source, vk, down);
            if !ev.is_null() {
                CGEventSetFlags(ev, self.mod_flags);
                CGEventPost(POST_TAP_HID, ev);
                CFRelease(ev as CFTypeRef);
            }
        }
    }

    /// `click_state` > 0 时设 kCGMouseEventClickState（按下/抬起带连击计数，使双击生效）；
    /// 移动/拖动传 0 不设此字段。`delta` 非 0 分量写入 kCGMouseEventDeltaX/Y（T-610）。
    fn post_mouse(
        &self,
        etype: u32,
        pos: CGPoint,
        btn: u32,
        button_number: Option<i64>,
        click_state: i64,
        delta: (i64, i64),
    ) {
        // SAFETY: source 有效；other 类事件补 button number 字段；带当前修饰 flags（Cmd+Click）
        unsafe {
            let ev = CGEventCreateMouseEvent(self.source, etype, pos, btn);
            if !ev.is_null() {
                if let Some(n) = button_number {
                    CGEventSetIntegerValueField(ev, FIELD_BUTTON_NUMBER, n);
                }
                if click_state > 0 {
                    CGEventSetIntegerValueField(ev, FIELD_CLICK_STATE, click_state);
                }
                if delta.0 != 0 {
                    CGEventSetIntegerValueField(ev, FIELD_DELTA_X, delta.0);
                }
                if delta.1 != 0 {
                    CGEventSetIntegerValueField(ev, FIELD_DELTA_Y, delta.1);
                }
                CGEventSetFlags(ev, self.mod_flags);
                CGEventPost(POST_TAP_HID, ev);
                CFRelease(ev as CFTypeRef);
            }
        }
    }

    /// 按下时根据与上次同键点击的时间间隔与位移推导连击计数（1/2/3…），并更新状态。
    fn next_click_count(&mut self, btn: Button, pos: CGPoint) -> i64 {
        let now = std::time::Instant::now();
        self.click_count = derive_click_count(self.last_click, self.click_count, btn, pos, now);
        self.last_click = Some((btn, now, pos));
        self.click_count
    }
}

/// 纯函数：根据上次点击 (按钮,时刻,位置)、之前计数、本次点击与当前时刻推导连击计数。
/// 同键 + 间隔/位移均在阈内则递增，否则复位为 1。抽出便于单测（不触 FFI）。
fn derive_click_count(
    last: Option<(Button, std::time::Instant, CGPoint)>,
    prev_count: i64,
    btn: Button,
    pos: CGPoint,
    now: std::time::Instant,
) -> i64 {
    let continued = last.is_some_and(|(b, t, p)| {
        b == btn
            && now.duration_since(t) <= DOUBLE_CLICK_INTERVAL
            && (p.x - pos.x).abs() <= DOUBLE_CLICK_DIST
            && (p.y - pos.y).abs() <= DOUBLE_CLICK_DIST
    });
    if continued { prev_count + 1 } else { 1 }
}

impl Drop for MacInject {
    fn drop(&mut self) {
        let _ = self.release_all();
        if !self.source.is_null() {
            // SAFETY: source 由 CGEventSourceCreate 得到，释放一次
            unsafe { CFRelease(self.source as CFTypeRef) };
            self.source = std::ptr::null_mut();
        }
    }
}

impl InputInject for MacInject {
    fn key(&mut self, key: HidKey, down: bool) -> Result<()> {
        // 先更新修饰键状态，使本次及后续事件携带正确 flags（组合键生效）
        if let Some(mask) = hid_modifier_flag(key.0) {
            if down {
                self.mod_flags |= mask;
            } else {
                self.mod_flags &= !mask;
            }
        }
        let r = match hid_to_vk(key) {
            Some(vk) => {
                self.post_key(vk, down);
                self.tracker.on_key(key, down);
                Ok(())
            }
            None => {
                tracing::debug!(target: "mouseshare::mac", %key, "无对应 vk，丢弃注入");
                Ok(())
            }
        };
        // 强制退出兜底：注入的 ⌘⌥⎋ 被 macOS 当系统热键过滤、不弹窗，故检测到后另走
        // System Events 触发（TECH-REVIEW B-9）。在 post 之后判，此时 mod_flags 已含 ⌘⌥。
        if is_force_quit(key.0, down, self.mod_flags) {
            tracing::info!(
                target: "mouseshare::mac",
                "检测到 ⌘⌥⎋，经 System Events 触发强制退出面板（绕过系统对合成热键的过滤）"
            );
            open_force_quit();
        }
        r
    }

    fn mouse_move_to(&mut self, x: f64, y: f64) -> Result<()> {
        // SAFETY: 查询主屏边界，纯标量返回
        let b = unsafe { CGDisplayBounds(CGMainDisplayID()) };
        // 防御性钳回屏内（T-616）：旧版对端的影子坐标会差一越界（y=h），WindowServer
        // 对越界坐标的事件不做边缘判定，自动隐藏的程序坞唤不出。屏内最后像素是 w-1/h-1。
        let x = x.clamp(b.origin.x, b.origin.x + b.size.w - 1.0);
        let y = y.clamp(b.origin.y, b.origin.y + b.size.h - 1.0);
        let pos = CGPoint { x, y };
        let prev = self.last_pos;
        self.last_pos = pos;
        let delta = move_deltas(
            (prev.x, prev.y),
            (x, y),
            (b.origin.x, b.origin.y, b.size.w, b.size.h),
        );
        // 按住键时发 *MouseDragged（否则 macOS 不识别为拖拽，拖文件失败）；否则普通移动。
        match self.drag {
            Some((etype, cg_btn, bn)) => self.post_mouse(etype, pos, cg_btn, bn, 0, delta),
            None => self.post_mouse(EV_MOUSE_MOVED, pos, MOUSE_BTN_LEFT, None, 0, delta),
        }
        // SAFETY: 标量入参，无别名要求
        unsafe { CGWarpMouseCursorPosition(pos) };
        Ok(())
    }

    fn mouse_button(&mut self, btn: Button, down: bool) -> Result<()> {
        let pos = self.last_pos;
        // (按下事件, 抬起事件, 拖动事件, CG 按钮, button_number)
        let (down_ev, up_ev, drag_ev, cg_btn, bn): (u32, u32, u32, u32, Option<i64>) = match btn {
            Button::Left => (
                EV_LEFT_DOWN,
                EV_LEFT_UP,
                EV_LEFT_DRAGGED,
                MOUSE_BTN_LEFT,
                None,
            ),
            Button::Right => (
                EV_RIGHT_DOWN,
                EV_RIGHT_UP,
                EV_RIGHT_DRAGGED,
                MOUSE_BTN_RIGHT,
                None,
            ),
            Button::Middle => (
                EV_OTHER_DOWN,
                EV_OTHER_UP,
                EV_OTHER_DRAGGED,
                MOUSE_BTN_CENTER,
                Some(2),
            ),
            Button::X1 => (EV_OTHER_DOWN, EV_OTHER_UP, EV_OTHER_DRAGGED, 3, Some(3)),
            Button::X2 => (EV_OTHER_DOWN, EV_OTHER_UP, EV_OTHER_DRAGGED, 4, Some(4)),
        };
        // 连击计数：按下时按时序+位移推导，抬起复用同一计数（双击的 up 也须带 clickState=2）
        let click_state = if down {
            self.next_click_count(btn, pos)
        } else {
            self.click_count.max(1)
        };
        self.post_mouse(
            if down { down_ev } else { up_ev },
            pos,
            cg_btn,
            bn,
            click_state,
            (0, 0),
        );
        // 维护拖动状态：按下记录（后续移动发 *Dragged），抬起清除
        if down {
            self.drag = Some((drag_ev, cg_btn, bn));
        } else if self.drag.map(|(e, _, _)| e == drag_ev).unwrap_or(false) {
            self.drag = None;
        }
        self.tracker.on_button(btn, down);
        Ok(())
    }

    fn scroll(&mut self, dx: f64, dy: f64, precise: bool) -> Result<()> {
        let unit = if precise {
            SCROLL_UNIT_PIXEL
        } else {
            SCROLL_UNIT_LINE
        };
        // wheel1=纵向（向上为正），wheel2=横向
        let w1 = dy.round() as i32;
        let w2 = dx.round() as i32;
        // SAFETY: 变参调用，wheel_count=2 对应随后两个 i32 实参
        unsafe {
            let ev = CGEventCreateScrollWheelEvent(self.source, unit, 2, w1, w2);
            if !ev.is_null() {
                CGEventPost(POST_TAP_HID, ev);
                CFRelease(ev as CFTypeRef);
            }
        }
        Ok(())
    }

    fn release_all(&mut self) -> Result<()> {
        let (keys, buttons) = self.tracker.drain_pressed();
        for key in keys {
            // 抬起修饰键时同步清除其 flags，使递减后的 up 事件 flags 正确，最终归 0
            if let Some(mask) = hid_modifier_flag(key.0) {
                self.mod_flags &= !mask;
            }
            if let Some(vk) = hid_to_vk(key) {
                self.post_key(vk, false);
            }
        }
        let pos = self.last_pos;
        for btn in buttons {
            let (etype, cgbtn, num) = match btn {
                Button::Left => (EV_LEFT_UP, MOUSE_BTN_LEFT, None),
                Button::Right => (EV_RIGHT_UP, MOUSE_BTN_RIGHT, None),
                Button::Middle => (EV_OTHER_UP, MOUSE_BTN_CENTER, Some(2)),
                Button::X1 => (EV_OTHER_UP, 3, Some(3)),
                Button::X2 => (EV_OTHER_UP, 4, Some(4)),
            };
            self.post_mouse(etype, pos, cgbtn, num, 0, (0, 0));
        }
        self.drag = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 修饰键hid映射到cgevent_flags() {
        assert_eq!(hid_modifier_flag(0xE0), Some(FLAG_CONTROL), "LCtrl");
        assert_eq!(hid_modifier_flag(0xE4), Some(FLAG_CONTROL), "RCtrl");
        assert_eq!(hid_modifier_flag(0xE3), Some(FLAG_CMD), "LGui/Command");
        assert_eq!(hid_modifier_flag(0xE7), Some(FLAG_CMD), "RGui/Command");
        assert_eq!(hid_modifier_flag(0xE2), Some(FLAG_ALT), "LAlt/Option");
        assert_eq!(hid_modifier_flag(0xE1), Some(FLAG_SHIFT), "LShift");
        assert_eq!(hid_modifier_flag(0x04), None, "字母键非修饰键");
        assert_eq!(hid_modifier_flag(0x29), None, "Esc 非修饰键");
    }

    #[test]
    fn 强制退出组合识别() {
        // Esc 按下 + 同时按住 Cmd 与 Option → 触发
        assert!(is_force_quit(HID_ESC, true, FLAG_CMD | FLAG_ALT));
        assert!(is_force_quit(
            HID_ESC,
            true,
            FLAG_CMD | FLAG_ALT | FLAG_SHIFT
        ));
        // 缺任一修饰键不触发
        assert!(!is_force_quit(HID_ESC, true, FLAG_CMD));
        assert!(!is_force_quit(HID_ESC, true, FLAG_ALT));
        assert!(!is_force_quit(HID_ESC, true, 0));
        // 抬起不触发
        assert!(!is_force_quit(HID_ESC, false, FLAG_CMD | FLAG_ALT));
        // 非 Esc 不触发
        assert!(!is_force_quit(0x04, true, FLAG_CMD | FLAG_ALT));
    }

    #[test]
    fn 连击计数推导() {
        use std::time::Duration;
        let t0 = std::time::Instant::now();
        let p = CGPoint { x: 100.0, y: 100.0 };

        // 首次点击 → 1
        assert_eq!(derive_click_count(None, 0, Button::Left, p, t0), 1);
        // 间隔与位移都在阈内 → 递增到 2、3（双击/三击）
        let c2 = derive_click_count(
            Some((Button::Left, t0, p)),
            1,
            Button::Left,
            p,
            t0 + Duration::from_millis(200),
        );
        assert_eq!(c2, 2, "快速同位同键应识别为双击");
        let c3 = derive_click_count(
            Some((Button::Left, t0, p)),
            2,
            Button::Left,
            p,
            t0 + Duration::from_millis(300),
        );
        assert_eq!(c3, 3, "再一次应为三击");
        // 超时复位
        assert_eq!(
            derive_click_count(
                Some((Button::Left, t0, p)),
                1,
                Button::Left,
                p,
                t0 + Duration::from_millis(600)
            ),
            1,
            "间隔超过 500ms 复位为单击"
        );
        // 位移过大复位
        let far = CGPoint { x: 200.0, y: 200.0 };
        assert_eq!(
            derive_click_count(
                Some((Button::Left, t0, p)),
                1,
                Button::Left,
                far,
                t0 + Duration::from_millis(100)
            ),
            1,
            "位移超阈复位为单击"
        );
        // 不同按钮复位
        assert_eq!(
            derive_click_count(
                Some((Button::Left, t0, p)),
                1,
                Button::Right,
                p,
                t0 + Duration::from_millis(100)
            ),
            1,
            "换按钮复位为单击"
        );
    }

    #[test]
    fn 移动delta_实位移与贴边合成推挤() {
        let b = (0.0, 0.0, 1440.0, 900.0);
        // 正常移动：delta = 实际位移
        assert_eq!(move_deltas((100.0, 100.0), (103.0, 98.0), b), (3, -2));
        // 屏内静止：无 delta
        assert_eq!(move_deltas((100.0, 100.0), (100.0, 100.0), b), (0, 0));
        // 贴下边被 clamp 后连续同位置（控制端仍在推）→ 合成向下推挤，唤出隐藏程序坞
        assert_eq!(
            move_deltas((700.0, 900.0), (700.0, 900.0), b),
            (0, EDGE_NUDGE)
        );
        // 贴上/左/右边同理
        assert_eq!(move_deltas((700.0, 0.0), (700.0, 0.0), b), (0, -EDGE_NUDGE));
        assert_eq!(move_deltas((0.0, 400.0), (0.0, 400.0), b), (-EDGE_NUDGE, 0));
        assert_eq!(
            move_deltas((1440.0, 400.0), (1440.0, 400.0), b),
            (EDGE_NUDGE, 0)
        );
        // 回归（T-615）：贴底边横向滑动（x 有实位移、y 被 clamp 为 0）时 y 轴仍须
        // 合成向下推挤——手推鼠标横向必然抖动，整体「有位移就跳过合成」会让程序坞
        // 几乎永远唤不出。x 轴保留实位移，不叠加合成量。
        assert_eq!(
            move_deltas((699.0, 900.0), (700.0, 900.0), b),
            (1, EDGE_NUDGE)
        );
        // 有实位移的轴不合成：斜向离开底边，两轴均用实位移
        assert_eq!(move_deltas((700.0, 900.0), (703.0, 895.0), b), (3, -5));
    }
}
