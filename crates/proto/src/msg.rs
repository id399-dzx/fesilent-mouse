//! 消息载荷定义（DEVELOPMENT.md §4.2）。
//! 每条消息一个独立 struct，编号与通道见 SPEC.md §3（编号在 wire.rs 锁定）。

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 矩形（左上原点、物理像素；SPEC §8）。协议层独立定义，避免依赖平台层。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// 屏幕边缘。ratio 方向约定：沿该边从左→右 / 上→下（SPEC §8）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

/// 屏幕布局：对端相对本端的方位（v1.0 双机一维布局，FR-201）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Layout {
    pub peer_position: Edge,
}

// ── Stream 0 控制消息 ──

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub proto_ver: u16,
    pub device_id: Uuid,
    pub name: String,
    pub app_ver: String,
    /// 键盘布局标识（macOS TISCopyCurrentKeyboardInputSource /
    /// Windows GetKeyboardLayout 串），用于布局一致性检测（TECH-REVIEW B-3）。
    pub kb_layout: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairRequest {
    pub cert_fingerprint: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairConfirm {
    /// 配对码（SAS）人工确认结果
    pub sas_verified: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ScreenInfo {
    pub virtual_rect: Rect,
    pub scale: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayoutUpdate {
    pub layout: Layout,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EnterScreen {
    pub entry_edge: Edge,
    /// 进入点在该边上的归一化位置 0.0–1.0
    pub ratio: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaveScreen {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ping {
    pub seq: u32,
    /// 发送端单调毫秒时间戳（不做跨机时钟假设，SPEC §8）
    pub t0: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pong {
    pub seq: u32,
    pub t0: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseAll {}

/// 睡眠预告（FR-604）：对端收到后进入等待态，跳过误判断连。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Suspend {}

// ── Stream 1 键鼠（可靠有序）──

/// 键盘事件；冗余携带影子坐标，注入端先校正位置（TECH-REVIEW A-4）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct KeyEvent {
    pub hid: u16,
    pub down: bool,
    pub shadow: (f32, f32),
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MouseButton {
    pub btn: u8,
    pub down: bool,
    pub shadow: (f32, f32),
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Scroll {
    pub dx: f32,
    pub dy: f32,
    pub precise: bool,
}

// ── Datagram 鼠标移动（不可靠，绝对影子坐标 + 序号）──

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MouseMove {
    pub seq: u32,
    pub x: f32,
    pub y: f32,
}

// ── Stream 2 剪贴板 ──

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipboardData {
    /// 中立 MIME：text/plain; utf-8 | text/html | image/png（DEVELOPMENT §5.2）
    pub mime: String,
    /// 压缩后总字节数（zstd）
    pub total: u32,
    pub chunk: Vec<u8>,
}

// ── Stream 3+ 文件传输 ──

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileOffer {
    pub id: Uuid,
    pub name: String,
    pub size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileAccept {
    pub id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChunk {
    pub id: Uuid,
    pub offset: u64,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDone {
    pub id: Uuid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileCancel {
    pub id: Uuid,
}
