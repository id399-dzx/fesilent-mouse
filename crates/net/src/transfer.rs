//! 文件传输（FR-501~505，SPEC §3 Stream 3+：每文件一流）。
//!
//! 一条 QUIC 单向流承载：FileOffer → FileChunk… → FileDone。取消 = 重置流。
//! 接收端先写临时文件，FileDone 后原子改名落地；取消/出错则删除临时文件（无残留）。

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use ms_proto::{FileChunk, FileDone, FileOffer, Msg};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

use crate::quic::MsgRecvStream;
use crate::{Conn, NetError};

/// 传输分块大小（SPEC §2：1 MiB）。
pub const FILE_CHUNK: usize = 1024 * 1024;

/// 残留传输临时文件的最长保留时间。
pub const STALE_PART_MAX_AGE: Duration = Duration::from_secs(8 * 60 * 60);

/// 进度回报。
#[derive(Debug, Clone, Copy)]
pub struct Progress {
    pub transferred: u64,
    pub total: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum TransferError {
    #[error("传输被取消")]
    Cancelled,
    #[error("网络错误: {0}")]
    Net(#[from] NetError),
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("协议错误: {0}")]
    Proto(String),
}

/// 发送一个文件。`cancel` 置位则尽快中止并重置流。
pub async fn send_file(
    conn: &Conn,
    path: &Path,
    cancel: Arc<AtomicBool>,
    mut on_progress: impl FnMut(Progress),
) -> std::result::Result<(), TransferError> {
    let mut file = tokio::fs::File::open(path).await?;
    let size = file.metadata().await?.len();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let id = Uuid::new_v4();

    let mut stream = conn.open_msg_stream().await?;
    stream
        .send(&Msg::from(FileOffer { id, name, size }))
        .await?;

    let mut buf = vec![0u8; FILE_CHUNK];
    let mut offset = 0u64;
    loop {
        if cancel.load(Ordering::Acquire) {
            stream.reset();
            return Err(TransferError::Cancelled);
        }
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        stream
            .send(&Msg::from(FileChunk {
                id,
                offset,
                data: buf[..n].to_vec(),
            }))
            .await?;
        offset += n as u64;
        on_progress(Progress {
            transferred: offset,
            total: size,
        });
    }
    stream.send(&Msg::from(FileDone { id })).await?;
    stream.finish().await?;
    Ok(())
}

/// 接收一个文件到 `dest_dir`，返回落地路径。出错/取消时删除临时文件。
/// 自行接受新流并读出首条 `FileOffer`（独立调用场景，如回环测试）。
pub async fn recv_file(
    conn: &Conn,
    dest_dir: &Path,
    cancel: Arc<AtomicBool>,
    on_progress: impl FnMut(Progress),
) -> std::result::Result<PathBuf, TransferError> {
    let mut stream = conn.accept_msg_stream().await?;
    // 首条必须是 FileOffer
    let offer = match stream.recv().await? {
        Some(Msg::FileOffer(o)) => o,
        other => {
            return Err(TransferError::Proto(format!(
                "期待 FileOffer，得到 {other:?}"
            )));
        }
    };
    recv_on_stream(&mut stream, &offer, dest_dir, cancel, on_progress).await
}

/// 在**已接受的流** + **已读出的 `FileOffer`** 上接收文件落地，返回落地路径。
/// 供守护进程的流分发器复用：分发器接受流、读首条判类型后把文件流交给本函数，
/// 避免重复 accept（一条流只能被接受一次）。出错/取消时删除临时文件（无残留）。
pub async fn recv_on_stream(
    stream: &mut MsgRecvStream,
    offer: &FileOffer,
    dest_dir: &Path,
    cancel: Arc<AtomicBool>,
    mut on_progress: impl FnMut(Progress),
) -> std::result::Result<PathBuf, TransferError> {
    tokio::fs::create_dir_all(dest_dir).await?;
    let final_path = unique_path(dest_dir, &sanitize_name(&offer.name));
    let tmp_path = final_path.with_extension("ms-part");

    // 任何失败路径都清理临时文件
    let result = recv_into(stream, &tmp_path, offer, &cancel, &mut on_progress).await;
    match result {
        Ok(()) => {
            tokio::fs::rename(&tmp_path, &final_path).await?;
            Ok(final_path)
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&tmp_path).await; // 无残留
            Err(e)
        }
    }
}

async fn recv_into(
    stream: &mut MsgRecvStream,
    tmp_path: &Path,
    offer: &FileOffer,
    cancel: &Arc<AtomicBool>,
    on_progress: &mut impl FnMut(Progress),
) -> std::result::Result<(), TransferError> {
    let mut file = tokio::fs::File::create(tmp_path).await?;
    let mut received = 0u64;
    loop {
        if cancel.load(Ordering::Acquire) {
            return Err(TransferError::Cancelled);
        }
        match stream.recv().await? {
            Some(Msg::FileChunk(c)) => {
                file.write_all(&c.data).await?;
                received += c.data.len() as u64;
                on_progress(Progress {
                    transferred: received,
                    total: offer.size,
                });
            }
            Some(Msg::FileDone(_)) => break,
            Some(Msg::FileCancel(_)) | None => return Err(TransferError::Cancelled),
            Some(other) => {
                return Err(TransferError::Proto(format!("传输中意外消息 {other:?}")));
            }
        }
    }
    file.flush().await?;
    Ok(())
}

