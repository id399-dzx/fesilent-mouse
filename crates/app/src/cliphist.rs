//! 跨机剪贴板历史（T-808）：进程级保留最近 N 条经同步通道流转的剪贴板内容
//! （本机发往对端的 + 对端同步过来的），供设置窗查看与一键回填。
//!
//! - 仅存内存、不落盘（隐私）；机密内容（FR-405）在读取层已被跳过、超限内容在
//!   编码层被拒，两者天然不会进入历史。
//! - 记录时机 = 同步时（控制权切换触发，FR-402）：未跨机的本地复制不记录
//!   （全量记录需常驻轮询本机剪贴板，隐私与开销都不划算，暂不做）。
//! - 防内存暴涨：单条超过 [`REFILL_MAX`] 只记预览、不支持回填（refill=None）；
//!   条数上限默认 20，可经 config.json `clip_history` 调整（0=关闭）。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};

use ms_platform::ClipboardContent;

/// 默认保留条数（config.json `clip_history`；0=关闭历史）。
pub const DEFAULT_CAP: usize = 20;

/// 单条可回填内容的大小上限：更大只记预览（全满时内存至多 ~cap MiB 量级）。
const REFILL_MAX: usize = 1024 * 1024;

/// 预览最长字符数（按字符截断，多字节安全）。
const PREVIEW_CHARS: usize = 60;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Text,
    Html,
    Png,
}

#[derive(Clone)]
pub struct Entry {
    pub kind: Kind,
    /// 单行预览：文本/HTML 取（退化）文本首个非空行截断；图片为大小提示。
    pub preview: String,
    /// 完整内容；超过 [`REFILL_MAX`] 为 None（UI 显示"过大"、不可回填）。
    pub refill: Option<ClipboardContent>,
    pub bytes: usize,
    /// true = 对端同步来的；false = 本机发往对端的。
    pub from_peer: bool,
}

struct Hist {
    items: Mutex<Vec<Entry>>,
    cap: AtomicUsize,
}

static HIST: LazyLock<Hist> = LazyLock::new(|| Hist {
    items: Mutex::new(Vec::new()),
    cap: AtomicUsize::new(DEFAULT_CAP),
});

fn lock_items() -> std::sync::MutexGuard<'static, Vec<Entry>> {
    HIST.items.lock().unwrap_or_else(|e| e.into_inner())
}

/// 启动时按 config 设置保留条数；0=关闭并清空，收缩时截断到新上限。
pub fn set_cap(cap: usize) {
    HIST.cap.store(cap, Ordering::Release);
    let mut items = lock_items();
    items.truncate(cap);
}

/// 记录一条经同步的剪贴板内容（进程级单例入口）。
pub fn record(c: &ClipboardContent, from_peer: bool) {
    let cap = HIST.cap.load(Ordering::Acquire);
    record_into(&mut lock_items(), cap, c, from_peer);
}

/// UI 快照（新的在前）。
pub fn snapshot() -> Vec<Entry> {
    lock_items().clone()
}

/// 清空历史。
pub fn clear() {
    lock_items().clear();
}

/// 核心记录逻辑（与单例解耦，可单测）：新在前、连续重复去重、按 cap 截断。
fn record_into(items: &mut Vec<Entry>, cap: usize, c: &ClipboardContent, from_peer: bool) {
    if cap == 0 {
        return;
    }
    let (kind, bytes) = content_meta(c);
    let preview = preview_of(c);
    // 连续重复（同内容多次来回穿越）不刷屏：与最新一条同类型同大小同预览即跳过。
    if let Some(top) = items.first()
        && top.kind == kind
        && top.bytes == bytes
        && top.preview == preview
    {
        return;
    }
    let refill = (bytes <= REFILL_MAX).then(|| c.clone());
    items.insert(
        0,
        Entry {
            kind,
            preview,
            refill,
            bytes,
            from_peer,
        },
    );
    items.truncate(cap);
}

fn content_meta(c: &ClipboardContent) -> (Kind, usize) {
    match c {
        ClipboardContent::Text(s) => (Kind::Text, s.len()),
        ClipboardContent::Html(s) => (Kind::Html, s.len()),
        ClipboardContent::Png(b) => (Kind::Png, b.len()),
    }
}

fn preview_of(c: &ClipboardContent) -> String {
    let text = match c {
        ClipboardContent::Text(s) => s.clone(),
        ClipboardContent::Html(s) => ms_platform::html_to_text(s),
        ClipboardContent::Png(b) => return format!("[图片 {}]", human_bytes(b.len())),
    };
    let line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    let mut out: String = line.chars().take(PREVIEW_CHARS).collect();
    if line.chars().count() > PREVIEW_CHARS {
        out.push('…');
    }
    if out.is_empty() {
        out = "（空白文本）".into();
    }
    out
}

fn human_bytes(b: usize) -> String {
    if b < 1024 {
        format!("{b} B")
    } else if b < 1024 * 1024 {
        format!("{:.1} KB", b as f64 / 1024.0)
    } else {
        format!("{:.1} MB", b as f64 / (1024.0 * 1024.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 记录_去重_截断_回填口径() {
        let mut items = Vec::new();
        // 新在前
        record_into(&mut items, 3, &ClipboardContent::Text("一".into()), false);
        record_into(&mut items, 3, &ClipboardContent::Text("二".into()), true);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].preview, "二");
        assert!(items[0].from_peer);
        // 连续重复去重
        record_into(&mut items, 3, &ClipboardContent::Text("二".into()), true);
        assert_eq!(items.len(), 2, "连续同内容不重复落账");
        // 截断到 cap
        record_into(&mut items, 3, &ClipboardContent::Text("三".into()), false);
        record_into(&mut items, 3, &ClipboardContent::Text("四".into()), false);
        assert_eq!(items.len(), 3);
        assert_eq!(items[2].preview, "二", "最旧的「一」被挤出");
        // cap=0 = 关闭
        let before = items.len();
        record_into(&mut items, 0, &ClipboardContent::Text("五".into()), false);
        assert_eq!(items.len(), before);

        // 超限内容只记预览、不可回填
        let big = "x".repeat(REFILL_MAX + 1);
        record_into(&mut items, 9, &ClipboardContent::Text(big), false);
        assert!(items[0].refill.is_none(), "超限不支持回填");
        assert!(items[0].preview.chars().count() <= PREVIEW_CHARS + 1);
        // 小内容可回填
        record_into(&mut items, 9, &ClipboardContent::Text("小".into()), false);
        assert!(matches!(
            items[0].refill,
            Some(ClipboardContent::Text(ref s)) if s == "小"
        ));
    }

    #[test]
    fn 预览_多字节截断与图片提示() {
        // 61 个中文字符 → 截 60 + 省略号，不 panic（按字符不按字节）
        let long: String = "汉".repeat(PREVIEW_CHARS + 1);
        let p = preview_of(&ClipboardContent::Text(long));
        assert_eq!(p.chars().count(), PREVIEW_CHARS + 1);
        assert!(p.ends_with('…'));
        // 取首个非空行
        let p = preview_of(&ClipboardContent::Text("\n\n  第一行  \n第二行".into()));
        assert_eq!(p, "第一行");
        // HTML 走退化文本
        let p = preview_of(&ClipboardContent::Html("<b>加粗</b>".into()));
        assert!(p.contains("加粗"));
        // 图片显示大小
        let p = preview_of(&ClipboardContent::Png(vec![0u8; 2048]));
        assert_eq!(p, "[图片 2.0 KB]");
    }
}
