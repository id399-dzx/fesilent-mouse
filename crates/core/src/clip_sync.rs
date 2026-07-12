//! 剪贴板同步编解码（FR-401~405，DEVELOPMENT.md §5.2）。
//!
//! 切换控制权时（DEVELOPMENT §5.2）源端读剪贴板，若有变化、未标记机密、未超限，
//! 则压缩（zstd）分块发送 `ClipboardData`；目标端重组解压写入。
//! 上限 10 MiB（压缩前，FR-403），分块 256 KiB（SPEC §2）。

use ms_platform::ClipboardContent;
use ms_proto::ClipboardData;

/// 同步上限（压缩前，FR-403）。
pub const MAX_UNCOMPRESSED: usize = 10 * 1024 * 1024;
/// 分块大小（SPEC §2）。
pub const CHUNK_SIZE: usize = 256 * 1024;

const MIME_TEXT: &str = "text/plain; utf-8";
const MIME_HTML: &str = "text/html";
const MIME_PNG: &str = "image/png";

#[derive(Debug, thiserror::Error)]
pub enum ClipError {
    #[error("内容超过 {MAX_UNCOMPRESSED} 字节上限")]
    TooLarge,
    #[error("压缩失败: {0}")]
    Compress(String),
    #[error("解压失败: {0}")]
    Decompress(String),
    #[error("未知 MIME: {0}")]
    UnknownMime(String),
    #[error("分块数据不一致")]
    Inconsistent,
}

fn content_to_bytes(c: &ClipboardContent) -> (&'static str, Vec<u8>) {
    match c {
        ClipboardContent::Text(s) => (MIME_TEXT, s.as_bytes().to_vec()),
        ClipboardContent::Html(s) => (MIME_HTML, s.as_bytes().to_vec()),
        ClipboardContent::Png(b) => (MIME_PNG, b.clone()),
    }
}

fn bytes_to_content(mime: &str, bytes: Vec<u8>) -> Result<ClipboardContent, ClipError> {
    match mime {
        MIME_TEXT => Ok(ClipboardContent::Text(
            String::from_utf8_lossy(&bytes).into_owned(),
        )),
        MIME_HTML => Ok(ClipboardContent::Html(
            String::from_utf8_lossy(&bytes).into_owned(),
        )),
        MIME_PNG => Ok(ClipboardContent::Png(bytes)),
        other => Err(ClipError::UnknownMime(other.to_string())),
    }
}

/// 把剪贴板内容编码为一串 `ClipboardData`（压缩 + 分块）。超限返回 Err。
pub fn encode(content: &ClipboardContent) -> Result<Vec<ClipboardData>, ClipError> {
    let (mime, raw) = content_to_bytes(content);
    if raw.len() > MAX_UNCOMPRESSED {
        return Err(ClipError::TooLarge);
    }
    let compressed =
        zstd::encode_all(raw.as_slice(), 3).map_err(|e| ClipError::Compress(e.to_string()))?;
    let total = compressed.len() as u32;
    let chunks: Vec<ClipboardData> = if compressed.is_empty() {
        vec![ClipboardData {
            mime: mime.to_string(),
            total,
            chunk: Vec::new(),
        }]
    } else {
        compressed
            .chunks(CHUNK_SIZE)
            .map(|c| ClipboardData {
                mime: mime.to_string(),
                total,
                chunk: c.to_vec(),
            })
            .collect()
    };
    Ok(chunks)
}

/// 重组器：按到达顺序累积分块，集齐后解压还原内容。
#[derive(Debug, Default)]
pub struct Reassembler {
    mime: Option<String>,
    total: u32,
    buf: Vec<u8>,
}

