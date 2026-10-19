//! 文件传输（FR-501~505，SPEC §3 Stream 3+：每文件一流）。
//!
//! 一条 QUIC 单向流承载：FileOffer → FileChunk… → FileDone。取消 = 重置流。
//! 接收端先写独占临时文件，核验 FileDone 后原子、不覆盖地落地；取消/出错清理临时文件。

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
const TEMP_PREFIX: &str = ".fesilent-transfer-";

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
    #[error("暂不支持复制文件夹，请先压缩为单个文件")]
    Directory,
    #[error("发送期间源文件大小发生变化，请重新复制")]
    SourceChanged,
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
    if cancel.load(Ordering::Acquire) {
        return Err(TransferError::Cancelled);
    }
    // Windows 的 File::open 不允许目录句柄，先给出跨平台一致的可操作提示。
    let metadata = tokio::fs::metadata(path).await?;
    if metadata.is_dir() {
        return Err(TransferError::Directory);
    }
    if !metadata.is_file() {
        return Err(TransferError::Proto("只能发送普通文件".into()));
    }
    let mut file = tokio::fs::File::open(path).await?;
    let metadata = file.metadata().await?;
    if metadata.is_dir() {
        return Err(TransferError::Directory);
    }
    if !metadata.is_file() {
        return Err(TransferError::Proto("只能发送普通文件".into()));
    }
    let size = metadata.len();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let id = Uuid::new_v4();

    let mut stream = tokio::select! {
        biased;
        _ = wait_cancelled(&cancel) => return Err(TransferError::Cancelled),
        stream = conn.open_msg_stream() => stream?,
    };
    let result = tokio::select! {
        biased;
        _ = wait_cancelled(&cancel) => Err(TransferError::Cancelled),
        result = async {
            stream.send(&Msg::from(FileOffer { id, name, size })).await?;
            let mut buf = vec![0u8; FILE_CHUNK];
            let mut offset = 0u64;
            loop {
                let n = file.read(&mut buf).await?;
                if n == 0 {
                    break;
                }
                if n as u64 > size.saturating_sub(offset) {
                    return Err(TransferError::SourceChanged);
                }
                stream.send(&Msg::from(FileChunk { id, offset, data: buf[..n].to_vec() })).await?;
                offset += n as u64;
                on_progress(Progress { transferred: offset, total: size });
            }
            if offset != size {
                return Err(TransferError::SourceChanged);
            }
            stream.send(&Msg::from(FileDone { id })).await?;
            stream.finish().await?;
            Ok(())
        } => result,
    };
    if result.is_err() {
        // 读盘失败、源文件变化与取消均重置流，接收端不能把前缀误当完整文件。
        stream.reset();
    }
    result
}

/// 取消由 UI 原子量发布；等待网络时同样有界响应，不反复取消不安全的 recv。
async fn wait_cancelled(cancel: &AtomicBool) {
    while !cancel.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// 路径守卫覆盖普通错误与 future 被 abort/drop 两种清理路径。
struct TemporaryFile(PathBuf);

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.0)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(target: "mouseshare::net", path = %self.0.display(), %error, "清理传输临时文件失败");
        }
    }
}

