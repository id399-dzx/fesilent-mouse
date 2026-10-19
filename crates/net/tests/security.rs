//! T-703 安全验证（验收标准 6）：
//! 1. 全部流量为密文——经内置 UDP 中继抓取两端全部数据报，断言明文金丝雀不出现；
//! 2. 伪造设备被拒——持他人证书但无对应私钥（TLS 验签不过），握手必须失败。
//!
//! 「未配对设备连接被拒绝」的判定逻辑在应用层（ms-app daemon `decide_trust`），
//! 单测见 crates/app/src/daemon.rs；本文件覆盖传输层的两条性质。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};

use ms_net::{Endpoint, Identity};
use ms_proto::{ClipboardData, Msg};
use tokio::net::UdpSocket;

fn loopback() -> SocketAddr {
    (Ipv4Addr::LOCALHOST, 0).into()
}

/// 起一个记录型 UDP 中继：client ↔ relay ↔ server，抓下经过的每个数据报。
/// 返回 (中继地址, 抓包缓冲)。以第一个非 server 来源的包认定 client 地址。
async fn spawn_recording_relay(server: SocketAddr) -> (SocketAddr, Arc<Mutex<Vec<Vec<u8>>>>) {
    let sock = UdpSocket::bind(loopback()).await.unwrap();
    let addr = sock.local_addr().unwrap();
    let captured: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&captured);
    tokio::spawn(async move {
        let mut client: Option<SocketAddr> = None;
        let mut buf = vec![0u8; 65536];
        loop {
            let Ok((n, from)) = sock.recv_from(&mut buf).await else {
                return;
            };
            log.lock().unwrap().push(buf[..n].to_vec());
            let to = if from == server {
                match client {
                    Some(c) => c,
                    None => continue, // 尚不知道 client 地址
                }
            } else {
                client = Some(from);
                server
            };
            let _ = sock.send_to(&buf[..n], to).await;
        }
    });
    (addr, captured)
}

/// 明文金丝雀不得出现在任何抓到的数据报里（含跨包拼接，防边界切割漏检）。
fn assert_no_plaintext(captured: &[Vec<u8>], canary: &[u8], what: &str) {
    let all: Vec<u8> = captured.iter().flatten().copied().collect();
    assert!(
        !all.windows(canary.len()).any(|w| w == canary),
        "{what}：金丝雀明文出现在线上流量中（未加密！）"
    );
}

#[tokio::test]
async fn 抓包_全部流量为密文() {
    let server_id = Identity::generate().unwrap();
    let client_id = Identity::generate().unwrap();
    let server = Endpoint::new(loopback(), &server_id).unwrap();
    let server_addr = server.local_addr().unwrap();

    let (relay_addr, captured) = spawn_recording_relay(server_addr).await;

    let client = Endpoint::new(loopback(), &client_id).unwrap();
    let accept = tokio::spawn(async move { server.accept().await.unwrap().unwrap() });
    // client 经中继连 server：线上每个包都被抓下
    let client_conn = client.connect(relay_addr).await.unwrap();
    let server_conn = accept.await.unwrap();

    // 双向发含金丝雀的剪贴板消息（bincode 编码会原样含这些字节，若明文必被搜到）
    let canary_c2s = b"MOUSESHARE-PLAINTEXT-CANARY-C2S-7f3a".as_slice();
    let canary_s2c = b"MOUSESHARE-PLAINTEXT-CANARY-S2C-9e51".as_slice();
    let mk = |canary: &[u8]| {
        let chunk: Vec<u8> = canary.repeat(8); // 重复多次，跨包切割也搜得到
        Msg::from(ClipboardData {
            mime: "text/plain; utf-8".into(),
            total: chunk.len() as u32,
            chunk,
        })
    };
    client_conn.send_uni(&mk(canary_c2s)).await.unwrap();
    let got = server_conn.recv_uni().await.unwrap();
    assert!(matches!(got, Msg::ClipboardData(_)), "消息应经中继送达");
    server_conn.send_uni(&mk(canary_s2c)).await.unwrap();
    let got = client_conn.recv_uni().await.unwrap();
    assert!(matches!(got, Msg::ClipboardData(_)), "回程消息应经中继送达");

    let packets = captured.lock().unwrap().clone();
    assert!(
        packets.len() >= 4,
        "中继应抓到握手+数据的多个数据报（实际 {}）",
        packets.len()
    );
    assert_no_plaintext(&packets, canary_c2s, "client→server");
    assert_no_plaintext(&packets, canary_s2c, "server→client");
    // mime 字符串同样不得明文可见
    assert_no_plaintext(&packets, b"text/plain", "mime 字段");
}

#[tokio::test]
async fn 伪造设备_持他人证书无私钥_握手被拒() {
    // 攻击者拿到受信任设备 A 的证书（公开信息），但没有 A 的私钥，
    // 用自己的私钥 B 冒充：TLS CertificateVerify 验签必须失败，连接建立不起来。
    let victim = Identity::generate().unwrap(); // 受信任设备 A
    let attacker_key = Identity::generate().unwrap(); // 攻击者自己的密钥 B
    let forged = Identity::from_der(victim.cert_der().to_vec(), attacker_key.key_der().to_vec());

    let server_id = Identity::generate().unwrap();
    let server = Endpoint::new(loopback(), &server_id).unwrap();
    let server_addr = server.local_addr().unwrap();
    // server 侧 accept：伪造端要么握手失败要么根本到不了 accept 成功
    let accept = tokio::spawn(async move {
        tokio::time::timeout(std::time::Duration::from_secs(3), server.accept()).await
    });

    // Endpoint 构建可能直接拒绝密钥/证书不匹配；若能构建，握手也必须失败
    // 构建期即拒绝，或之后握手被拒/超时，都满足验证预期。
    if let Ok(ep) = Endpoint::new(loopback(), &forged) {
        let r =
            tokio::time::timeout(std::time::Duration::from_secs(3), ep.connect(server_addr)).await;
        if let Ok(Ok(_conn)) = r {
            panic!("伪造身份（他人证书+自己私钥）竟然握手成功");
        }
    }
    // server 侧不得得到一条成功的连接
    if let Ok(Some(Ok(_conn))) = accept.await.unwrap() {
        panic!("server 侧不应接受伪造身份的连接");
    }
}
