//! 并发落地、损坏帧与等待期间取消的真实 QUIC 回归。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ms_net::quic::{MsgRecvStream, MsgSendStream};
use ms_net::transfer::{FILE_CHUNK, TransferError, recv_file, recv_on_stream, send_file};
use ms_net::{Conn, Endpoint, Identity};
use ms_proto::{FileChunk, FileDone, FileOffer, Msg};
use uuid::Uuid;

async fn pair() -> (Endpoint, Endpoint, Conn, Conn) {
    let client = Endpoint::new(
        "127.0.0.1:0".parse().unwrap(),
        &Identity::generate().unwrap(),
    )
    .unwrap();
    let server = Endpoint::new(
        "127.0.0.1:0".parse().unwrap(),
        &Identity::generate().unwrap(),
    )
    .unwrap();
    let addr = server.local_addr().unwrap();
    let (a, b) = tokio::join!(client.connect(addr), server.accept());
    (client, server, a.unwrap(), b.unwrap().unwrap())
}

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/transfer-tests")
            .join(Uuid::new_v4().to_string());
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn offer_stream(a: &Conn, b: &Conn, offer: &FileOffer) -> (MsgSendStream, MsgRecvStream) {
    let mut send = a.open_msg_stream().await.unwrap();
    send.send(&Msg::FileOffer(offer.clone())).await.unwrap();
    let mut recv = b.accept_msg_stream().await.unwrap();
    assert_eq!(
        recv.recv().await.unwrap(),
        Some(Msg::FileOffer(offer.clone()))
    );
    (send, recv)
}

async fn complete_pair(names: [&str; 2]) {
    let (_client, _server, a, b) = pair().await;
    let dir = TestDir::new();
    let mut receivers = Vec::new();
    let mut senders = Vec::new();
    for (index, name) in names.into_iter().enumerate() {
        let offer = FileOffer {
            id: Uuid::new_v4(),
            name: name.into(),
            size: 4,
        };
        let (mut send, mut stream) = offer_stream(&a, &b, &offer).await;
        let path = dir.0.clone();
        let received = Arc::new(AtomicBool::new(false));
        let progress = received.clone();
        let id = offer.id;
        receivers.push(tokio::spawn(async move {
            recv_on_stream(
                &mut stream,
                &offer,
                &path,
                Arc::new(AtomicBool::new(false)),
                |_| {
                    progress.store(true, Ordering::Release);
                },
            )
            .await
        }));
        send.send(&Msg::FileChunk(FileChunk {
            id,
            offset: 0,
            data: vec![index as u8 + 1; 4],
        }))
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while !received.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        senders.push((send, id));
    }
    // 两条流都已有暂存内容，再完成，覆盖“同名”和“同主名不同扩展”两种竞争。
    for (mut send, id) in senders {
        send.send(&Msg::FileDone(FileDone { id })).await.unwrap();
        send.finish().await.unwrap();
    }
    let mut landed = Vec::new();
    for (index, receiver) in receivers.into_iter().enumerate() {
        let path = receiver.await.unwrap().unwrap();
        assert_eq!(
            tokio::fs::read(&path).await.unwrap(),
            vec![index as u8 + 1; 4]
        );
        landed.push(path);
    }
    assert_ne!(landed[0], landed[1], "并发文件必须各自落地且不覆盖");
    assert_eq!(
        std::fs::read_dir(&dir.0).unwrap().count(),
        2,
        "不残留临时文件"
    );
}

#[tokio::test]
async fn 同主名不同扩展并发接收不共用临时文件() {
    complete_pair(["report.txt", "report.csv"]).await;
}

#[tokio::test]
async fn 同名并发接收各自落地且不覆盖() {
    complete_pair(["report.txt", "report.txt"]).await;
}

