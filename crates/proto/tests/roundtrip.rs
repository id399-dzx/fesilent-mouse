//! T-101 验证：全部消息编解码 round-trip、编号锁定、错误路径。

#![allow(clippy::unwrap_used)]

use ms_proto::*;
use proptest::prelude::*;
use uuid::Uuid;

fn roundtrip(msg: Msg) -> Msg {
    let bytes = encode(&msg).unwrap();
    let (decoded, used) = decode(&bytes).unwrap();
    assert_eq!(used, bytes.len(), "单帧必须恰好消耗全部字节");
    decoded
}

/// 全部 21 种消息各一个样本：round-trip 等价 + 类型字节与 SPEC §3 一致。
#[test]
fn 全消息样本_roundtrip_且编号锁定() {
    let rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 2560.0,
        h: 1440.0,
    };
    let samples: Vec<(Msg, u8)> = vec![
        (
            Hello {
                proto_ver: PROTO_VER,
                device_id: Uuid::new_v4(),
                name: "Mac".into(),
                app_ver: "0.1.0".into(),
                kb_layout: "com.apple.keylayout.ABC".into(),
            }
            .into(),
            0x01,
        ),
        (
            PairRequest {
                cert_fingerprint: [7u8; 32],
            }
            .into(),
            0x02,
        ),
        (PairConfirm { sas_verified: true }.into(), 0x03),
        (
            ScreenInfo {
                virtual_rect: rect,
                scale: 2.0,
            }
            .into(),
            0x04,
        ),
        (
            LayoutUpdate {
                layout: Layout {
                    peer_position: Edge::Right,
                },
            }
            .into(),
            0x05,
        ),
        (
            EnterScreen {
                entry_edge: Edge::Left,
                ratio: 0.42,
            }
            .into(),
            0x06,
        ),
        (LeaveScreen {}.into(), 0x07),
        (Ping { seq: 1, t0: 123 }.into(), 0x08),
        (Pong { seq: 1, t0: 123 }.into(), 0x09),
        (ReleaseAll {}.into(), 0x0A),
        (Suspend {}.into(), 0x0B),
        (
            KeyEvent {
                hid: 0x04,
                down: true,
                shadow: (1.0, 2.0),
            }
            .into(),
            0x10,
        ),
        (
            MouseButton {
                btn: 0,
                down: false,
                shadow: (3.0, 4.0),
            }
            .into(),
            0x11,
        ),
        (
            Scroll {
                dx: -1.5,
                dy: 3.0,
                precise: true,
            }
            .into(),
            0x12,
        ),
        (
            MouseMove {
                seq: 99,
                x: 100.5,
                y: 200.25,
            }
            .into(),
            0x18,
        ),
        (
            ClipboardData {
                mime: "text/plain; utf-8".into(),
                total: 5,
                chunk: b"hello".to_vec(),
            }
            .into(),
            0x20,
        ),
        (
            FileOffer {
                id: Uuid::new_v4(),
                name: "测试 文件.txt".into(),
                size: 1 << 30,
            }
            .into(),
            0x30,
        ),
        (FileAccept { id: Uuid::new_v4() }.into(), 0x31),
        (
            FileChunk {
                id: Uuid::new_v4(),
                offset: 4096,
                data: vec![0xAB; 1024],
            }
            .into(),
            0x32,
        ),
        (FileDone { id: Uuid::new_v4() }.into(), 0x33),
        (FileCancel { id: Uuid::new_v4() }.into(), 0x34),
    ];
    assert_eq!(samples.len(), 21, "SPEC §3 共 21 种消息，缺一不可");
    for (msg, expect_code) in samples {
        assert_eq!(msg.msg_type(), expect_code, "{msg:?} 编号与 SPEC §3 不符");
        assert_eq!(roundtrip(msg.clone()), msg);
    }
}

#[test]
fn 数据不足返回truncated() {
    let bytes = encode(&Msg::from(Ping { seq: 1, t0: 2 })).unwrap();
    for cut in 0..bytes.len() {
        match decode(&bytes[..cut]) {
            Err(ProtoError::Truncated { .. }) => {}
            other => panic!("截断到 {cut} 字节应报 Truncated，得到 {other:?}"),
        }
    }
}

#[test]
fn 未知类型与超限载荷被拒() {
    // 未知类型 0xFF
    let mut bad = vec![0xFF];
    bad.extend_from_slice(&0u32.to_le_bytes());
    assert!(matches!(decode(&bad), Err(ProtoError::UnknownType(0xFF))));

    // 长度字段声称超过 MAX_PAYLOAD：必须立即拒绝而非等待攒包（防内存放大）
    let mut huge = vec![0x08];
    huge.extend_from_slice(&((MAX_PAYLOAD as u32) + 1).to_le_bytes());
    assert!(matches!(decode(&huge), Err(ProtoError::TooLarge(_))));
}

#[test]
fn 连续多帧解码() {
    let a = encode(&Msg::from(Ping { seq: 1, t0: 10 })).unwrap();
    let b = encode(&Msg::from(ReleaseAll {})).unwrap();
    let mut buf = a.clone();
    buf.extend_from_slice(&b);
    let (m1, n1) = decode(&buf).unwrap();
    let (m2, n2) = decode(&buf[n1..]).unwrap();
    assert_eq!(m1, Msg::from(Ping { seq: 1, t0: 10 }));
    assert_eq!(m2, Msg::from(ReleaseAll {}));
    assert_eq!(n1 + n2, buf.len());
}

// ── proptest 随机化：高频与变长消息 ──

proptest! {
    #[test]
    fn mousemove_roundtrip(seq: u32, x: f32, y: f32) {
        prop_assume!(x.is_finite() && y.is_finite());
        let msg = Msg::from(MouseMove { seq, x, y });
        prop_assert_eq!(roundtrip(msg.clone()), msg);
    }

    #[test]
    fn keyevent_roundtrip(hid: u16, down: bool, sx: f32, sy: f32) {
        prop_assume!(sx.is_finite() && sy.is_finite());
        let msg = Msg::from(KeyEvent { hid, down, shadow: (sx, sy) });
        prop_assert_eq!(roundtrip(msg.clone()), msg);
    }

    #[test]
    fn clipboard_roundtrip(mime in "[a-z/;= -]{1,40}", total: u32, chunk in proptest::collection::vec(any::<u8>(), 0..4096)) {
        let msg = Msg::from(ClipboardData { mime, total, chunk });
        prop_assert_eq!(roundtrip(msg.clone()), msg);
    }

    #[test]
    fn 任意字节流解码不崩溃(bytes in proptest::collection::vec(any::<u8>(), 0..256)) {
        let _ = decode(&bytes); // 只要求不 panic，错误均可
    }
}
