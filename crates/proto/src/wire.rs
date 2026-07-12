//! 线格式编解码：`[u8 类型][u32 长度 LE][bincode 载荷]`（SPEC.md §3）。
//! 编号一经发布不得复用；废弃编号永久保留注释占位。

use crate::msg::*;

/// 帧头长度：1 字节类型 + 4 字节载荷长度（LE）。
pub const HEADER_LEN: usize = 5;
/// 载荷上限（SPEC §3）：防御畸形长度字段导致的内存放大。
pub const MAX_PAYLOAD: usize = 2 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    #[error("未知消息类型 0x{0:02X}")]
    UnknownType(u8),
    #[error("数据不完整：需要 {need} 字节，仅有 {got}")]
    Truncated { need: usize, got: usize },
    #[error("载荷超限：{0} 字节 > {MAX_PAYLOAD}")]
    TooLarge(usize),
    #[error("载荷尾部有 {0} 字节多余数据")]
    TrailingBytes(usize),
    #[error("编码失败: {0}")]
    Encode(#[from] bincode::error::EncodeError),
    #[error("解码失败: {0}")]
    Decode(#[from] bincode::error::DecodeError),
}

fn config() -> bincode::config::Configuration {
    bincode::config::standard()
}

/// 用宏锁定「编号 ↔ 消息」映射，保证 enum、编码、解码三处永不失配。
macro_rules! messages {
    ($( $code:literal => $variant:ident ),+ $(,)?) => {
        /// 全部协议消息（编号见 SPEC.md §3）。
        #[derive(Debug, Clone, PartialEq)]
        pub enum Msg {
            $( $variant($variant), )+
        }

        $(
            impl From<$variant> for Msg {
                fn from(m: $variant) -> Self { Msg::$variant(m) }
            }
        )+

        impl Msg {
            /// 线格式类型字节。
            pub fn msg_type(&self) -> u8 {
                match self { $( Msg::$variant(_) => $code, )+ }
            }

            fn encode_payload(&self) -> Result<Vec<u8>, ProtoError> {
                match self {
                    $( Msg::$variant(m) => Ok(bincode::serde::encode_to_vec(m, config())?), )+
                }
            }

            fn decode_payload(code: u8, bytes: &[u8]) -> Result<Msg, ProtoError> {
                match code {
                    $(
                        $code => {
                            let (m, used) =
                                bincode::serde::decode_from_slice::<$variant, _>(bytes, config())?;
                            if used != bytes.len() {
                                return Err(ProtoError::TrailingBytes(bytes.len() - used));
                            }
                            Ok(Msg::$variant(m))
                        }
                    )+
                    other => Err(ProtoError::UnknownType(other)),
                }
            }
        }
    };
}

messages! {
    0x01 => Hello,
    0x02 => PairRequest,
    0x03 => PairConfirm,
    0x04 => ScreenInfo,
    0x05 => LayoutUpdate,
    0x06 => EnterScreen,
    0x07 => LeaveScreen,
    0x08 => Ping,
    0x09 => Pong,
    0x0A => ReleaseAll,
    0x0B => Suspend,
    0x10 => KeyEvent,
    0x11 => MouseButton,
    0x12 => Scroll,
    0x18 => MouseMove,
    0x20 => ClipboardData,
    0x30 => FileOffer,
    0x31 => FileAccept,
    0x32 => FileChunk,
    0x33 => FileDone,
    0x34 => FileCancel,
}

/// 编码为完整帧。
pub fn encode(msg: &Msg) -> Result<Vec<u8>, ProtoError> {
    let payload = msg.encode_payload()?;
    if payload.len() > MAX_PAYLOAD {
        return Err(ProtoError::TooLarge(payload.len()));
    }
    let mut out = Vec::with_capacity(HEADER_LEN + payload.len());
    out.push(msg.msg_type());
    #[allow(clippy::cast_possible_truncation)] // 已被 MAX_PAYLOAD 上限约束
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

/// 从字节流头部解出一帧，返回 (消息, 消耗字节数)。
/// 数据不足返回 `Truncated`，调用方继续攒包后重试。
pub fn decode(buf: &[u8]) -> Result<(Msg, usize), ProtoError> {
    if buf.len() < HEADER_LEN {
        return Err(ProtoError::Truncated {
            need: HEADER_LEN,
            got: buf.len(),
        });
    }
    let code = buf[0];
    let len_bytes: [u8; 4] = buf[1..5].try_into().map_err(|_| ProtoError::Truncated {
        need: HEADER_LEN,
        got: buf.len(),
    })?;
    let len = u32::from_le_bytes(len_bytes) as usize;
    if len > MAX_PAYLOAD {
        return Err(ProtoError::TooLarge(len));
    }
    let total = HEADER_LEN + len;
    if buf.len() < total {
        return Err(ProtoError::Truncated {
            need: total,
            got: buf.len(),
        });
    }
    let msg = Msg::decode_payload(code, &buf[HEADER_LEN..total])?;
    Ok((msg, total))
}
