//! 文件传输接入守护进程（T-504，FR-501~505）。
//!
//! 粘贴式（用户选定）：源端复制文件后切换控制权，读出剪贴板文件引用经独立 QUIC 流
//! 发往对端，落到接收目录（SPEC §4：Downloads/MouseShare，FR-501），并写回对端剪贴板
//! 供 Cmd+V/Ctrl+V 粘贴。传输实现见 `ms_net::transfer`；进度/取消/大文件确认经
//! [`crate::transfers`] 传输中心对接设置界面（FR-503/504）。

use std::path::PathBuf;

use ms_net::Conn;
use ms_net::quic::MsgRecvStream;
use ms_net::transfer::{self, TransferError};
use ms_proto::FileOffer;

use crate::transfers::{self, Dir, Phase};

/// 后台清理默认接收目录中超过 8 小时的残留传输临时文件。
pub fn spawn_stale_cleanup() {
    let Some(dir) = transfer::default_receive_dir() else {
        tracing::warn!(target: "mouseshare::clip", "无法定位接收目录，跳过残留传输文件清理");
        return;
    };
    tokio::spawn(async move {
        match transfer::cleanup_stale_parts(&dir).await {
            Ok(0) => {}
            Ok(removed) => tracing::info!(
                target: "mouseshare::clip",
                removed,
                dir = %dir.display(),
                "已清理超过 8 小时的残留传输文件"
            ),
            Err(e) => tracing::warn!(
                target: "mouseshare::clip",
                error = %e,
                dir = %dir.display(),
                "残留传输文件清理失败"
            ),
        }
    });
}

/// 起后台任务把若干文件发往对端（每文件一条独立流，与键鼠/剪贴板流隔离）。
/// 登记到传输中心：显示进度、支持取消；单文件 >1 GiB 先等用户确认（FR-504）。
pub fn spawn_send(conn: &Conn, paths: Vec<PathBuf>) {
    for path in paths {
        let conn = conn.clone();
        tokio::spawn(async move {
            let total = tokio::fs::metadata(&path)
                .await
                .map(|m| m.len())
                .unwrap_or(0);
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "file".to_string());
            let h = transfers::center().begin(name.clone(), total, Dir::Send);

            // 大文件（>1 GiB）发送前等用户在设置界面确认（FR-504）
            if total > transfers::LARGE_FILE_THRESHOLD {
                if !h.await_confirm(120).await {
                    h.set_phase(Phase::Cancelled);
                    tracing::info!(
                        target: "mouseshare::clip", file = %name, "大文件发送被取消/未确认"
                    );
                    return;
                }
                h.set_phase(Phase::Active);
            }

            let cancel = h.cancel_flag();
            match transfer::send_file(&conn, &path, cancel, |p| h.update(p.transferred)).await {
                Ok(()) => {
                    h.set_phase(Phase::Done);
                    tracing::info!(target: "mouseshare::clip", file = %name, "文件已发往对端");
                }
                Err(TransferError::Cancelled) => {
                    h.set_phase(Phase::Cancelled);
                    tracing::info!(target: "mouseshare::clip", file = %name, "文件发送已取消");
                }
                Err(e) => {
                    h.set_phase(Phase::Failed);
                    tracing::warn!(target: "mouseshare::clip", file = %name, %e, "文件发送失败");
                }
            }
        });
    }
}

/// 在分发器**已接受的流** + **已读出的 `FileOffer`** 上接收文件落地，返回落地路径
/// （供调用方写回剪贴板使 Cmd+V 可粘贴）。登记到传输中心：显示进度、支持取消。
pub async fn receive(stream: &mut MsgRecvStream, offer: FileOffer) -> Option<PathBuf> {
    let dir = transfer::default_receive_dir().or_else(|| {
        tracing::warn!(target: "mouseshare::clip", "无法定位接收目录，丢弃文件");
        None
    })?;
    if let Err(e) = transfer::cleanup_stale_parts(&dir).await {
        tracing::warn!(target: "mouseshare::clip", error = %e, "接收前清理残留传输文件失败");
    }
    let h = transfers::center().begin(offer.name.clone(), offer.size, Dir::Recv);
    let cancel = h.cancel_flag();
    match transfer::recv_on_stream(stream, &offer, &dir, cancel, |p| h.update(p.transferred)).await
    {
        Ok(path) => {
            h.set_phase(Phase::Done);
            tracing::info!(target: "mouseshare::clip", file = %path.display(), "已接收文件落地");
            Some(path)
        }
        Err(TransferError::Cancelled) => {
            h.set_phase(Phase::Cancelled);
            tracing::info!(target: "mouseshare::clip", "文件接收已取消");
            None
        }
        Err(e) => {
            h.set_phase(Phase::Failed);
            tracing::warn!(target: "mouseshare::clip", %e, "接收文件失败");
            None
        }
    }
}
