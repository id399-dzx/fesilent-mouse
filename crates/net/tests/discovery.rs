//! T-202 验证：本机两个 Discovery 实例互相发现（需多播可用的网络）。
//!
//! mDNS 依赖多播，部分沙箱/CI 环境不可用；若本机环境无多播，此测试会超时失败，
//! 届时改 `#[ignore]` 并归入 H-2 真机批次（见任务备注）。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use ms_net::Discovery;

// 默认沙箱拦截多播（224.0.0.251:5353），常规 cargo test 会超时。
// 已在非沙箱真实网络实测通过（0.76s 互相发现）。需复测时：去掉 ignore 在非沙箱环境跑。
#[ignore = "需多播网络；沙箱拦截多播。非沙箱已实测通过"]
#[tokio::test]
async fn 两实例互相发现() {
    // A 广播
    let mut adv = Discovery::new().unwrap();
    adv.advertise("device-A", "Mac-A", 53728).unwrap();

    // B 浏览
    let browser = Discovery::new().unwrap();
    let mut rx = browser.browse("device-B".to_string()).unwrap();

    // 等待发现 A（最多 10s）
    let peer = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("10s 内未发现对端（多播可能不可用，见文件头说明）")
        .expect("浏览通道关闭");

    assert_eq!(peer.device_id, "device-A");
    assert_eq!(peer.proto_ver, 1);
    assert_eq!(peer.port, 53728);
    assert!(!peer.addrs.is_empty());
}
