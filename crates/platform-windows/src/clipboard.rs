//! Windows 剪贴板读写（Win32 剪贴板 API；DEVELOPMENT.md §3.1，FR-401/405）。
//!
//! 仅在 Windows 编译。三类中立内容 ↔ 剪贴板格式：
//! 文本 `CF_UNICODETEXT`(UTF-16)、HTML `CF_HTML`(头部封装，见 clipboard_fmt)、
//! 图片 `CF_DIB`↔PNG（见 clipboard_fmt）。机密标记 `Clipboard Viewer Ignore`
//! 存在时跳过（FR-405）。变更检测用 `GetClipboardSequenceNumber`（FR-402）。

use std::path::PathBuf;

use ms_platform::{ClipboardAccess, ClipboardContent, PlatformError, Result};

use windows::Win32::Foundation::{HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber,
    IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
use windows::core::PCWSTR;

const CF_UNICODETEXT: u32 = 13;
const CF_DIB: u32 = 8;
const CF_HDROP: u32 = 15;

use crate::clipboard_fmt::{cf_html_strip, cf_html_wrap, dib_to_png, png_to_dib};

pub struct WinClipboard;

impl WinClipboard {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WinClipboard {
    fn default() -> Self {
        Self::new()
    }
}

/// 注册剪贴板格式名，返回格式 ID。
fn register_format(name: &str) -> u32 {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: wide 是以 NUL 结尾的有效 UTF-16 串
    unsafe { RegisterClipboardFormatW(PCWSTR(wide.as_ptr())) }
}

/// RAII：打开剪贴板，析构时关闭。
struct ClipboardGuard;

impl ClipboardGuard {
    fn open() -> Result<Self> {
        // SAFETY: 传 None 关联到当前任务；失败返回 Err
        unsafe { OpenClipboard(Some(HWND::default())) }
            .map_err(|e| PlatformError::Api(format!("OpenClipboard 失败: {e}")))?;
        Ok(Self)
    }
}

impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        // SAFETY: 与 OpenClipboard 配对
        let _ = unsafe { CloseClipboard() };
    }
}

/// 读取某格式的全部字节（GlobalLock→拷贝→GlobalUnlock）。
///
/// # Safety
/// 必须在剪贴板已打开期间调用。
unsafe fn read_format(format: u32) -> Option<Vec<u8>> {
    // SAFETY: 调用方保证剪贴板已打开
    let handle: HANDLE = unsafe { GetClipboardData(format) }.ok()?;
    if handle.is_invalid() {
        return None;
    }
    let hglobal = HGLOBAL(handle.0);
    // SAFETY: handle 来自剪贴板，按 HGLOBAL 锁定
    let ptr = unsafe { GlobalLock(hglobal) };
    if ptr.is_null() {
        return None;
    }
    // CF_UNICODETEXT/CF_DIB 等无显式长度；用 GlobalSize 取容量
    // SAFETY: hglobal 有效
    let size = unsafe { windows::Win32::System::Memory::GlobalSize(hglobal) };
    // SAFETY: ptr 指向 size 字节有效内存
    let bytes = unsafe { std::slice::from_raw_parts(ptr as *const u8, size) }.to_vec();
    // SAFETY: 与 GlobalLock 配对
    unsafe {
        let _ = GlobalUnlock(hglobal);
    }
    Some(bytes)
}