/// 文件名跨平台合法化（FR-505）：替换非法字符，去掉路径分隔。
pub fn sanitize_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => '_',
            c if (c as u32) < 0x20 => '_',
            c => c,
        })
        .collect();
    // 仅去尾部点/空格（Windows 禁止），保留点开头的合法文件名
    let trimmed = cleaned.trim_end_matches([' ', '.']).to_string();
    if trimmed.is_empty() {
        "file".to_string()
    } else {
        trimmed
    }
}

/// 默认接收目录（SPEC §4：Downloads/FesilentMouse）。
pub fn default_receive_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from);
    #[cfg(not(target_os = "windows"))]
    let home = std::env::var_os("HOME").map(PathBuf::from);
    home.map(|h| h.join("Downloads").join("FesilentMouse"))
}

/// 删除 `dir` 中最后修改时间超过 8 小时的 `.ms-part` 残留传输文件。
///
/// 仅检查目录直属文件；无法读取元数据或单个文件删除失败时跳过并记录告警，
/// 避免清理任务影响正常文件传输。目录尚不存在视为无需清理。
pub async fn cleanup_stale_parts(dir: &Path) -> std::io::Result<usize> {
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(e) => return Err(e),
    };
    let now = SystemTime::now();
    let mut removed = 0;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.extension() != Some(OsStr::new("ms-part")) {
            continue;
        }
        let metadata = match entry.metadata().await {
            Ok(metadata) if metadata.is_file() => metadata,
            Ok(_) => continue,
            Err(e) => {
                tracing::warn!(file = %path.display(), error = %e, "读取残留传输文件元数据失败，跳过清理");
                continue;
            }
        };
        let is_stale = metadata
            .modified()
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > STALE_PART_MAX_AGE);
        if !is_stale {
            continue;
        }
        match tokio::fs::remove_file(&path).await {
            Ok(()) => removed += 1,
            Err(e) => {
                tracing::warn!(file = %path.display(), error = %e, "删除残留传输文件失败");
            }
        }
    }
    Ok(removed)
}

/// 在 `dir` 下为 `name` 选一个不冲突的路径：已存在则插入 " (1)"、" (2)"…（不覆盖）。
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let path = Path::new(name);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or(name);
    let ext = path.extension().and_then(|s| s.to_str());
    for i in 1..10_000 {
        let candidate_name = match ext {
            Some(e) => format!("{stem} ({i}).{e}"),
            None => format!("{stem} ({i})"),
        };
        let p = dir.join(candidate_name);
        if !p.exists() {
            return p;
        }
    }
    dir.join(name) // 极端情况兜底
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn 文件名合法化矩阵() {
        assert_eq!(sanitize_name("a/b:c*.txt"), "a_b_c_.txt");
        assert_eq!(sanitize_name("  ..  "), "file");
        assert_eq!(sanitize_name("正常 文件.png"), "正常 文件.png");
        assert_eq!(sanitize_name("../../etc/passwd"), ".._.._etc_passwd");
        // 全套 Windows 非法字符
        assert_eq!(
            sanitize_name(r#"a<b>c:d"e|f?g*h\i/j"#),
            "a_b_c_d_e_f_g_h_i_j"
        );
        // 控制字符
        assert_eq!(sanitize_name("x\u{1}y\u{7}z"), "x_y_z");
        // 空与纯非法 → 兜底
        assert_eq!(sanitize_name(""), "file");
        assert_eq!(sanitize_name("///"), "___");
        // 点开头保留（dotfile）
        assert_eq!(sanitize_name(".gitignore"), ".gitignore");
        // emoji 等保留
        assert_eq!(sanitize_name("照片 🌅.jpg"), "照片 🌅.jpg");
    }

    #[test]
    fn 冲突避让不覆盖() {
        let dir = std::env::temp_dir().join(format!("ms-unique-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        // 无冲突
        assert_eq!(unique_path(&dir, "a.txt"), dir.join("a.txt"));
        // 制造冲突
        std::fs::write(dir.join("a.txt"), b"x").unwrap();
        assert_eq!(unique_path(&dir, "a.txt"), dir.join("a (1).txt"));
        std::fs::write(dir.join("a (1).txt"), b"x").unwrap();
        assert_eq!(unique_path(&dir, "a.txt"), dir.join("a (2).txt"));
        // 无扩展名
        std::fs::write(dir.join("README"), b"x").unwrap();
        assert_eq!(unique_path(&dir, "README"), dir.join("README (1)"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn 只清理超过八小时的残留传输文件() {
        use std::fs::{File, FileTimes};

        let dir = std::env::temp_dir().join(format!("ms-cleanup-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let stale = dir.join("stale.ms-part");
        let fresh = dir.join("fresh.ms-part");
        let normal = dir.join("keep.txt");
        std::fs::write(&stale, b"old").unwrap();
        std::fs::write(&fresh, b"new").unwrap();
        std::fs::write(&normal, b"keep").unwrap();
        let old = SystemTime::now() - STALE_PART_MAX_AGE - Duration::from_secs(1);
        File::options()
            .write(true)
            .open(&stale)
            .unwrap()
            .set_times(FileTimes::new().set_modified(old))
            .unwrap();

        assert_eq!(cleanup_stale_parts(&dir).await.unwrap(), 1);
        assert!(!stale.exists());
        assert!(fresh.exists());
        assert!(normal.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn 接收目录不存在时清理成功且删除数为零() {
        let dir = std::env::temp_dir().join(format!("ms-cleanup-missing-{}", Uuid::new_v4()));
        assert_eq!(cleanup_stale_parts(&dir).await.unwrap(), 0);
    }
}