#[tokio::test]
async fn 损坏文件消息不得落地或残留() {
    let (_client, _server, a, b) = pair().await;
    for case in 0..7 {
        let dir = TestDir::new();
        let id = Uuid::new_v4();
        let offer = FileOffer {
            id,
            name: "checked.bin".into(),
            size: 4,
        };
        let (mut send, mut stream) = offer_stream(&a, &b, &offer).await;
        let path = dir.0.clone();
        let receiver = tokio::spawn(async move {
            recv_on_stream(
                &mut stream,
                &offer,
                &path,
                Arc::new(AtomicBool::new(false)),
                |_| {},
            )
            .await
        });
        let chunk = FileChunk {
            id: if case == 0 { Uuid::new_v4() } else { id },
            offset: u64::from(case == 1),
            data: vec![
                5;
                if case == 2 {
                    5
                } else if case == 3 {
                    2
                } else {
                    4
                }
            ],
        };
        send.send(&Msg::FileChunk(chunk)).await.unwrap();
        if case != 5 {
            let _ = send
                .send(&Msg::FileDone(FileDone {
                    id: if case == 4 { Uuid::new_v4() } else { id },
                }))
                .await;
        }
        if case == 6 {
            let _ = send
                .send(&Msg::FileChunk(FileChunk {
                    id,
                    offset: 4,
                    data: vec![7],
                }))
                .await;
        }
        let _ = send.finish().await;
        let result = receiver.await.unwrap();
        assert!(
            matches!(result, Err(TransferError::Proto(_))),
            "case {case}: {result:?}"
        );
        assert_eq!(
            std::fs::read_dir(&dir.0).unwrap().count(),
            0,
            "case {case} 不得落地或残留"
        );
    }
}

#[tokio::test]
async fn 等待下一块时取消能及时清理() {
    let (_client, _server, a, b) = pair().await;
    let dir = TestDir::new();
    let offer = FileOffer {
        id: Uuid::new_v4(),
        name: "waiting.bin".into(),
        size: 4,
    };
    let (_send, mut stream) = offer_stream(&a, &b, &offer).await;
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    let path = dir.0.clone();
    let mut receiver =
        tokio::spawn(async move { recv_on_stream(&mut stream, &offer, &path, flag, |_| {}).await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while std::fs::read_dir(&dir.0).unwrap().count() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    cancel.store(true, Ordering::Release);
    let result = tokio::time::timeout(Duration::from_secs(1), &mut receiver).await;
    if result.is_err() {
        receiver.abort();
        let _ = receiver.await;
    }
    assert!(
        matches!(result, Ok(Ok(Err(TransferError::Cancelled)))),
        "网络等待不能挡住取消"
    );
    assert_eq!(std::fs::read_dir(&dir.0).unwrap().count(), 0);
}

#[tokio::test]
async fn 接收任务被终止也清理临时文件() {
    let (_client, _server, a, b) = pair().await;
    let dir = TestDir::new();
    let offer = FileOffer {
        id: Uuid::new_v4(),
        name: "aborted.bin".into(),
        size: 4,
    };
    let (mut send, mut stream) = offer_stream(&a, &b, &offer).await;
    let id = offer.id;
    let path = dir.0.clone();
    let (progress, reached) = tokio::sync::oneshot::channel();
    let mut progress = Some(progress);
    let receiver = tokio::spawn(async move {
        recv_on_stream(
            &mut stream,
            &offer,
            &path,
            Arc::new(AtomicBool::new(false)),
            |_| {
                if let Some(progress) = progress.take() {
                    let _ = progress.send(());
                }
            },
        )
        .await
    });
    send.send(&Msg::FileChunk(FileChunk {
        id,
        offset: 0,
        data: vec![1],
    }))
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(2), reached)
        .await
        .unwrap()
        .unwrap();
    receiver.abort();
    let _ = receiver.await;
    assert_eq!(
        std::fs::read_dir(&dir.0).unwrap().count(),
        0,
        "会话结束取消任务时不得残留"
    );
}

#[tokio::test]
async fn 文件夹明确拒绝且不发出文件流() {
    let (_client, _server, a, b) = pair().await;
    let dir = TestDir::new();
    let result = send_file(&a, &dir.0, Arc::new(AtomicBool::new(false)), |_| {}).await;
    assert!(matches!(result, Err(TransferError::Directory)));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), b.accept_msg_stream())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn 发送期间源文件被截短不得标记完成或落地() {
    let (_client, _server, a, b) = pair().await;
    let dir = TestDir::new();
    let source = dir.0.join("changing.bin");
    let dest = dir.0.join("received");
    std::fs::write(&source, vec![1; FILE_CHUNK * 3]).unwrap();
    let receiving = dest.clone();
    let receiver = tokio::spawn(async move {
        recv_file(&b, &receiving, Arc::new(AtomicBool::new(false)), |_| {}).await
    });
    let mut truncated = false;
    let result = send_file(&a, &source, Arc::new(AtomicBool::new(false)), |_| {
        if !truncated {
            std::fs::OpenOptions::new()
                .write(true)
                .open(&source)
                .unwrap()
                .set_len(0)
                .unwrap();
            truncated = true;
        }
    })
    .await;
    assert!(matches!(result, Err(TransferError::SourceChanged)));
    assert!(receiver.await.unwrap().is_err());
    assert!(!dest.exists() || std::fs::read_dir(dest).unwrap().count() == 0);
}
