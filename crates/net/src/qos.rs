//! QoS / DSCP 标记的自定义 QUIC UDP socket（NFR-08；调研见 OPLOG 2026-06-29）。
//!
//! ## 为什么需要自己实现 socket
//!
//! 目标：给 MouseShare 的全部 QUIC 流量打上高优先级 DSCP 标记（默认 EF=46），
//! 让路由器 QoS / Wi-Fi WMM 把键鼠控制包**插队**到下载等大流量之前，
//! 缓解"下载软件 / 多设备抢网时鼠标卡顿"（NFR-01 延迟、NFR-08）。
//!
//! quinn 0.11 的默认 socket（`quinn_udp::UdpSocketState`）在**每次发包**时用
//! cmsg 把整个 IP_TOS 字节写成 ECN 值（无 ECN 时为 0），会**覆盖**我们在 socket
//! 上设的 DSCP（实测见 quinn-udp 0.5.14 `unix.rs:608`）。且 quinn 直到 main 分支
//! 的 `Transmit` 仍只有 `ecn` 字段、无任何 DSCP API。
//!
//! 因此这里实现一个最小的 [`quinn::AsyncUdpSocket`]：直接用 `tokio` 的 UDP socket
//! 收发**单个数据报**（不走 cmsg、不做 GSO/GRO/ECN），DSCP 在建 socket 时用
//! `socket2::set_tos` 设一次即长期生效。代价是失去 GSO/GRO 批量与 ECN 拥塞反馈——
//! 对本项目这点极小流量（鼠标移动几十字节、文件块 1MiB）可忽略。
//!
//! ## 平台差异
//!
//! - **macOS / Linux**：`set_tos` 即 `setsockopt(IP_TOS)`，直接生效。
//! - **Windows**：自 XP SP2 起默认忽略用 `setsockopt` 设的 IP_TOS（安全限制），
//!   本模块的 `set_tos` 在 Windows 上是**静默空操作**；Windows 的 DSCP 标记须经
//!   qWAVE（`ms-platform-windows`，T-708）在连接建立后把 socket 挂到 QoS flow。
//!   本模块对此暴露 [`DscpUdpSocket::raw_socket`] 供平台层取原始句柄。

use std::fmt;
use std::io::{self, IoSliceMut};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use quinn::udp::{RecvMeta, Transmit};
use quinn::{AsyncUdpSocket, UdpPoller};
use socket2::{Domain, Protocol, Socket, Type};
use tokio::io::ReadBuf;

/// 默认 DSCP 类别：EF（Expedited Forwarding，加急转发，值 46）。
///
/// EF 是 IETF 为低延迟实时流量定义的标准标记，被绝大多数路由器 QoS 识别；
/// 在 Wi-Fi WMM 上落到高优先级接入类别。可按需改（如 CS6=48 落到 WMM 语音类）。
pub const DSCP_EF: u8 = 46;

/// 把 6 位 DSCP 值放进 IP_TOS 字节的高 6 位（低 2 位留给 ECN）。
const fn dscp_to_tos(dscp: u8) -> u32 {
    ((dscp & 0x3f) as u32) << 2
}

/// 用指定 DSCP 标记建立并绑定一个非阻塞 UDP socket。
///
/// `set_tos` 仅对 IPv4 设置 IP_TOS；IPv6 的 traffic class 当前不设（项目默认绑
/// IPv4，见 daemon），且 Windows 上 `set_tos` 为空操作（DSCP 走 qWAVE / T-708）。
fn bind_dscp_socket(bind: SocketAddr, dscp: u8) -> io::Result<std::net::UdpSocket> {
    let domain = if bind.is_ipv4() {
        Domain::IPV4
    } else {
        Domain::IPV6
    };
    let sock = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
    sock.set_nonblocking(true)?;
    // 仅 IPv4 设 IP_TOS；非致命：标记失败不应阻断连接，仅告警。
    if bind.is_ipv4() {
        if let Err(e) = sock.set_tos(dscp_to_tos(dscp)) {
            tracing::warn!(error = %e, "设置 DSCP(IP_TOS) 失败，连接继续但无 QoS 标记");
        }
    }
    sock.bind(&bind.into())?;
    Ok(sock.into())
}

