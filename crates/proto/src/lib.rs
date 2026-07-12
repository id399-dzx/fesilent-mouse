//! ms-proto —— 协议层：消息定义、编解码、版本协商。
//! 消息类型编号与通道划分以 SPEC.md §3 为唯一依据。

pub mod msg;
pub mod wire;

pub use msg::*;
pub use wire::{HEADER_LEN, MAX_PAYLOAD, Msg, ProtoError, decode, encode};

/// 协议版本（SPEC.md §2：不兼容变更 +1）。
pub const PROTO_VER: u16 = 1;
