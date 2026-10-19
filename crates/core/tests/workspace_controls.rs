//! 设备工作台的运行交互回归；通过两个真实 Engine 交换输出验证控制权事务。

#![allow(clippy::unwrap_used)]

use ms_core::control::{Action, ControlState};
use ms_core::engine::{EdgeConfig, Engine, EngineOut, Inject};
use ms_core::input_map::{Shortcut, ShortcutTracker};
use ms_core::layout::{Point, offset_entry_ratio};
use ms_platform::{HidKey, InputEvent, Rect};
use ms_proto::{self as proto, Edge, Layout, Msg};

fn rect(width: f64, height: f64) -> Rect {
    Rect {
        x: -200.0,
        y: -100.0,
        w: width,
        h: height,
    }
}

fn engine(own: Rect, peer: Rect, side: Edge) -> Engine {
    Engine::new(
        own,
        peer,
        Layout {
            peer_position: side,
        },
        EdgeConfig {
            threshold: 2.0,
            corner_dead: 0.0,
        },
    )
}

fn key(hid: u16, down: bool) -> InputEvent {
    InputEvent::Key {
        key: HidKey(hid),
        down,
    }
}

fn shortcuts() -> (Shortcut, Shortcut) {
    let switch = Shortcut {
        ctrl: true,
        alt: true,
        shift: false,
        meta: false,
        key: 0x2c,
    };
    (
        switch,
        Shortcut {
            key: 0x0f,
            ..switch
        },
    )
}

fn deliver(receiver: &mut Engine, output: &EngineOut) -> Vec<Inject> {
    output
        .to_peer
        .iter()
        .flat_map(|message| receiver.feed_peer(message.clone()).inject)
        .collect()
}

fn enter_ratio(output: &EngineOut) -> Option<f32> {
    output.to_peer.iter().find_map(|message| match message {
        Msg::EnterScreen(enter) => Some(enter.ratio),
        _ => None,
    })
}

fn point_on_edge(own: Rect, side: Edge, along: f64) -> InputEvent {
    let (ax, ay) = match side {
        Edge::Right => (own.x + own.w - 1.0, own.y + along),
        Edge::Left => (own.x, own.y + along),
        Edge::Top => (own.x + along, own.y),
        Edge::Bottom => (own.x + along, own.y + own.h - 1.0),
    };
    InputEvent::MouseMove {
        ax,
        ay,
        dx: 20.0,
        dy: 20.0,
    }
}

#[test]
fn 自定义组合键左右修饰键等价且重复键只触发一次() {
    let binding = Shortcut {
        ctrl: true,
        alt: false,
        shift: true,
        meta: false,
        key: 0x3b,
    };
    let mut tracker = ShortcutTracker::default();
    assert!(!tracker.update(0xe4, true, binding));
    assert!(!tracker.update(0xe5, true, binding));
    assert!(tracker.update(0x3b, true, binding));
    assert!(!tracker.update(0x3b, true, binding));
    assert!(!tracker.update(0x3b, false, binding));
    assert!(tracker.update(0x3b, true, binding));
    tracker.update(0x3b, false, binding);
    tracker.update(0xe2, true, binding);
    assert!(!tracker.update(0x3b, true, binding), "额外修饰键不能误触发");
    tracker.reset();
    assert!(!tracker.update(0x3b, true, binding));
}