impl Reassembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// 推入一个分块；集齐（buf 长度达到 total）时返回还原的内容。
    pub fn push(&mut self, data: ClipboardData) -> Result<Option<ClipboardContent>, ClipError> {
        match &self.mime {
            None => {
                self.mime = Some(data.mime);
                self.total = data.total;
                self.buf.clear();
            }
            Some(m) => {
                if m != &data.mime || self.total != data.total {
                    return Err(ClipError::Inconsistent);
                }
            }
        }
        self.buf.extend_from_slice(&data.chunk);
        if self.buf.len() as u32 >= self.total {
            let raw = if self.total == 0 {
                Vec::new()
            } else {
                zstd::decode_all(self.buf.as_slice())
                    .map_err(|e| ClipError::Decompress(e.to_string()))?
            };
            let mime = self.mime.take().unwrap_or_default();
            let content = bytes_to_content(&mime, raw)?;
            *self = Self::new();
            Ok(Some(content))
        } else {
            Ok(None)
        }
    }
}

/// 切换时的剪贴板同步状态（开关 + 去重）。
#[derive(Debug)]
pub struct ClipSyncState {
    enabled: bool,
    last_change: Option<u64>,
}

impl ClipSyncState {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            last_change: None,
        }
    }

    pub fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
    }

    /// 是否开启。
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// 控制权切换时调用：给定当前剪贴板变更序号，判断是否需要同步。
    /// 返回 true 表示应读取并发送（开关开 + 序号有变化，FR-402/404）。
    pub fn should_sync(&mut self, change_count: u64) -> bool {
        if !self.enabled {
            return false;
        }
        if self.last_change == Some(change_count) {
            return false; // 自上次同步无变化
        }
        self.last_change = Some(change_count);
        true
    }

    /// 记录某变更序号已同步（不触发发送）。接收端写入对端内容后调用：把因写入而
    /// 抬升的本地序号标记为已同步，避免同一内容在两端间来回回弹（echo）。
    pub fn note_synced(&mut self, change_count: u64) {
        self.last_change = Some(change_count);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn roundtrip(content: ClipboardContent) -> ClipboardContent {
        let chunks = encode(&content).expect("encode");
        let mut r = Reassembler::new();
        let mut result = None;
        for c in chunks {
            if let Some(out) = r.push(c).expect("push") {
                result = Some(out);
            }
        }
        result.expect("应集齐还原")
    }

    #[test]
    fn 文本往返() {
        let c = ClipboardContent::Text("你好，剪贴板同步！".into());
        assert_eq!(roundtrip(c.clone()), c);
    }

    #[test]
    fn html往返() {
        let c = ClipboardContent::Html("<b>富文本</b>".into());
        assert_eq!(roundtrip(c.clone()), c);
    }

    #[test]
    fn png多分块往返() {
        // 造一个 >256KiB 的"图片"字节，强制多分块（用低熵数据保证压缩后仍 >1 块或 1 块均可）
        let big: Vec<u8> = (0..700_000u32).map(|i| (i % 251) as u8).collect();
        let c = ClipboardContent::Png(big);
        assert_eq!(roundtrip(c.clone()), c);
    }

    #[test]
    fn 超限拒绝() {
        let huge = ClipboardContent::Png(vec![0u8; MAX_UNCOMPRESSED + 1]);
        assert!(matches!(encode(&huge), Err(ClipError::TooLarge)));
    }

    #[test]
    fn 分块mime不一致报错() {
        let mut r = Reassembler::new();
        r.push(ClipboardData {
            mime: "text/plain; utf-8".into(),
            total: 100,
            chunk: vec![1, 2, 3],
        })
        .unwrap();
        let bad = r.push(ClipboardData {
            mime: "image/png".into(),
            total: 100,
            chunk: vec![4],
        });
        assert!(matches!(bad, Err(ClipError::Inconsistent)));
    }

    #[test]
    fn 开关与去重() {
        let mut s = ClipSyncState::new(true);
        assert!(s.should_sync(5), "首次有变化应同步");
        assert!(!s.should_sync(5), "同序号无变化不重复同步");
        assert!(s.should_sync(6), "序号变化应同步");
        s.set_enabled(false);
        assert!(!s.should_sync(7), "关闭后不同步");
    }

    #[test]
    fn 接收写入抑制回弹() {
        let mut s = ClipSyncState::new(true);
        // 接收端写入对端内容后，本地序号抬到 9；标记已同步
        s.note_synced(9);
        assert!(!s.should_sync(9), "刚写入的内容不应被当作本地新内容回弹");
        assert!(s.should_sync(10), "之后真正的本地变化仍应同步");
    }
}
