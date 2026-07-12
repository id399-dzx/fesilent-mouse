//! 控制权状态机（DEVELOPMENT.md §2.3，TECH-REVIEW B-2）。
//!
//! 三态：LOCAL（用本机输入控制本机）、REMOTE（本机输入被 grab 并转发给对端）、
//! CONTROLLED（被对端控制，注入收到的事件）。任一时刻全局只有一个控制端。
//!
//! 进入/退出 REMOTE 必须是**固定顺序、不可重排的事务**——Deskflow #9779 的光标
//! 跳变 bug 即因顺序不全所致。状态机不直接碰平台/网络，而是产出一串有序
//! [`Action`]，由上层按序执行；这样事务顺序可被纯逻辑单测/proptest 完整覆盖。

use ms_proto::Edge;

/// 控制权状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlState {
    /// 独立使用本机。
    Local,
    /// 本机为控制端，输入转发给对端。
    Remote,
    /// 本机被对端控制，注入收到的输入。
    Controlled,
}

/// 驱动状态机的事件。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ControlEvent {
    /// 本机光标命中边缘 / 热键：开始控制对端（携带在对端的入口边与比例）。
    EnterRemote { edge: Edge, ratio: f32 },
    /// 本机影子光标返回自身屏幕 / 热键：交还控制权。
    ExitRemote,
    /// 收到对端 EnterScreen：本机进入被控。
    PeerEnter { edge: Edge, ratio: f32 },
    /// 收到对端 LeaveScreen：被控结束。
    PeerLeave,
    /// 连接断开：强制安全回到 LOCAL（FR-605）。
    Disconnect,
}

/// 要发往对端的控制信号（上层映射为 proto 消息）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Signal {
    EnterScreen { edge: Edge, ratio: f32 },
    LeaveScreen,
    ReleaseAll,
}

/// 状态机产出的有序动作，上层按序执行（顺序即契约，不可重排）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    /// 把本机光标 warp 到屏幕中心（进入 REMOTE 第 1 步）。
    WarpCursorCenter,
    /// 重置相对位移参考点（影子光标累积起点）。
    ResetDeltaRef,
    /// 初始化影子光标到对端入口坐标。
    InitShadow { edge: Edge, ratio: f32 },
    /// 设置/解除 grab（拦截本机输入）。
    SetGrab(bool),
    /// 显示/隐藏本机光标。
    SetCursorVisible(bool),
    /// 把本机光标 warp 回返回位置（退出 REMOTE 时；断连保持中心）。
    WarpCursorReturn,
    /// 被控端：在入口处显示光标。
    ShowCursorAtEntry { edge: Edge, ratio: f32 },
    /// 本端释放所有按下键（防卡键 FR-305）。
    LocalReleaseAll,
    /// 向对端发送控制信号。
    Send(Signal),
}

/// 控制权状态机。
#[derive(Debug)]
pub struct ControlMachine {
    state: ControlState,
}

impl ControlMachine {
    pub fn new() -> Self {
        Self {
            state: ControlState::Local,
        }
    }

    pub fn state(&self) -> ControlState {
        self.state
    }

    /// 处理事件，迁移状态并返回有序动作序列。非法事件（当前态无意义）返回空。
    pub fn on_event(&mut self, ev: ControlEvent) -> Vec<Action> {
        use ControlEvent as E;
        use ControlState as S;
        match (self.state, ev) {
            // ── 进入 REMOTE（固定顺序事务，TECH-REVIEW B-2）──
            (S::Local, E::EnterRemote { edge, ratio }) => {
                self.state = S::Remote;
                vec![
                    Action::WarpCursorCenter,
                    Action::ResetDeltaRef,
                    Action::InitShadow { edge, ratio },
                    Action::SetGrab(true),
                    Action::SetCursorVisible(false),
                    Action::Send(Signal::EnterScreen { edge, ratio }),
                ]
            }
            // ── 退出 REMOTE：边缘返回 / 热键 ──
            (S::Remote, E::ExitRemote) => {
                self.state = S::Local;
                vec![
                    Action::SetGrab(false),
                    Action::WarpCursorReturn,
                    Action::SetCursorVisible(true),
                    Action::LocalReleaseAll,
                    Action::Send(Signal::ReleaseAll),
                    Action::Send(Signal::LeaveScreen),
                ]
            }
            // ── 退出 REMOTE：断连（跳过一切网络发送，FR-605）──
            (S::Remote, E::Disconnect) => {
                self.state = S::Local;
                vec![
                    Action::SetGrab(false),
                    Action::WarpCursorReturn,
                    Action::SetCursorVisible(true),
                    Action::LocalReleaseAll,
                ]
            }
            // ── 控制端收到对端 LeaveScreen（对端请求释放）：同 ExitRemote 安全退出，
            //    但不再回发网络信号（避免乒乓）。对端主动释放能连带解除本端 grab。──
            (S::Remote, E::PeerLeave) => {
                self.state = S::Local;
                vec![
                    Action::SetGrab(false),
                    Action::WarpCursorReturn,
                    Action::SetCursorVisible(true),
                    Action::LocalReleaseAll,
                ]
            }
            // ── 进入被控 ──
            (S::Local, E::PeerEnter { edge, ratio }) => {
                self.state = S::Controlled;
                vec![Action::ShowCursorAtEntry { edge, ratio }]
            }
            // ── 退出被控：对端交还 / 断连——都要释放注入的按键（防卡键）──
            (S::Controlled, E::PeerLeave) | (S::Controlled, E::Disconnect) => {
                self.state = S::Local;
                vec![Action::LocalReleaseAll]
            }
            // ── LOCAL 下断连：已安全，无动作 ──
            (S::Local, E::Disconnect) => vec![],
            // ── 其余组合非法/无意义：忽略 ──
            _ => vec![],
        }
    }
}

