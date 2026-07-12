//! T-306 卡键防护审计：穷举控制权状态机的所有"退出可能按键的状态"的迁移，
//! 断言每条都释放按键（LocalReleaseAll），杜绝卡键（FR-305/605）。

use ms_core::control::{Action, ControlEvent, ControlMachine, ControlState, Signal};
use ms_core::engine::{EdgeConfig, Engine, Inject};
use ms_platform::Rect;
use ms_proto::{self as proto, Edge, Layout, Msg};

/// "持键态"：可能有按下的键（REMOTE 持有本机转发键；CONTROLLED 持有注入键）。
fn is_holding(s: ControlState) -> bool {
    matches!(s, ControlState::Remote | ControlState::Controlled)
}

fn all_events() -> Vec<ControlEvent> {
    vec![
        ControlEvent::EnterRemote {
            edge: Edge::Right,
            ratio: 0.5,
        },
        ControlEvent::ExitRemote,
        ControlEvent::PeerEnter {
            edge: Edge::Left,
            ratio: 0.5,
        },
        ControlEvent::PeerLeave,
        ControlEvent::Disconnect,
    ]
}

/// 把状态机驱动到目标态。
fn drive_to(target: ControlState) -> ControlMachine {
    let mut m = ControlMachine::new();
    match target {
        ControlState::Local => {}
        ControlState::Remote => {
            m.on_event(ControlEvent::EnterRemote {
                edge: Edge::Right,
                ratio: 0.5,
            });
        }
        ControlState::Controlled => {
            m.on_event(ControlEvent::PeerEnter {
                edge: Edge::Left,
                ratio: 0.5,
            });
        }
    }
    assert_eq!(m.state(), target);
    m
}

#[test]
fn 所有从持键态离开的迁移都释放按键() {
    for &start in &[ControlState::Remote, ControlState::Controlled] {
        for ev in all_events() {
            let mut m = drive_to(start);
            let acts = m.on_event(ev);
            // 只关心"离开了持键态"的迁移
            if is_holding(start) && m.state() != start {
                assert!(
                    acts.contains(&Action::LocalReleaseAll),
                    "{start:?} --{ev:?}--> {:?} 未释放按键（卡键风险）",
                    m.state()
                );
            }
        }
    }
}

#[test]
fn 退出remote同时通知对端释放() {
    let mut m = drive_to(ControlState::Remote);
    let acts = m.on_event(ControlEvent::ExitRemote);
    assert!(acts.contains(&Action::LocalReleaseAll), "本端释放");
    assert!(
        acts.contains(&Action::Send(Signal::ReleaseAll)),
        "应通知对端释放（双端防卡键）"
    );
}

#[test]
fn 断连绝不向对端发送() {
    // 断连时连接已死，不能发送（会阻塞/报错），但本端必须释放
    for &start in &[ControlState::Remote, ControlState::Controlled] {
        let mut m = drive_to(start);
        let acts = m.on_event(ControlEvent::Disconnect);
        assert!(
            acts.contains(&Action::LocalReleaseAll),
            "{start:?} 断连必释放本端"
        );
        assert!(
            !acts.iter().any(|a| matches!(a, Action::Send(_))),
            "{start:?} 断连不得发送网络信号"
        );
    }
}

#[test]
fn 收到对端releaseall消息必注入释放() {
    // 被控端：无论何时收到 ReleaseAll 都要释放注入的键
    let mut b = Engine::new(
        Rect {
            x: 0.0,
            y: 0.0,
            w: 1000.0,
            h: 1000.0,
        },
        Rect {
            x: 0.0,
            y: 0.0,
            w: 1000.0,
            h: 1000.0,
        },
        Layout {
            peer_position: Edge::Left,
        },
        EdgeConfig::default(),
    );
    b.feed_peer(Msg::from(proto::EnterScreen {
        entry_edge: Edge::Left,
        ratio: 0.5,
    }));
    let out = b.feed_peer(Msg::from(proto::ReleaseAll {}));
    assert!(
        out.inject.contains(&Inject::ReleaseAll),
        "收到 ReleaseAll 必注入释放"
    );
}