#[test]
fn 快捷键跨屏后吞掉触发末键重复与抬起且普通输入仍可往返() {
    let own = rect(1000.0, 1000.0);
    let mut a = engine(own, own, Edge::Right);
    let mut b = engine(own, own, Edge::Left);
    a.set_shortcuts(Some(shortcuts()));
    a.feed_local(key(0xe0, true));
    a.feed_local(key(0xe2, true));
    let entered = a.feed_local(key(0x2c, true));
    assert_eq!(a.state(), ControlState::Remote);
    assert!(entered.actions.contains(&Action::SetGrab(true)));
    for hid in [0xe0, 0xe2, 0x2c] {
        assert!(
            entered.inject.contains(&Inject::Key { hid, down: false }),
            "进入grab前已到达本机OS的快捷键必须显式释放"
        );
    }
    deliver(&mut b, &entered);
    assert_eq!(b.state(), ControlState::Controlled);
    for down in [true, true, false] {
        let output = a.feed_local(key(0x2c, down));
        assert_eq!(a.state(), ControlState::Remote, "长按不能反复切换");
        assert!(
            !output
                .to_peer
                .iter()
                .any(|message| matches!(message, Msg::KeyEvent(event) if event.hid == 0x2c)),
            "快捷键末键的重复/抬起不能作为普通空格发送到对端"
        );
    }
    for modifier in [0xe0, 0xe2] {
        deliver(&mut b, &a.feed_local(key(modifier, false)));
    }
    let typed = a.feed_local(key(0x04, true));
    assert!(deliver(&mut b, &typed).contains(&Inject::Key {
        hid: 0x04,
        down: true,
    }));
    deliver(&mut b, &a.feed_local(key(0x04, false)));
    for modifier in [0xe0, 0xe2] {
        deliver(&mut b, &a.feed_local(key(modifier, true)));
    }
    let returned = a.feed_local(key(0x2c, true));
    assert_eq!(returned.actions.first(), Some(&Action::SetGrab(false)));
    assert!(deliver(&mut b, &returned).contains(&Inject::ReleaseAll));
    assert_eq!(a.state(), ControlState::Local);
    assert_eq!(b.state(), ControlState::Local);
}

#[test]
fn 临时锁定结束控制并拒绝后续远端输入与进入请求() {
    let own = rect(1000.0, 1000.0);
    let mut a = engine(own, own, Edge::Right);
    let mut b = engine(own, own, Edge::Left);
    a.set_shortcuts(Some(shortcuts()));
    deliver(&mut b, &a.toggle_control());
    for modifier in [0xe0, 0xe2] {
        deliver(&mut b, &a.feed_local(key(modifier, true)));
    }
    let locked = a.feed_local(key(0x0f, true));
    assert!(a.local_locked());
    assert_eq!(a.state(), ControlState::Local);
    assert_eq!(locked.actions.first(), Some(&Action::SetGrab(false)));
    assert!(locked.inject.contains(&Inject::ReleaseAll));
    assert!(deliver(&mut b, &locked).contains(&Inject::ReleaseAll));
    assert_eq!(b.state(), ControlState::Local);
    let request = b.toggle_control();
    assert_eq!(b.state(), ControlState::Remote);
    let rejected = a.feed_peer(
        request
            .to_peer
            .into_iter()
            .find(|m| matches!(m, Msg::EnterScreen(_)))
            .unwrap(),
    );
    assert!(rejected.inject.is_empty());
    deliver(&mut b, &rejected);
    assert_eq!(a.state(), ControlState::Local);
    assert_eq!(b.state(), ControlState::Local);
    assert!(
        a.feed_peer(Msg::from(proto::KeyEvent {
            hid: 4,
            down: true,
            shadow: (0.0, 0.0)
        }))
        .inject
        .is_empty()
    );
    assert!(
        a.feed_local(point_on_edge(own, Edge::Right, 500.0))
            .to_peer
            .is_empty()
    );
    a.set_local_lock(false);
    assert_eq!(a.state(), ControlState::Local, "解锁不得自动抢占控制权");
    assert!(enter_ratio(&a.toggle_control()).is_some());
}

#[test]
fn 被控端锁定释放已注入按键并让控制方解除grab() {
    let own = rect(1000.0, 1000.0);
    let mut controller = engine(own, own, Edge::Right);
    let mut controlled = engine(own, own, Edge::Left);
    deliver(&mut controlled, &controller.toggle_control());
    deliver(&mut controlled, &controller.feed_local(key(0xe0, true)));
    let released = controlled.set_local_lock(true);
    assert!(released.inject.contains(&Inject::ReleaseAll));
    assert!(
        released
            .to_peer
            .iter()
            .any(|message| matches!(message, Msg::ReleaseAll(_)))
    );
    assert!(
        released
            .to_peer
            .iter()
            .any(|message| matches!(message, Msg::LeaveScreen(_)))
    );
    deliver(&mut controller, &released);
    assert_eq!(controlled.state(), ControlState::Local);
    assert_eq!(controller.state(), ControlState::Local);
    assert!(controlled.local_locked());
}

