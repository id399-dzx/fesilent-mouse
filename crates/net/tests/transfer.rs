//! T-502 验证：回环文件传输 sha256 一致；中途取消无残留（FR-501~505）。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ms_net::transfer::{recv_file, send_file};
use ms_net::{Endpoint, Identity};
use sha2::{Digest, Sha256};

fn loopback() -> SocketAddr {
    (Ipv4Addr::LOCALHOST, 0).into()
}

async fn connect_pair() -> (ms_net::Conn, ms_net::Conn) {
    let sid = Identity::generate().unwrap();
    let cid = Identity::generate().unwrap();
    let server = Endpoint::new(loopback(), &sid).unwrap();
    let addr = server.local_addr().unwrap();
    let client = Endpoint::new(loopback(), &cid).unwrap();
    let accept = tokio::spawn(async move { server.accept().await.unwrap().unwrap() });
    let cc = client.connect(addr).await.unwrap();
    let sc = accept.await.unwrap();
    std::mem::forget(client);
    (cc, sc)
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().into()
}

#[tokio::test]
async fn 回环文件传输sha256一致() {
    let (cc, sc) = connect_pair().await;
    let dir = std::env::temp_dir().join(format!("ms-xfer-{}", uuid::Uuid::new_v4()));

    // 造一个 ~8MiB 伪随机文件（多分块）
    let src_path = dir.join("源.bin");
    tokio::fs::create_dir_all(&dir).await.unwrap();
    let data: Vec<u8> = (0..8_000_000u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 13) as u8)
        .collect();
    tokio::fs::write(&src_path, &data).await.unwrap();
    let src_hash = sha256(&data);

    let dest_dir = dir.join("接收");
    let cancel = Arc::new(AtomicBool::new(false));
    let c2 = Arc::clone(&cancel);

    let recv = tokio::spawn(async move { recv_file(&sc, &dest_dir, c2, |_| {}).await });
    let sent_bytes = Arc::new(AtomicU64::new(0));
    let sb = Arc::clone(&sent_bytes);
    send_file(&cc, &src_path, cancel, move |p| {
        sb.store(p.transferred, Ordering::Relaxed);
    })
    .await
    .unwrap();

    let recv_path = recv.await.unwrap().unwrap();
    let got = tokio::fs::read(&recv_path).await.unwrap();
    assert_eq!(sha256(&got), src_hash, "收发文件 sha256 必须一致");
    assert_eq!(recv_path.file_name().unwrap().to_string_lossy(), "源.bin");
    assert_eq!(sent_bytes.load(Ordering::Relaxed), data.len() as u64);

    let _ = tokio::fs::remove_dir_all(&dir).await;
}

#[tokio::test]
async fn 中途取消无残留() {
    let (cc, sc) = connect_pair().await;
    let dir = std::env::temp_dir().join(format!("ms-xfer-cancel-{}", uuid::Uuid::new_v4()));
    tokio::fs::create_dir_all(&dir).await.unwrap();

    let src_path = dir.join("big.bin");
    let data = vec![7u8; 16_000_000]; // 16MiB
    tokio::fs::write(&src_path, &data).await.unwrap();

    let dest_dir = dir.join("recv");
    let cancel = Arc::new(AtomicBool::new(false));
    let c_send = Arc::clone(&cancel);

    let dest_dir2 = dest_dir.clone();
    let recv = tokio::spawn(async move {
        recv_file(&sc, &dest_dir2, Arc::new(AtomicBool::new(false)), |_| {}).await
    });

    // 发送进行到一部分时取消
    let send = tokio::spawn(async move { send_file(&cc, &src_path, c_send, |_| {}).await });
    // 让传输跑一会再取消
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    cancel.store(true, Ordering::Release);

    let send_res = send.await.unwrap();
    assert!(send_res.is_err(), "取消后发送应返回错误");
    let recv_res = recv.await.unwrap();
    assert!(recv_res.is_err(), "取消后接收应返回错误");

    // 接收目录里不应残留任何 .ms-part 临时文件或落地文件
    if dest_dir.exists() {
        let mut entries = tokio::fs::read_dir(&dest_dir).await.unwrap();
        let mut count = 0;
        while let Some(e) = entries.next_entry().await.unwrap() {
            count += 1;
            eprintln!("残留: {:?}", e.path());
        }
        assert_eq!(count, 0, "取消后不应有残留文件");
    }

    let _ = tokio::fs::remove_dir_all(&dir).await;
}

// 大文件完整性回归（~13s，平时 ignore，手动跑：cargo test -p ms-net --test transfer -- --ignored）。
#[ignore = "耗时大文件完整性回归，手动运行"]
#[tokio::test]
async fn 大文件64mib_sha256一致() {
    let (cc, sc) = connect_pair().await;
    let dir = std::env::temp_dir().join(format!("ms-xfer-big-{}", uuid::Uuid::new_v4()));
    tokio::fs::create_dir_all(&dir).await.unwrap();
    let src = dir.join("big.bin");
    let data: Vec<u8> = (0..67_108_864u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 11) as u8)
        .collect();
    tokio::fs::write(&src, &data).await.unwrap();
    let h = sha256(&data);
    let dest = dir.join("recv");
    let c2 = Arc::new(AtomicBool::new(false));
    let cc2 = Arc::clone(&c2);
    let recv = tokio::spawn(async move { recv_file(&sc, &dest, cc2, |_| {}).await });
    send_file(&cc, &src, c2, |_| {}).await.unwrap();
    let p = recv.await.unwrap().unwrap();
    let got = tokio::fs::read(&p).await.unwrap();
    assert_eq!(got.len(), data.len(), "大小一致");
    assert_eq!(sha256(&got), h, "64MiB sha256 一致");
    let _ = tokio::fs::remove_dir_all(&dir).await;
}
