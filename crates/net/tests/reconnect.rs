//! T-204 验证：连接瞬断后客户端自动重连 ≤5s（FR-602/607）。
//!
//! 服务端全程存活；对**第一条**连接短暂响应后主动关闭（模拟网络瞬断/对端重启
//! 那条 TCP/QUIC 连接消失），客户端 drive 检测到关闭→退避→重连到同一服务端。
//! 不依赖端口重用（quinn 异步释放套接字，进程内同端口重绑不可靠）。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ms_net::session::{respond_loop, run_reconnecting_client};
use ms_net::{ConnState, Endpoint, Identity};
use tokio::sync::watch;

/// 起一个服务端：第一条连接响应 300ms 后关闭（模拟瞬断），其余连接正常长响应。
fn spawn_server() -> (tokio::task::JoinHandle<()>, SocketAddr) {
    let id = Identity::generate().unwrap();
    let ep = Endpoint::new((Ipv4Addr::LOCALHOST, 0).into(), &id).unwrap();
    let addr = ep.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let mut set = tokio::task::JoinSet::new();
        let mut first = true;
        while let Some(Ok(conn)) = ep.accept().await {
            if first {
                first = false;
                set.spawn(async move {
                    // 先正常响应一会，确保客户端进入 Connected，再制造瞬断
                    let t = tokio::time::sleep(Duration::from_millis(300));
                    tokio::pin!(t);
                    loop {
                        tokio::select! {
                            _ = &mut t => { conn.close(); break; }
                            r = conn.recv_uni() => match r {
                                Ok(ms_proto::Msg::Ping(p)) => {
                                    let _ = conn.send_uni(&ms_proto::Msg::from(
                                        ms_proto::Pong { seq: p.seq, t0: p.t0 })).await;
                                }
                                Ok(_) => {}
                                Err(_) => break,
                            }
                        }
                    }
                });
            } else {
                set.spawn(respond_loop(conn));
            }
        }
    });
    (handle, addr)
}

async fn wait_state(
    rx: &mut watch::Receiver<ConnState>,
    want: ConnState,
    within: Duration,
) -> bool {
    tokio::time::timeout(within, async {
        loop {
            if *rx.borrow_and_update() == want {
                return;
            }
            if rx.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    })
    .await
    .is_ok()
}

#[tokio::test]
async fn 瞬断后自动重连() {
    let (server, addr) = spawn_server();

    let client_id = Identity::generate().unwrap();
    let client_ep = Arc::new(Endpoint::new((Ipv4Addr::LOCALHOST, 0).into(), &client_id).unwrap());
    let (state_tx, mut state_rx) = watch::channel(ConnState::Discovering);
    let wake = Arc::new(tokio::sync::Notify::new());
    let client = tokio::spawn(run_reconnecting_client(
        Arc::clone(&client_ep),
        addr,
        state_tx,
        wake,
    ));

    // 1) 首次连上
    assert!(
        wait_state(&mut state_rx, ConnState::Connected, Duration::from_secs(5)).await,
        "首次应在 5s 内连上"
    );

    // 2) 服务端关闭第一条连接 → 客户端应进入重连态
    assert!(
        wait_state(
            &mut state_rx,
            ConnState::Reconnecting,
            Duration::from_secs(8)
        )
        .await,
        "瞬断后应进入重连态"
    );

    // 3) 自动重连回 Connected，且 ≤5s
    let t = Instant::now();
    assert!(
        wait_state(&mut state_rx, ConnState::Connected, Duration::from_secs(5)).await,
        "应在 5s 内自动重连成功"
    );
    assert!(
        t.elapsed() <= Duration::from_secs(5),
        "重连耗时 {:?} 超 5s",
        t.elapsed()
    );

    client.abort();
    server.abort();
}