/// 把字节写入剪贴板某格式（分配 GMEM_MOVEABLE 全局内存并 SetClipboardData）。
///
/// # Safety
/// 必须在剪贴板已打开且已 EmptyClipboard 后调用。
unsafe fn write_format(format: u32, data: &[u8]) -> Result<()> {
    // SAFETY: 分配可移动全局内存
    let hglobal = unsafe { GlobalAlloc(GMEM_MOVEABLE, data.len()) }
        .map_err(|e| PlatformError::Api(format!("GlobalAlloc 失败: {e}")))?;
    // SAFETY: 新分配的句柄，锁定得到可写指针
    let ptr = unsafe { GlobalLock(hglobal) };
    if ptr.is_null() {
        return Err(PlatformError::Api("GlobalLock 失败".into()));
    }
    // SAFETY: ptr 指向 data.len() 字节
    unsafe {
        std::ptr::copy_nonoverlapping(data.as_ptr(), ptr as *mut u8, data.len());
        let _ = GlobalUnlock(hglobal);
    }
    // SetClipboardData 成功后内存所有权转移给系统
    // SAFETY: format 合法，hglobal 为有效全局内存
    unsafe { SetClipboardData(format, Some(HANDLE(hglobal.0))) }
        .map_err(|e| PlatformError::Api(format!("SetClipboardData 失败: {e}")))?;
    Ok(())
}

impl ClipboardAccess for WinClipboard {
    fn read(&mut self) -> Result<Option<ClipboardContent>> {
        let _guard = ClipboardGuard::open()?;
        let ignore_fmt = register_format("Clipboard Viewer Ignore");
        // SAFETY: 剪贴板已打开
        if unsafe { IsClipboardFormatAvailable(ignore_fmt) }.is_ok() {
            return Ok(None); // 机密内容跳过（FR-405）
        }
        let html_fmt = register_format("HTML Format");
        let png_fmt = register_format("PNG");

        // SAFETY: 剪贴板已打开期间读取
        unsafe {
            if let Some(bytes) = read_format(png_fmt) {
                return Ok(Some(ClipboardContent::Png(bytes)));
            }
            if let Some(bytes) = read_format(CF_DIB)
                && let Ok(png) = dib_to_png(&bytes)
            {
                return Ok(Some(ClipboardContent::Png(png)));
            }
            if let Some(bytes) = read_format(html_fmt)
                && let Some(frag) = cf_html_strip(&bytes)
            {
                return Ok(Some(ClipboardContent::Html(frag)));
            }
            if let Some(bytes) = read_format(CF_UNICODETEXT) {
                return Ok(Some(ClipboardContent::Text(utf16_bytes_to_string(&bytes))));
            }
        }
        Ok(None)
    }

    fn write(&mut self, content: &ClipboardContent) -> Result<()> {
        let _guard = ClipboardGuard::open()?;
        // SAFETY: 剪贴板已打开，清空后写入
        unsafe {
            EmptyClipboard()
                .map_err(|e| PlatformError::Api(format!("EmptyClipboard 失败: {e}")))?;
            match content {
                ClipboardContent::Text(s) => {
                    write_format(CF_UNICODETEXT, &string_to_utf16_bytes(s))?;
                }
                ClipboardContent::Html(s) => {
                    // 同时写 HTML 与纯文本：富文本控件用 CF_HTML，地址栏/搜索栏等只认
                    // CF_UNICODETEXT 的目标用退化文本（否则只写 HTML 时这些控件无内容可粘）。
                    let html_fmt = register_format("HTML Format");
                    write_format(html_fmt, &cf_html_wrap(s))?;
                    let plain = ms_platform::html_to_text(s);
                    write_format(CF_UNICODETEXT, &string_to_utf16_bytes(&plain))?;
                }
                ClipboardContent::Png(bytes) => {
                    // 同时放 CF_DIB（最大兼容）与注册的 PNG 格式
                    if let Ok(dib) = png_to_dib(bytes) {
                        write_format(CF_DIB, &dib)?;
                    }
                    let png_fmt = register_format("PNG");
                    write_format(png_fmt, bytes)?;
                }
            }
        }
        Ok(())
    }

    fn change_count(&mut self) -> Result<u64> {
        // SAFETY: 无副作用的查询
        let n = unsafe { GetClipboardSequenceNumber() };
        Ok(n as u64)
    }

