//! ms-platform —— 平台抽象层（DEVELOPMENT.md §2.2）。
//!
//! 只含 trait 与中立类型，不依赖任何系统 API；具体实现位于
//! ms-platform-windows / ms-platform-macos。坐标与键码约定见 SPEC.md §8：
//! 左上原点、y 向下、f64 物理像素；键码统一 USB HID Usage ID（page 0x07）。

pub mod stub;
pub mod synthetic;

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

// ─────────────────────────── 基础类型 ───────────────────────────

/// USB HID Usage ID（Keyboard/Keypad page 0x07）的中立键码。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct HidKey(pub u16);

impl fmt::Display for HidKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HID(0x{:02X})", self.0)
    }
}

/// 鼠标按键。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Button {
    Left,
    Right,
    Middle,
    /// 侧键（后退）
    X1,
    /// 侧键（前进）
    X2,
}

/// 矩形（左上原点，物理像素）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// 本机多显示器并集抽象的虚拟屏幕（DEVELOPMENT.md §4.3）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VirtualScreen {
    pub rect: Rect,
    /// 主屏缩放比（UI 提示用；坐标本身已是物理像素）
    pub scale: f32,
}

/// 捕获层产出的原始输入事件。
/// 鼠标移动为相对位移（Raw Input / tap 的 delta，未加速），
/// 由 ms-core 的影子光标模型累积为绝对坐标（TECH-REVIEW A-4）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum InputEvent {
    Key {
        key: HidKey,
        down: bool,
    },
    /// 鼠标移动：`dx/dy` 为相对位移（REMOTE 影子累积用，TECH-REVIEW A-4）；
    /// `ax/ay` 为捕获时的绝对光标位置（LOCAL 边缘判定用，更可靠）。
    MouseMove {
        dx: f64,
        dy: f64,
        ax: f64,
        ay: f64,
    },
    Button {
        btn: Button,
        down: bool,
    },
    /// precise=true 表示平滑滚动（Mac 触控板/精密滚轮）
    Scroll {
        dx: f64,
        dy: f64,
        precise: bool,
    },
}

/// 剪贴板中立内容（协议格式映射见 DEVELOPMENT.md §5.2）。
#[derive(Debug, Clone, PartialEq)]
pub enum ClipboardContent {
    Text(String),
    /// 纯 HTML fragment（UTF-8）；CF_HTML 头部封装在 Windows 实现内处理
    Html(String),
    Png(Vec<u8>),
}

/// 把 HTML fragment 退化为纯文本：去标签 + 解码常见实体。用于写剪贴板时附带
/// 一份纯文本格式，使**只认纯文本的目标**（浏览器地址栏/搜索栏、终端等）也能粘贴
/// （否则只写 HTML 格式时这些控件无内容可粘）。退化文本仅作兼容回退，富文本控件
/// 仍优先用 HTML 格式。
pub fn html_to_text(html: &str) -> String {
    // 1) 去标签：丢弃 `<...>` 之间的内容；块级标签换行近似排版
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut tag = String::new();
    for ch in html.chars() {
        match ch {
            '<' => {
                in_tag = true;
                tag.clear();
            }
            '>' => {
                in_tag = false;
                let lower = tag.to_ascii_lowercase();
                let is_close = lower.starts_with('/');
                let name = lower.trim_start_matches('/');
                let block = name.starts_with("p")
                    || name.starts_with("div")
                    || name.starts_with("li")
                    || name.starts_with("tr");
                // 闭合块标签 / 自闭合 <br> 处换行（开标签不换，避免重复空行）
                if name.starts_with("br") || (block && is_close) {
                    out.push('\n');
                }
            }
            _ if in_tag => tag.push(ch),
            _ => out.push(ch),
        }
    }
    // 2) 解码常见实体
    let out = out
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'");
    // 3) 收尾：去掉首尾空白（保留中间换行/空格，URL 等不被破坏）
    out.trim().to_string()
}

/// 事件回调。实现方在捕获线程同步调用；**必须 O(1) 返回、不得阻塞**
/// （钩子/tap 回调时限见 TECH-REVIEW A-2/A-3），重活交给消费侧。
pub type EventSink = Box<dyn FnMut(InputEvent) + Send + 'static>;

