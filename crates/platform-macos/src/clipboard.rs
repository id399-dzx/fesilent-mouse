//! macOS 剪贴板读写（NSPasteboard；DEVELOPMENT.md §3.2，FR-401/405）。
//!
//! 仅在 macOS 编译。三类中立内容 ↔ NSPasteboard 类型；机密内容
//! （`org.nspasteboard.ConcealedType`，密码管理器标记）跳过不读（FR-405）。
//! 变更检测用 `changeCount`（上层 200ms 轮询，FR-402）。

use std::path::{Path, PathBuf};

use ms_platform::{ClipboardAccess, ClipboardContent, PlatformError, Result};

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{
    NSPasteboard, NSPasteboardItem, NSPasteboardTypeFileURL, NSPasteboardTypeHTML,
    NSPasteboardTypePNG, NSPasteboardTypeString, NSPasteboardWriting,
};
use objc2_foundation::{NSArray, NSData, NSString};

/// 密码管理器等标记的"机密"剪贴板类型（FR-405）。
const CONCEALED_TYPE: &str = "org.nspasteboard.ConcealedType";

pub struct MacClipboard;

impl MacClipboard {
    pub fn new() -> Self {
        Self
    }

    fn general() -> objc2::rc::Retained<NSPasteboard> {
        NSPasteboard::generalPasteboard()
    }

    /// 当前剪贴板是否被标记为机密。
    fn is_concealed(pb: &NSPasteboard) -> bool {
        let Some(types) = pb.types() else {
            return false;
        };
        types.iter().any(|t| t.to_string() == CONCEALED_TYPE)
    }
}

impl Default for MacClipboard {
    fn default() -> Self {
        Self::new()
    }
}

impl ClipboardAccess for MacClipboard {
    fn read(&mut self) -> Result<Option<ClipboardContent>> {
        let pb = Self::general();
        if Self::is_concealed(&pb) {
            return Ok(None); // 机密内容不读（FR-405）
        }
        // SAFETY: NSPasteboardType* 是 AppKit 导出的不可变全局字符串常量，读取安全
        let (png_t, html_t, str_t) = unsafe {
            (
                NSPasteboardTypePNG,
                NSPasteboardTypeHTML,
                NSPasteboardTypeString,
            )
        };
        // 优先级：图片 > HTML > 文本
        if let Some(data) = pb.dataForType(png_t) {
            return Ok(Some(ClipboardContent::Png(data.to_vec())));
        }
        if let Some(s) = pb.stringForType(html_t) {
            return Ok(Some(ClipboardContent::Html(s.to_string())));
        }
        if let Some(s) = pb.stringForType(str_t) {
            return Ok(Some(ClipboardContent::Text(s.to_string())));
        }
        Ok(None)
    }

    fn write(&mut self, content: &ClipboardContent) -> Result<()> {
        let pb = Self::general();
        pb.clearContents();
        // SAFETY: NSPasteboardType* 为 AppKit 导出的不可变全局字符串常量，读取安全
        let (str_t, html_t, png_t) = unsafe {
            (
                NSPasteboardTypeString,
                NSPasteboardTypeHTML,
                NSPasteboardTypePNG,
            )
        };
        let ok = match content {
            ClipboardContent::Text(s) => pb.setString_forType(&NSString::from_str(s), str_t),
            ClipboardContent::Html(s) => {
                // 同时写 HTML 与纯文本：富文本控件用 HTML，地址栏/搜索栏等只认纯文本的
                // 目标用退化文本（否则只写 HTML 时这些控件无内容可粘）。
                let html_ok = pb.setString_forType(&NSString::from_str(s), html_t);
                let plain = ms_platform::html_to_text(s);
                let text_ok = pb.setString_forType(&NSString::from_str(&plain), str_t);
                html_ok && text_ok
            }
            ClipboardContent::Png(bytes) => {
                let data = NSData::with_bytes(bytes);
                pb.setData_forType(Some(&data), png_t)
            }
        };
        if !ok {
            return Err(PlatformError::Api("写入剪贴板失败".into()));
        }
        Ok(())
    }

    fn change_count(&mut self) -> Result<u64> {
        let pb = Self::general();
        let c = pb.changeCount();
        Ok(c as u64)
    }