/// 实现 [`quinn::AsyncUdpSocket`] 的最小 DSCP 标记 UDP socket。
pub struct DscpUdpSocket {
    io: tokio::net::UdpSocket,
}

impl fmt::Debug for DscpUdpSocket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DscpUdpSocket")
            .field("local_addr", &self.io.local_addr().ok())
            .finish()
    }
}

impl DscpUdpSocket {
    /// 在 `bind` 上建立带 DSCP 标记的 socket，包装成 quinn 可用的 [`AsyncUdpSocket`]。
    pub fn bind(bind: SocketAddr, dscp: u8) -> io::Result<Arc<Self>> {
        let std = bind_dscp_socket(bind, dscp)?;
        let io = tokio::net::UdpSocket::from_std(std)?;
        Ok(Arc::new(Self { io }))
    }

    /// 底层 UDP socket 的原始句柄（Windows：供 `ms-platform-windows` 的 qWAVE 用，
    /// 见 T-708——Windows 上 DSCP 不能经 `setsockopt`，须把该句柄挂到 QoS flow）。
    #[cfg(windows)]
    pub fn raw_socket(&self) -> std::os::windows::io::RawSocket {
        use std::os::windows::io::AsRawSocket;
        self.io.as_raw_socket()
    }

    /// 底层 UDP socket 的原始 fd（unix：DSCP 已在建 socket 时经 `set_tos` 生效，
    /// 此句柄一般无需再用；提供以备诊断/对称）。
    #[cfg(unix)]
    pub fn raw_socket(&self) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd;
        self.io.as_raw_fd()
    }
}

/// 写就绪 poller：直接复用 tokio 的 `poll_send_ready`，无需自存 future。
struct DscpPoller(Arc<DscpUdpSocket>);

impl fmt::Debug for DscpPoller {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DscpPoller").finish_non_exhaustive()
    }
}

impl UdpPoller for DscpPoller {
    fn poll_writable(self: Pin<&mut Self>, cx: &mut Context) -> Poll<io::Result<()>> {
        self.0.io.poll_send_ready(cx)
    }
}

impl AsyncUdpSocket for DscpUdpSocket {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
        Box::pin(DscpPoller(self))
    }

    fn try_send(&self, transmit: &Transmit) -> io::Result<()> {
        // 我们声明 max_transmit_segments()==1，故 contents 恒为单个数据报，
        // 不会有 GSO 批；DSCP 已在 socket 级设好，普通 send_to 即带标记。
        self.io
            .try_send_to(transmit.contents, transmit.destination)
            .map(|_| ())
    }

    fn poll_recv(
        &self,
        cx: &mut Context,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        // 一次收一个数据报（无 GRO）。quinn 保证至少各有一个 buf/meta。
        let Some(buf) = bufs.first_mut() else {
            return Poll::Ready(Ok(0));
        };
        let Some(meta0) = meta.first_mut() else {
            return Poll::Ready(Ok(0));
        };
        let mut rb = ReadBuf::new(buf);
        match self.io.poll_recv_from(cx, &mut rb) {
            Poll::Ready(Ok(addr)) => {
                let len = rb.filled().len();
                *meta0 = RecvMeta {
                    addr,
                    len,
                    stride: len,
                    ecn: None,
                    dst_ip: None,
                };
                Poll::Ready(Ok(1))
            }
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Pending => Poll::Pending,
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.io.local_addr()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn dscp_ef_maps_to_tos_0xb8() {
        // EF=46 → TOS 高 6 位 = 46，整字节 = 46<<2 = 184 = 0xB8。
        assert_eq!(dscp_to_tos(DSCP_EF), 0xB8);
    }

    #[test]
    fn dscp_masks_to_6_bits() {
        // 高位脏数据被掩掉，只取低 6 位。
        assert_eq!(dscp_to_tos(0xff), 0x3f << 2);
        assert_eq!(dscp_to_tos(0), 0);
    }

    #[tokio::test]
    async fn binds_and_reports_local_addr() {
        let bind: SocketAddr = ([127, 0, 0, 1], 0).into();
        let sock = DscpUdpSocket::bind(bind, DSCP_EF).unwrap();
        let local = AsyncUdpSocket::local_addr(&*sock).unwrap();
        assert!(local.ip().is_loopback());
        assert_ne!(local.port(), 0);
    }
}