// ─────────────────────────── 错误类型 ───────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    #[error("功能尚未实现: {0}")]
    Unimplemented(&'static str),
    #[error("系统权限不足: {0}")]
    PermissionDenied(String),
    #[error("系统 API 调用失败: {0}")]
    Api(String),
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, PlatformError>;

// ─────────────────────────── 能力 trait ───────────────────────────

/// 本机键鼠捕获。
pub trait InputCapture: Send {
    /// 周期检查捕获线程是否仍可用；故障须由应用解除 grab 并释放输入。
    fn check_health(&self) -> Result<()> {
        Ok(())
    }
    /// 开始捕获；grab=true 时拦截事件不让本机响应（控制对端期间）。
    fn start(&mut self, sink: EventSink, grab: bool) -> Result<()>;
    /// 动态切换 grab（进入/退出 REMOTE 的控制权事务，DEVELOPMENT §2.3）。
    fn set_grab(&mut self, grab: bool) -> Result<()>;
    fn stop(&mut self) -> Result<()>;
    /// 将光标钉在屏幕中心并隐藏（REMOTE 期间防止本机光标移动）。
    fn confine_cursor(&mut self, on: bool) -> Result<()>;
}

/// 键鼠事件注入。
pub trait InputInject: Send {
    fn key(&mut self, key: HidKey, down: bool) -> Result<()>;
    /// 绝对移动（本机虚拟屏幕坐标，物理像素）。
    fn mouse_move_to(&mut self, x: f64, y: f64) -> Result<()>;
    fn mouse_button(&mut self, btn: Button, down: bool) -> Result<()>;
    fn scroll(&mut self, dx: f64, dy: f64, precise: bool) -> Result<()>;
    /// 释放所有按下状态。断连/切换/权限回退的安全事务成员（FR-305），
    /// 实现必须幂等且尽力而为（单键失败不中断其余释放）。
    fn release_all(&mut self) -> Result<()>;
}

/// 系统剪贴板访问。
pub trait ClipboardAccess: Send {
    /// 读取当前内容；机密标记内容（FR-405）返回 None。
    fn read(&mut self) -> Result<Option<ClipboardContent>>;
    /// 写入；实现应同时放入该平台全部等价格式（DEVELOPMENT.md §5.2）。
    fn write(&mut self, content: &ClipboardContent) -> Result<()>;
    /// 平台变更序号（Windows GetClipboardSequenceNumber / macOS changeCount），
    /// 用于"切换时机同步"判断是否有新内容（FR-402）。
    fn change_count(&mut self) -> Result<u64>;

    /// 读取剪贴板中的文件引用（Finder/资源管理器复制的文件，mac `public.file-url`、
    /// Windows `CF_HDROP`），无文件时返回空。用于粘贴式文件传输（FR-501/502）：
    /// 复制文件后切换控制权，源端读出路径并发往对端落地。默认无（非文件平台/未实现）。
    fn read_files(&mut self) -> Result<Vec<PathBuf>> {
        Ok(Vec::new())
    }

    /// 把一组本地文件路径写入剪贴板，使后续粘贴可落地这些文件（接收端落地后调用，
    /// 让对端 Cmd+V/Ctrl+V 能粘贴）。默认未实现（接收仍会落到接收目录，FR-501）。
    fn write_files(&mut self, paths: &[PathBuf]) -> Result<()> {
        let _ = paths;
        Err(PlatformError::Unimplemented("write_files"))
    }
}

/// 屏幕信息。
pub trait ScreenInfo: Send {
    /// 多显示器并集的虚拟屏幕外边界（FR-204）。
    fn virtual_screen(&mut self) -> Result<VirtualScreen>;
    /// 键盘布局标识（如 "com.apple.keylayout.ABC" / KLID 字符串），
    /// 用于布局一致性检测（TECH-REVIEW B-3）。
    fn keyboard_layout(&mut self) -> Result<String>;
}

/// 开机自启。
pub trait Autostart: Send {
    fn set_enabled(&mut self, on: bool) -> Result<()>;
    fn is_enabled(&mut self) -> Result<bool>;
}

/// 安全存储（macOS Keychain / Windows DPAPI，FR-703）。
pub trait SecureStore: Send {
    fn get(&mut self, key: &str) -> Result<Option<Vec<u8>>>;
    fn set(&mut self, key: &str, value: &[u8]) -> Result<()>;
    fn delete(&mut self, key: &str) -> Result<()>;
}

/// 平台能力工厂：每个平台 crate 暴露一个实现，app 层经 cfg 选择。
pub trait Platform {
    fn name(&self) -> &'static str;
    fn input_capture(&self) -> Result<Box<dyn InputCapture>>;
    fn input_inject(&self) -> Result<Box<dyn InputInject>>;
    fn clipboard(&self) -> Result<Box<dyn ClipboardAccess>>;
    fn screen_info(&self) -> Result<Box<dyn ScreenInfo>>;
    fn autostart(&self) -> Result<Box<dyn Autostart>>;
    fn secure_store(&self) -> Result<Box<dyn SecureStore>>;

    /// 输入捕获/注入所需的系统授权是否已就绪（macOS「辅助功能」；Windows 普通进程无需
    /// 特殊授权，默认 `true`）。供首次使用引导页检测（FR-803）。
    fn has_input_permission(&self) -> bool {
        true
    }

    /// 引导用户开启输入授权：把本应用加入授权列表并打开系统设置对应页。默认 no-op。
    fn open_input_permission_settings(&self) {}
}

#[cfg(test)]
mod tests {
    use super::html_to_text;

    #[test]
    fn html_退化纯文本_链接取可见文本() {
        assert_eq!(
            html_to_text(r#"<a href="https://example.com">点我</a>"#),
            "点我"
        );
    }

    #[test]
    fn html_退化纯文本_解码实体与去标签() {
        assert_eq!(html_to_text("<b>a&amp;b</b>&nbsp;c&lt;d&gt;"), "a&b c<d>");
    }

    #[test]
    fn html_退化纯文本_纯url原样() {
        // 地址栏最常见场景：富文本里就是一个 URL
        assert_eq!(
            html_to_text("<span>https://example.com/a?b=1&amp;c=2</span>"),
            "https://example.com/a?b=1&c=2"
        );
    }

    #[test]
    fn html_退化纯文本_块级换行() {
        assert_eq!(html_to_text("<p>一</p><p>二</p>"), "一\n二");
    }
}