fn create_temporary(dir: &Path) -> std::io::Result<(TemporaryFile, tokio::fs::File)> {
    loop {
        // a.txt/a.csv、同名并发与重复 offer id 均不共用暂存文件。
        let path = dir.join(format!("{TEMP_PREFIX}{}.ms-part", Uuid::new_v4()));
        // 创建与守卫绑定在同一个同步步骤，避免取消异步 open 后后台 IO 才创建出孤儿文件。
        // 这里仅创建目录项，批量数据读写仍使用 Tokio；不在输入热路径运行。
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => return Ok((TemporaryFile(path), tokio::fs::File::from_std(file))),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
}

/// 同目录硬链接是原子的，遇到已存在目标必失败，消除 exists→rename 的覆盖竞争。
/// 不支持该原语的文件系统报错，绝不回退到可能覆盖已有文件的 rename。
async fn publish_unique(tmp: &Path, dir: &Path, name: &str) -> std::io::Result<PathBuf> {
    for index in 0..10_000 {
        let candidate = numbered_path(dir, name, index);
        match tokio::fs::hard_link(tmp, &candidate).await {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "同名文件过多，无法分配接收文件名",
    ))
}

/// 接收一个文件到 `dest_dir`，返回落地路径。出错/取消时删除临时文件。
/// 自行接受新流并读出首条 `FileOffer`（独立调用场景，如回环测试）。
pub async fn recv_file(
    conn: &Conn,
    dest_dir: &Path,
    cancel: Arc<AtomicBool>,
    on_progress: impl FnMut(Progress),
) -> std::result::Result<PathBuf, TransferError> {
    let mut stream = tokio::select! {
        biased;
        _ = wait_cancelled(&cancel) => return Err(TransferError::Cancelled),
        stream = conn.accept_msg_stream() => stream?,
    };
    // 首条必须是 FileOffer
    let first = tokio::select! {
        biased;
        _ = wait_cancelled(&cancel) => return Err(TransferError::Cancelled),
        first = stream.recv() => first?,
    };
    let offer = match first {
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
    if cancel.load(Ordering::Acquire) {
        return Err(TransferError::Cancelled);
    }
    tokio::fs::create_dir_all(dest_dir).await?;
    let (temporary, mut file) = create_temporary(dest_dir)?;
    // 取消选中后整个接收 future 被丢弃，不能再复用已读过部分帧的 stream。
    let result = tokio::select! {
        biased;
        _ = wait_cancelled(&cancel) => Err(TransferError::Cancelled),
        result = recv_into(stream, &mut file, offer, &mut on_progress) => result,
    };
    drop(file); // Windows 上同样先关闭文件，再落地/清理。
    result?;
    if cancel.load(Ordering::Acquire) {
        return Err(TransferError::Cancelled);
    }
    Ok(publish_unique(&temporary.0, dest_dir, &sanitize_name(&offer.name)).await?)
}

async fn recv_into(
    stream: &mut MsgRecvStream,
    file: &mut tokio::fs::File,
    offer: &FileOffer,
    on_progress: &mut impl FnMut(Progress),
) -> std::result::Result<(), TransferError> {
    let mut received = 0u64;
    loop {
        match stream.recv().await? {
            Some(Msg::FileChunk(c)) => {
                if c.id != offer.id || c.offset != received {
                    return Err(TransferError::Proto("文件分块标识或偏移不匹配".into()));
                }
                if c.data.is_empty() || c.data.len() as u64 > offer.size.saturating_sub(received) {
                    return Err(TransferError::Proto("文件分块超出声明大小或为空".into()));
                }
                file.write_all(&c.data).await?;
                received += c.data.len() as u64;
                on_progress(Progress {
                    transferred: received,
                    total: offer.size,
                });
            }
            Some(Msg::FileDone(done)) => {
                if done.id != offer.id || received != offer.size {
                    return Err(TransferError::Proto("文件结束标识或实际大小不匹配".into()));
                }
                break;
            }
            Some(Msg::FileCancel(cancel)) if cancel.id == offer.id => {
                return Err(TransferError::Cancelled);
            }
            None => return Err(TransferError::Proto("文件流未完成即结束".into())),
            Some(_) => return Err(TransferError::Proto("文件流包含不匹配的消息".into())),
        }
    }
    // FileDone 不是 QUIC FIN：必须消费 EOF 才能正常释放 RecvStream，否则 Drop 会
    // STOP_SENDING(0)，把正常发送误报为取消；同时拒绝 FileDone 后的任何额外帧。
    if stream.recv().await?.is_some() {
        return Err(TransferError::Proto("文件结束后仍包含额外消息".into()));
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
        return "file".to_string();
    }
    if trimmed.starts_with(TEMP_PREFIX) {
        // 保留内部暂存命名空间，收到同名真实文件时也不会被自动清理误删。
        return format!("_{trimmed}");
    }
    // Windows 设备名即使带扩展名仍保留，普通字符替换无法让 CON.txt 等合法。
    let stem = trimmed
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(|suffix| suffix.len() == 1 && matches!(suffix.as_bytes()[0], b'1'..=b'9'))
    {
        format!("_{trimmed}")
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

/// 删除 `dir` 中最后修改时间超过 8 小时、带内部前缀及 UUID 的残留暂存文件。
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
        let is_temporary = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix(TEMP_PREFIX))
            .and_then(|name| name.strip_suffix(".ms-part"))
            .is_some_and(|id| Uuid::parse_str(id).is_ok());
        if !is_temporary {
            continue;
        }
        let metadata = match tokio::fs::symlink_metadata(&path).await {
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
    (0..10_000)
        .map(|index| numbered_path(dir, name, index))
        .find(|path| !path.exists())
        // 只供文件名预览，真正落地必须使用 publish_unique 原子创建。
        .unwrap_or_else(|| dir.join(format!("{}-{name}", Uuid::new_v4())))
}

fn numbered_path(dir: &Path, name: &str, index: u32) -> PathBuf {
    if index == 0 {
        return dir.join(name);
    }
    let path = Path::new(name);
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or(name);
    let ext = path.extension().and_then(|s| s.to_str());
    dir.join(match ext {
        Some(ext) => format!("{stem} ({index}).{ext}"),
        None => format!("{stem} ({index})"),
    })
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
        assert_eq!(sanitize_name("CON.txt"), "_CON.txt");
        assert_eq!(sanitize_name("com1"), "_com1");
        assert_eq!(sanitize_name("Lpt9.log"), "_Lpt9.log");
        assert_eq!(sanitize_name("com10.txt"), "com10.txt");
        assert_eq!(
            sanitize_name(".fesilent-transfer-user.ms-part"),
            "_.fesilent-transfer-user.ms-part"
        );
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
        let stale = dir.join(format!("{TEMP_PREFIX}{}.ms-part", Uuid::new_v4()));
        let fresh = dir.join(format!("{TEMP_PREFIX}{}.ms-part", Uuid::new_v4()));
        let normal = dir.join("用户文件.ms-part");
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
        File::options()
            .write(true)
            .open(&normal)
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