#[test]
fn 双端同时进入按指纹优先级只保留一个控制方且交换顺序不影响结果() {
    let own = rect(1000.0, 1000.0);
    for winner_first in [false, true] {
        let mut winner = engine(own, own, Edge::Right);
        let mut loser = engine(own, own, Edge::Left);
        winner.set_collision_priority(true);
        loser.set_collision_priority(false);
        let winner_enter = winner.toggle_control();
        let loser_enter = loser.toggle_control();
        assert_eq!(winner.state(), ControlState::Remote);
        assert_eq!(loser.state(), ControlState::Remote);
        if winner_first {
            deliver(&mut winner, &loser_enter);
            deliver(&mut loser, &winner_enter);
        } else {
            deliver(&mut loser, &winner_enter);
            deliver(&mut winner, &loser_enter);
        }
        assert_eq!(winner.state(), ControlState::Remote);
        assert_eq!(loser.state(), ControlState::Controlled);
        let typed = winner.feed_local(key(0x05, true));
        assert!(deliver(&mut loser, &typed).contains(&Inject::Key {
            hid: 0x05,
            down: true
        }));
        assert!(loser.feed_local(key(0x06, true)).to_peer.is_empty());
        deliver(&mut loser, &winner.toggle_control());
        assert_eq!(winner.state(), ControlState::Local);
        assert_eq!(loser.state(), ControlState::Local);
    }
}

#[test]
fn 非零偏移按物理像素映射四边且只在共同边穿越() {
    let own = rect(1200.0, 1000.0);
    let peer = Rect {
        x: 300.0,
        y: 200.0,
        w: 800.0,
        h: 600.0,
    };
    for (side, along, expected) in [
        (Edge::Left, 500.0, 0.5),
        (Edge::Right, 500.0, 0.5),
        (Edge::Top, 600.0, 0.5),
        (Edge::Bottom, 600.0, 0.5),
    ] {
        let mut a = engine(own, peer, side);
        a.set_display_offset(200.0);
        assert!(
            a.feed_local(point_on_edge(own, side, 100.0))
                .to_peer
                .is_empty()
        );
        assert_eq!(a.state(), ControlState::Local);
        let entered = a.feed_local(point_on_edge(own, side, along));
        assert!((enter_ratio(&entered).unwrap() - expected).abs() < 0.00001);
        let target_side = ms_core::layout::opposite(side);
        let mut b = engine(peer, own, target_side);
        let injections = deliver(&mut b, &entered);
        assert!(injections.iter().any(|injection| match injection {
            Inject::MoveTo { x, y } => match side {
                Edge::Left | Edge::Right => (*y - 500.0).abs() < 0.001,
                Edge::Top | Edge::Bottom => (*x - 700.0).abs() < 0.001,
            },
            _ => false,
        }));
    }
    let mut blocked = engine(own, peer, Edge::Right);
    blocked.set_display_offset(200.0);
    assert!(
        blocked
            .feed_local(point_on_edge(own, Edge::Right, 900.0))
            .to_peer
            .is_empty()
    );
    blocked.set_display_offset(-200.0);
    let entered = blocked.feed_local(point_on_edge(own, Edge::Right, 100.0));
    assert!((enter_ratio(&entered).unwrap() - 0.5).abs() < 0.00001);
}

#[test]
fn 默认零偏移在不同分辨率下保持旧ratio() {
    let own = rect(1200.0, 1000.0);
    let peer = rect(800.0, 600.0);
    for (side, along) in [
        (Edge::Left, 250.0),
        (Edge::Right, 250.0),
        (Edge::Top, 300.0),
        (Edge::Bottom, 300.0),
    ] {
        let mut a = engine(own, peer, side);
        a.set_display_offset(0.0);
        assert!(
            (enter_ratio(&a.feed_local(point_on_edge(own, side, along))).unwrap() - 0.25).abs()
                < 0.00001
        );
    }
}

#[test]
fn 偏移换算拒绝非有限显示尺寸避免发送畸形ratio() {
    let own = rect(1000.0, 1000.0);
    let pos = Point {
        x: own.x + 999.0,
        y: own.y + 500.0,
    };
    for length in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.0, -1.0] {
        let peer = Rect { h: length, ..own };
        assert!(offset_entry_ratio(&own, &peer, pos, Edge::Right, 100.0).is_none());
    }
}