    fn read_files(&mut self) -> Result<Vec<PathBuf>> {
        let pb = Self::general();
        // 优先 NSFilenamesPboardType：Finder 提供的**真实绝对路径**数组。
        // ⚠ Finder 的 `public.file-url` 是文件引用形式 `file:///.file/id=NNN.MMM`（不可直接当
        // 路径用，打开会失败/读错——曾导致传过去文件名与内容全错，TECH-REVIEW B-10）。
        let names_t = NSString::from_str("NSFilenamesPboardType");
        if let Some(plist) = pb.propertyListForType(&names_t)
            && let Ok(arr) = plist.downcast::<NSArray>()
        {
            let mut out = Vec::with_capacity(arr.count());
            for i in 0..arr.count() {
                if let Ok(s) = arr.objectAtIndex(i).downcast::<NSString>() {
                    out.push(PathBuf::from(s.to_string()));
                }
            }
            if !out.is_empty() {
                return Ok(out);
            }
        }
        // 回退：逐条 file-url（osascript/其它应用可能用真实路径形式的 file-url）。
        // SAFETY: NSPasteboardTypeFileURL 是 AppKit 导出的不可变全局字符串常量
        let file_url_t = unsafe { NSPasteboardTypeFileURL };
        let Some(items) = pb.pasteboardItems() else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for i in 0..items.count() {
            let item = items.objectAtIndex(i);
            if let Some(s) = item.stringForType(file_url_t)
                && let Some(p) = file_url_to_path(&s.to_string())
            {
                out.push(p);
            }
        }
        Ok(out)
    }

    fn write_files(&mut self, paths: &[PathBuf]) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        let pb = Self::general();
        // SAFETY: NSPasteboardTypeFileURL 是 AppKit 导出的不可变全局字符串常量
        let file_url_t = unsafe { NSPasteboardTypeFileURL };
        let mut items: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> =
            Vec::with_capacity(paths.len());
        for p in paths {
            let url = NSString::from_str(&path_to_file_url(p));
            let item = NSPasteboardItem::new();
            if !item.setString_forType(&url, file_url_t) {
                return Err(PlatformError::Api("设置剪贴板文件 URL 失败".into()));
            }
            items.push(ProtocolObject::from_retained(item));
        }
        let array = NSArray::from_retained_slice(&items);
        pb.clearContents();
        if !pb.writeObjects(&array) {
            return Err(PlatformError::Api("写入剪贴板文件失败".into()));
        }
        Ok(())
    }
}

