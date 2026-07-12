//! T-301 proptest：任意事件序列下，控制权状态机的安全不变量恒成立。

use ms_core::control::{Action, ControlEvent, ControlMachine, ControlState, Signal};
use ms_proto::Edge;
use proptest::prelude::*;

fn any_event() -> impl Strategy<Value = ControlEvent> {
    let edge = prop_oneof![
        Just(Edge::Left),
        Just(Edge::Right),
        Just(Edge::Top),
        Just(Edge::Bottom),
    ];
    prop_oneof![
        (edge.clone(), 0.0f32..1.0)
            .prop_map(|(edge, ratio)| ControlEvent::EnterRemote { edge, ratio }),
        Just(ControlEvent::ExitRemote),
        (edge, 0.0f32..1.0).prop_map(|(edge, ratio)| ControlEvent::PeerEnter { edge, ratio }),
        Just(ControlEvent::PeerLeave),
        Just(ControlEvent::Disconnect),
    ]
}

proptest! {
    /// 不变量集合：跑任意事件序列，每步校验。
    #[test]
    fn 安全不变量恒成立(events in prop::collection::vec(any_event(), 0..200)) {
        let mut m = ControlMachine::new();
        let mut grabbed = false; // 跟踪 grab 状态（由动作推导）

        for ev in events {
            let prev = m.state();
            let acts = m.on_event(ev);

            // 推进 grab 跟踪
            for a in &acts {
                if let Action::SetGrab(g) = a {
                    grabbed = *g;
                }
            }

            // ① 断连永远回到 LOCAL
            if ev == ControlEvent::Disconnect {
                prop_assert_eq!(m.state(), ControlState::Local, "断连后必须是 LOCAL");
            }

            // ② 断连产生的动作里绝不含网络发送（连接已死）
            if ev == ControlEvent::Disconnect {
                prop_assert!(
                    !acts.iter().any(|a| matches!(a, Action::Send(_))),
                    "断连不得发送网络信号"
                );
            }

            // ③ 任何"退出 REMOTE"的迁移：第一个动作必是 SetGrab(false)，且含 LocalReleaseAll
            if prev == ControlState::Remote && m.state() != ControlState::Remote {
                prop_assert_eq!(acts.first(), Some(&Action::SetGrab(false)), "退出 REMOTE 必先解 grab");
                prop_assert!(acts.contains(&Action::LocalReleaseAll), "退出 REMOTE 必释放按键");
            }

            // ④ 进入 REMOTE 的迁移：grab 在 send EnterScreen 之前已置位（顺序契约）
            if prev == ControlState::Local && m.state() == ControlState::Remote {
                let grab_idx = acts.iter().position(|a| *a == Action::SetGrab(true));
                let send_idx = acts.iter().position(|a|
                    matches!(a, Action::Send(Signal::EnterScreen { .. })));
                prop_assert!(grab_idx < send_idx, "必须先 grab 再发 EnterScreen");
                prop_assert_eq!(acts.first(), Some(&Action::WarpCursorCenter), "进入必先 warp 中心");
            }

            // ⑤ grab 只在 REMOTE 态为真
            match m.state() {
                ControlState::Remote => prop_assert!(grabbed, "REMOTE 态 grab 应为真"),
                _ => prop_assert!(!grabbed, "非 REMOTE 态 grab 应为假"),
            }
        }
    }
}
