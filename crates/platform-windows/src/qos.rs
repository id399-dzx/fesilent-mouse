//! Windows qWAVE DSCP 标记（T-708，NFR-08；坑点见 TECH-REVIEW C-? / OPLOG 2026-06-29）。
//!
//! ## 为什么 Windows 要单独做
//!
//! Windows 自 XP SP2 起**默认忽略**用 `setsockopt(IP_TOS)` 设的 DSCP（安全限制），
//! 所以 `ms-net` 里 `socket2::set_tos` 在 Windows 上是空操作。要让发出的 QUIC 包带
//! DSCP 标记，必须用 qWAVE：`QOSCreateHandle` 拿句柄 → `QOSAddSocketToFlow` 把已绑定
//! 的 UDP socket 挂到一个 QoS flow。
//!
//! 用 `QOSTrafficTypeVoice`（语音流量类型）→ 系统自动给该 flow 打 **DSCP 46（EF）**，
//! 与 `ms-net` 在 macOS/Linux 用的默认值一致，且**不需要管理员权限**（显式
//! `QOSSetOutgoingDSCPValue` 才需要）。配 `QOS_NON_ADAPTIVE_FLOW` 关闭自适应限速——
//! 我们只要优先级标记，不要 qWAVE 替我们调速。
//!
//! ## 生命周期
//!
//! `QosFlow` 持有 qWAVE 句柄，`Drop` 时 `QOSCloseHandle` 释放该 flow。调用方
//! （daemon）应在连接存活期间持有它；连接断开/重连时丢弃旧的、对新连接重新挂 flow。
//!
//! ## 限制
//!
//! 当前仅处理 IPv4 目标（项目默认绑 IPv4，见 daemon）；IPv6 目标返回错误，调用方据此
//! 跳过标记、连接照常。**真机验证归 H-2/T-701**（AI 无法在本机跑 Windows qWAVE）。

#![cfg(target_os = "windows")]

use std::io;
use std::net::SocketAddr;
use std::os::windows::io::RawSocket;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::NetworkManagement::QoS::{
    QOS_NON_ADAPTIVE_FLOW, QOS_VERSION, QOSAddSocketToFlow, QOSCloseHandle, QOSCreateHandle,
    QOSTrafficTypeVoice,
};
use windows::Win32::Networking::WinSock::{
    AF_INET, IN_ADDR, IN_ADDR_0, SOCKADDR, SOCKADDR_IN, SOCKET,
};

/// 持有一个 qWAVE QoS flow；Drop 时关闭句柄释放 flow。
pub struct QosFlow {
    handle: HANDLE,
    flow_id: u32,
}

impl QosFlow {
    /// qWAVE 分配的 flow id（诊断用）。
    pub fn flow_id(&self) -> u32 {
        self.flow_id
    }
}

impl Drop for QosFlow {
    fn drop(&mut self) {
        // SAFETY: handle 由本模块 QOSCreateHandle 成功创建、未被关闭过，且 QosFlow 不可
        // 复制（无 Clone），故此处是唯一一次关闭，无重复释放风险。
        unsafe {
            let _ = QOSCloseHandle(self.handle);
        }
    }
}

/// 把已绑定的 UDP socket 挂到 qWAVE QoS flow（EF/语音优先级），让 Windows 给该 socket
/// 发往 `dest` 的数据报打上 DSCP 标记。
///
/// `raw_socket` 来自 `ms-net` 的 `Endpoint::raw_socket()`；`dest` 为对端地址
/// （`Conn::remote_addr()`）。仅支持 IPv4 目标。
pub fn add_socket_to_flow(raw_socket: RawSocket, dest: SocketAddr) -> io::Result<QosFlow> {
    let SocketAddr::V4(v4) = dest else {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "qWAVE DSCP 当前仅支持 IPv4 目标",
        ));
    };

    // 构造网络字节序的 SOCKADDR_IN。
    let sockaddr_in = SOCKADDR_IN {
        sin_family: AF_INET,
        sin_port: v4.port().to_be(),
        sin_addr: IN_ADDR {
            S_un: IN_ADDR_0 {
                S_addr: u32::from_ne_bytes(v4.ip().octets()),
            },
        },
        sin_zero: [0; 8],
    };

    let version = QOS_VERSION {
        MajorVersion: 1,
        MinorVersion: 0,
    };
    let mut handle = HANDLE::default();
    // SAFETY: version 指向栈上有效且已初始化的 QOS_VERSION；handle 为有效可写指针。
    // 失败返回 FALSE，不写出无效句柄。
    let created = unsafe { QOSCreateHandle(&version, &mut handle) };
    if !created.as_bool() {
        return Err(io::Error::last_os_error());
    }

    let socket = SOCKET(raw_socket as usize);
    let sockaddr_ptr = (&sockaddr_in as *const SOCKADDR_IN).cast::<SOCKADDR>();
    let mut flow_id: u32 = 0; // 0 = 新建 flow，返回时填入分配的 id
    // SAFETY: handle 为上面成功创建的有效句柄；socket 为 ms-net 持有的有效已绑定 UDP
    // socket，其生命周期长于本 flow（daemon 在连接期间同时持有 endpoint 与 QosFlow）；
    // sockaddr_ptr 指向本函数栈上存活到调用结束的 SOCKADDR_IN；flow_id 为有效可写指针。
    let added = unsafe {
        QOSAddSocketToFlow(
            handle,
            socket,
            Some(sockaddr_ptr),
            QOSTrafficTypeVoice,
            Some(QOS_NON_ADAPTIVE_FLOW),
            &mut flow_id,
        )
    };
    if !added.as_bool() {
        let err = io::Error::last_os_error();
        // SAFETY: 同上，handle 有效且尚未关闭；失败路径下关闭以免泄漏。
        unsafe {
            let _ = QOSCloseHandle(handle);
        }
        return Err(err);
    }

    Ok(QosFlow { handle, flow_id })
}
