//! ms-net —— 传输层：QUIC(quinn) 连接、mDNS 发现、心跳/断连/重连状态机。
//! 网络常量以 SPEC.md §2 为准；断连原因码枚举见 SPEC.md §5（T-204 落地）。

pub mod cert;
pub mod discovery;
pub mod pairing;
pub mod qos;
pub mod quic;
pub mod reconnect;
pub mod session;
pub mod transfer;
pub mod verify;

pub use cert::{Identity, fingerprint_hex, fingerprint_of};
pub use discovery::{Discovery, PeerInfo, SERVICE_TYPE};
pub use pairing::{TrustStore, compute_sas};
pub use quic::{Conn, Endpoint, NetError};
pub use reconnect::{Backoff, ConnState, DisconnectReason, HeartbeatTracker};