impl Default for ControlMachine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enter() -> ControlEvent {
        ControlEvent::EnterRemote {
            edge: Edge::Right,
            ratio: 0.5,
        }
    }

    #[test]
    fn 进入remote事务顺序固定() {
        let mut m = ControlMachine::new();
        let acts = m.on_event(enter());
        assert_eq!(m.state(), ControlState::Remote);
        assert_eq!(
            acts,
            vec![
                Action::WarpCursorCenter,
                Action::ResetDeltaRef,
                Action::InitShadow {
                    edge: Edge::Right,
                    ratio: 0.5
                },
                Action::SetGrab(true),
                Action::SetCursorVisible(false),
                Action::Send(Signal::EnterScreen {
                    edge: Edge::Right,
                    ratio: 0.5
                }),
            ]
        );
    }

    #[test]
    fn 退出remote先解grab再发leave() {
        let mut m = ControlMachine::new();
        m.on_event(enter());
        let acts = m.on_event(ControlEvent::ExitRemote);
        assert_eq!(m.state(), ControlState::Local);
        assert_eq!(acts[0], Action::SetGrab(false), "必须先解除 grab");
        assert!(acts.contains(&Action::LocalReleaseAll));
        assert!(acts.contains(&Action::Send(Signal::LeaveScreen)));
    }

    #[test]
    fn 断连退出remote不发任何网络信号() {
        let mut m = ControlMachine::new();
        m.on_event(enter());
        let acts = m.on_event(ControlEvent::Disconnect);
        assert_eq!(m.state(), ControlState::Local);
        assert_eq!(acts[0], Action::SetGrab(false));
        assert!(acts.contains(&Action::LocalReleaseAll));
        assert!(
            !acts.iter().any(|a| matches!(a, Action::Send(_))),
            "断连时绝不能向已死连接发送"
        );
    }

    #[test]
    fn 被控进入与退出释放注入键() {
        let mut m = ControlMachine::new();
        let acts = m.on_event(ControlEvent::PeerEnter {
            edge: Edge::Left,
            ratio: 0.3,
        });
        assert_eq!(m.state(), ControlState::Controlled);
        assert_eq!(
            acts,
            vec![Action::ShowCursorAtEntry {
                edge: Edge::Left,
                ratio: 0.3
            }]
        );

        let acts = m.on_event(ControlEvent::Disconnect);
        assert_eq!(m.state(), ControlState::Local);
        assert_eq!(
            acts,
            vec![Action::LocalReleaseAll],
            "被控断连必须释放注入键"
        );
    }

    #[test]
    fn 非法事件被忽略() {
        let mut m = ControlMachine::new();
        // LOCAL 下 ExitRemote 无意义
        assert!(m.on_event(ControlEvent::ExitRemote).is_empty());
        assert_eq!(m.state(), ControlState::Local);
        // REMOTE 下再 EnterRemote 无意义
        m.on_event(enter());
        assert!(m.on_event(enter()).is_empty());
        assert_eq!(m.state(), ControlState::Remote);
    }

    #[test]
    fn 控制端收对端leavescreen也解grab退出且不回发() {
        let mut m = ControlMachine::new();
        m.on_event(enter()); // Remote
        let acts = m.on_event(ControlEvent::PeerLeave);
        assert_eq!(m.state(), ControlState::Local);
        assert_eq!(acts[0], Action::SetGrab(false));
        assert!(
            !acts.iter().any(|a| matches!(a, Action::Send(_))),
            "收对端 LeaveScreen 退出时不回发网络信号，防乒乓"
        );
    }
}