    fn read_files(&mut self) -> Result<Vec<PathBuf>> {
        let _guard = ClipboardGuard::open()?;
        // SAFETY: 剪贴板已打开；CF_HDROP 不存在则返回 Err
        let handle = match unsafe { GetClipboardData(CF_HDROP) } {
            Ok(h) if !h.is_invalid() => h,
            _ => return Ok(Vec::new()),
        };
        let hdrop = HDROP(handle.0);
        // 文件数：ifile=0xFFFFFFFF 且缓冲为 None 时返回数量
        // SAFETY: hdrop 来自剪贴板 CF_HDROP，查询数量无写入
        let count = unsafe { DragQueryFileW(hdrop, 0xFFFF_FFFF, None) };
        let mut out = Vec::with_capacity(count as usize);
        for i in 0..count {
            // 先查第 i 个路径的 UTF-16 长度（不含 NUL）
            // SAFETY: 缓冲 None 时仅返回所需长度
            let len = unsafe { DragQueryFileW(hdrop, i, None) };
            if len == 0 {
                continue;
            }
            let mut buf = vec![0u16; len as usize + 1];
            // SAFETY: buf 容量 len+1，足够容纳路径与 NUL
            let n = unsafe { DragQueryFileW(hdrop, i, Some(&mut buf)) };
            if n > 0 {
                out.push(PathBuf::from(String::from_utf16_lossy(&buf[..n as usize])));
            }
        }
        Ok(out)
    }

    fn write_files(&mut self, paths: &[PathBuf]) -> Result<()> {
        if paths.is_empty() {
            return Ok(());
        }
        let buf = build_hdrop(paths);
        let _guard = ClipboardGuard::open()?;
        // SAFETY: 剪贴板已打开，清空后写入 CF_HDROP（DROPFILES + 宽字符路径双 NUL 结尾）
        unsafe {
            EmptyClipboard()
                .map_err(|e| PlatformError::Api(format!("EmptyClipboard 失败: {e}")))?;
            write_format(CF_HDROP, &buf)?;
        }
        Ok(())
    }
}

/// 构造 CF_HDROP 数据：`DROPFILES`(20 字节头) + 各路径 UTF-16+NUL，末尾再补一个 NUL。
fn build_hdrop(paths: &[PathBuf]) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    let mut buf = Vec::new();
    // DROPFILES { pFiles: u32=20(偏移到文件列表), pt:{0,0}, fNC:0, fWide:1 }
    buf.extend_from_slice(&20u32.to_le_bytes());
    buf.extend_from_slice(&0i32.to_le_bytes()); // pt.x
    buf.extend_from_slice(&0i32.to_le_bytes()); // pt.y
    buf.extend_from_slice(&0i32.to_le_bytes()); // fNC = FALSE
    buf.extend_from_slice(&1i32.to_le_bytes()); // fWide = TRUE（宽字符）
    for p in paths {
        for u in p.as_os_str().encode_wide() {
            buf.extend_from_slice(&u.to_le_bytes());
        }
        buf.extend_from_slice(&0u16.to_le_bytes()); // 每条路径 NUL 结尾
    }
    buf.extend_from_slice(&0u16.to_le_bytes()); // 列表末尾额外 NUL
    buf
}

fn string_to_utf16_bytes(s: &str) -> Vec<u8> {
    let mut units: Vec<u16> = s.encode_utf16().collect();
    units.push(0); // NUL 结尾
    let mut bytes = Vec::with_capacity(units.len() * 2);
    for u in units {
        bytes.extend_from_slice(&u.to_le_bytes());
    }
    bytes
}

fn utf16_bytes_to_string(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&u| u != 0) // 去掉 NUL 结尾及之后
        .collect();
    String::from_utf16_lossy(&units)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16往返中文() {
        let s = "你好 world 🌍";
        let bytes = string_to_utf16_bytes(s);
        assert_eq!(utf16_bytes_to_string(&bytes), s);
    }
}
