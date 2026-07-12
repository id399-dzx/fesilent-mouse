//! T-201 验证：回环两 endpoint 建立 mTLS QUIC 连接，全类型消息 round-trip，
//! 指纹互认。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::{Ipv4Addr, SocketAddr};

use ms_net::{Endpoint, Identity};
use ms_proto::{Edge, PROTO_VER};
use ms_proto::{EnterScreen, Hello, MouseMove, Msg, Ping, ReleaseAll};
use uuid::Uuid;

fn loopback() -> SocketAddr {
    (Ipv4Addr::LOCALHOST, 0).into()
}

/// 建立一对已连接的 (client_conn, server_conn)。
async fn connect_pair() -> (ms_net::Conn, ms_net::Conn, [u8; 32], [u8; 32]) {
    let server_id = Identity::generate().unwrap();
    let client_id = Identity::generate().unwrap();
    let server_fp = server_id.fingerprint();
    let client_fp = client_id.fingerprint();

    let server = Endpoint::new(loopback(), &server_id).unwrap();
    let server_addr = server.local_addr().unwrap();
    let client = Endpoint::new(loopback(), &client_id).unwrap();

    let accept = tokio::spawn(async move { server.accept().await.unwrap().unwrap() });
    let client_conn = client.connect(server_addr).await.unwrap();
    let server_conn = accept.await.unwrap();

    // 两端 endpoint 需在连接存活期间保留；这里把 client endpoint 泄漏给连接生命周期
    std::mem::forget(client);
    (client_conn, server_conn, server_fp, client_fp)
}

#[tokio::test]
async fn sas_双端算出同一配对码() {
    let (client_conn, server_conn, server_fp, client_fp) = connect_pair().await;
    // 客户端 own=client_fp，服务端 own=server_fp，各自计算应得同码（DEVELOPMENT §4.5）
    let sas_client = client_conn.compute_sas(&client_fp).expect("客户端 SAS");
    let sas_server = server_conn.compute_sas(&server_fp).expect("服务端 SAS");
    assert_eq!(sas_client, sas_server, "双端 SAS 必须一致才能人工核对配对");
    assert_eq!(sas_client.len(), 6);
}

#[tokio::test]
async fn pinning_未配对指纹被拒() {
    use ms_net::TrustStore;
    let (client_conn, _server_conn, server_fp, _) = connect_pair().await;
    let mut trust = TrustStore::new();
    // 未配对：对端指纹不在信任列表 → 拒绝
    let peer = client_conn.peer_fingerprint().unwrap();
    assert!(!trust.is_trusted(&peer), "未配对应拒连");
    // 配对后放行
    trust.trust(&server_fp);
    assert!(trust.is_trusted(&peer), "配对后应放行（peer 即 server_fp）");
}

#[tokio::test]
async fn 握手成功且指纹互认() {
    let (client_conn, server_conn, server_fp, client_fp) = connect_pair().await;
    assert_eq!(
        client_conn.peer_fingerprint(),
        Some(server_fp),
        "客户端看到的对端指纹应为服务端证书指纹"
    );
    assert_eq!(
        server_conn.peer_fingerprint(),
        Some(client_fp),
        "服务端看到的对端指纹应为客户端证书指纹（mTLS）"
    );
}

#[tokio::test]
async fn 单向流消息双向round_trip() {
    let (client_conn, server_conn, _, _) = connect_pair().await;

    let samples = vec![
        Msg::from(Hello {
            proto_ver: PROTO_VER,
            device_id: Uuid::new_v4(),
            name: "Tester".into(),
            app_ver: "0.1.0".into(),
            kb_layout: "com.apple.keylayout.ABC".into(),
        }),
        Msg::from(EnterScreen {
            entry_edge: Edge::Left,
            ratio: 0.42,
        }),
        Msg::from(Ping { seq: 7, t0: 123 }),
        Msg::from(ReleaseAll {}),
    ];

    // client → server
    for msg in &samples {
        client_conn.send_uni(msg).await.unwrap();
        let got = server_conn.recv_uni().await.unwrap();
        assert_eq!(&got, msg, "client→server round-trip 失配");
    }
    // server → client
    for msg in &samples {
        server_conn.send_uni(msg).await.unwrap();
        let got = client_conn.recv_uni().await.unwrap();
        assert_eq!(&got, msg, "server→client round-trip 失配");
    }
}

#[tokio::test]
async fn datagram_鼠标移动round_trip() {
    let (client_conn, server_conn, _, _) = connect_pair().await;
    let msg = Msg::from(MouseMove {
        seq: 99,
        x: 100.5,
        y: 200.25,
    });
    // datagram 可能因 MTU/时序丢失，这里在本地回环重试几次确保至少一次成功
    let mut received = None;
    for _ in 0..5 {
        client_conn.send_datagram(&msg).unwrap();
        if let Ok(got) = tokio::time::timeout(
            std::time::Duration::from_millis(500),
            server_conn.recv_datagram(),
        )
        .await
        {
            received = Some(got.unwrap());
            break;
        }
    }
    assert_eq!(received, Some(msg), "datagram 鼠标移动 round-trip 失配");
}