/// 本地路径 → `file://` URL（百分号编码路径中的非安全字节）。
fn path_to_file_url(path: &Path) -> String {
    let s = path.to_string_lossy();
    let mut out = String::from("file://");
    for &b in s.as_bytes() {
        // 路径分隔符与 RFC 3986 unreserved 字符原样保留，其余百分号编码
        if b == b'/' || b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// 把 `file://` URL 还原为本地路径（去 scheme + 百分号解码）。非 file URL 返回 None。
fn file_url_to_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    // 可能为 file://localhost/path 或 file:///path
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let decoded = percent_decode(rest);
    if decoded.is_empty() {
        None
    } else {
        Some(PathBuf::from(decoded))
    }
}

/// 最小百分号解码（%XX → 字节），用于还原文件 URL 中的空格等转义。
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn file_url_还原路径() {
        assert_eq!(
            file_url_to_path("file:///Users/me/a.txt"),
            Some(PathBuf::from("/Users/me/a.txt"))
        );
        // 百分号解码：空格、中文
        assert_eq!(
            file_url_to_path("file:///Users/me/my%20file.txt"),
            Some(PathBuf::from("/Users/me/my file.txt"))
        );
        assert_eq!(
            file_url_to_path("file://localhost/tmp/x"),
            Some(PathBuf::from("/tmp/x"))
        );
        // 非 file URL
        assert_eq!(file_url_to_path("https://x/y"), None);
    }

    // 调试：打印当前系统剪贴板的所有类型 + read_files 结果（排查 Finder 复制文件读不到）。
    #[ignore = "手动：先在 Finder 复制文件，再 cargo test -p ms-platform-macos -- --ignored --nocapture 调试_打印剪贴板"]
    #[test]
    fn 调试_打印剪贴板() {
        let pb = MacClipboard::general();
        if let Some(types) = pb.types() {
            println!("=== 剪贴板类型 ({}) ===", types.count());
            for i in 0..types.count() {
                println!("  type[{i}] = {}", types.objectAtIndex(i));
            }
        } else {
            println!("=== 剪贴板无类型 ===");
        }
        let mut c = MacClipboard::new();
        println!("read_files() = {:?}", c.read_files());
    }

    // 真实 NSPasteboard 文件读写往返（验证 write_files/read_files FFI）。沙箱无 AppKit，标 ignore。
    #[ignore = "需 macOS AppKit 运行环境；非沙箱本机实测：cargo test -p ms-platform-macos -- --ignored"]
    #[test]
    fn 文件读写往返() {
        let mut c = MacClipboard::new();
        let paths = vec![
            PathBuf::from("/tmp/ms-test-a.txt"),
            PathBuf::from("/tmp/ms test 你好.png"),
        ];
        c.write_files(&paths).unwrap();
        let got = c.read_files().unwrap();
        assert_eq!(got, paths, "写入剪贴板的文件路径应能原样读回");
    }

    // 回归：写 HTML 后剪贴板应同时有纯文本格式，否则地址栏/搜索栏粘不上（用户回报）。
    #[ignore = "需 macOS AppKit 运行环境；非沙箱本机实测"]
    #[test]
    fn 写html也带纯文本格式() {
        let mut c = MacClipboard::new();
        c.write(&ClipboardContent::Html(
            r#"<a href="https://example.com">链接文本</a>"#.into(),
        ))
        .unwrap();
        let pb = MacClipboard::general();
        // SAFETY: AppKit 导出的全局常量
        let str_t = unsafe { NSPasteboardTypeString };
        let plain = pb.stringForType(str_t).map(|s| s.to_string());
        assert_eq!(
            plain.as_deref(),
            Some("链接文本"),
            "写 HTML 后应有纯文本回退格式，地址栏才能粘"
        );
    }

    // 临时调试：write_files() 后，用【系统级 pbcopy】（不经过我们代码）复制新文本，
    // 看 read_files()/read() 是否正确反映新文本（排查"我们写的文件引用是否会被真实
    // 系统复制正确覆盖"——之前的测试只验证了"我们自己的 write() 覆盖"，未验证真实场景）。
    #[ignore = "手动实测：需系统剪贴板 + pbcopy"]
    #[test]
    fn 调试_写文件后系统级pbcopy复制文本() {
        let mut c = MacClipboard::new();
        c.write_files(&[PathBuf::from("/tmp/ms-probe.txt")])
            .unwrap();
        println!("write_files 后 read_files = {:?}", c.read_files());
        println!("write_files 后 change_count = {:?}", c.change_count());

        // 用系统级 osascript（真实模拟用户 Cmd+C，不经过我们任何代码；比 pbcopy 更可靠，
        // 不受当前 shell locale 影响，见调试记录：沙箱 shell LANG=C 导致 pbcopy 对多字节
        // UTF-8 处理异常，osascript 走 AppleScript 层不受影响）覆盖为纯文本
        use std::process::Command;
        let status = Command::new("osascript")
            .arg("-e")
            .arg("set the clipboard to \"系统级新文本\"")
            .status()
            .unwrap();
        assert!(status.success(), "osascript 设置剪贴板失败");

        println!("pbcopy 后 read_files   = {:?}", c.read_files());
        println!("pbcopy 后 change_count = {:?}", c.change_count());
        println!("pbcopy 后 read         = {:?}", c.read());
        assert!(
            c.read_files().unwrap().is_empty(),
            "系统级复制文本后 read_files 应为空"
        );
        assert_eq!(
            c.read().unwrap(),
            Some(ClipboardContent::Text("系统级新文本".into())),
            "系统级复制文本后 read 应拿到新文本"
        );
    }

    #[test]
    fn 路径与url往返() {
        for p in ["/Users/me/a.txt", "/tmp/my file 你好.png", "/x/y~z-1._2"] {
            let url = path_to_file_url(Path::new(p));
            assert_eq!(
                file_url_to_path(&url),
                Some(PathBuf::from(p)),
                "round-trip 失败: {url}"
            );
        }
    }

    // 读写系统剪贴板，本机可跑（无需权限）。沙箱可能无 AppKit 环境，标 ignore。
    #[ignore = "需 macOS AppKit 运行环境；非沙箱本机实测"]
    #[test]
    fn 文本往返与changecount变化() {
        let mut c = MacClipboard::new();
        let before = c.change_count().unwrap();
        c.write(&ClipboardContent::Text("你好 MouseShare".into()))
            .unwrap();
        let after = c.change_count().unwrap();
        assert!(after > before, "写入后 changeCount 应增大");
        match c.read().unwrap() {
            Some(ClipboardContent::Text(s)) => assert_eq!(s, "你好 MouseShare"),
            other => panic!("应读回文本，得到 {other:?}"),
        }
    }
}
