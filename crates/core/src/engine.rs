//! 编织引擎：把捕获事件、控制权状态机、坐标映射、影子光标、协议消息串成一条
//! 链路（DEVELOPMENT.md §5.1）。引擎本身不碰平台/网络，输入是事件、输出是
//! "要发给对端的消息 + 要执行的平台动作 + 要注入的命令"，便于回环端到端单测。

use ms_platform::{Button, InputEvent, Rect};
use ms_proto::{self as proto, Layout, Msg};

use crate::control::{Action, ControlEvent, ControlMachine, ControlState, Signal};
use crate::input_map::{ModifierMode, map_modifier};
use crate::layout::{
    Point, clamp_to_rect, detect_exit, entry_point, inset_from_edge, opposite, push_toward,
};
use crate::shadow::{SeqGate, ShadowCursor};

/// 要在本机执行的注入命令（被控端）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Inject {
    MoveTo { x: f64, y: f64 },
    Key { hid: u16, down: bool },
    Button { btn: u8, down: bool },
    Scroll { dx: f32, dy: f32, precise: bool },
    ReleaseAll,
}

/// 引擎一次处理的输出。
#[derive(Debug, Default)]
pub struct EngineOut {
    /// 要发送给对端的协议消息。
    pub to_peer: Vec<Msg>,
    /// 要执行的平台动作（grab/光标/warp 等）。
    pub actions: Vec<Action>,
    /// 要注入的命令（被控端）。
    pub inject: Vec<Inject>,
}

/// 交还控制权返回本机时，光标还原到「离开点」后再朝屏幕内侧回缩的距离（像素）。
/// 避免还原到正贴边缘的位置——既贴边手感差，又容易立刻再次触发穿越。
const RETURN_INSET: f64 = 20.0;

/// 边缘触发参数。
pub struct EdgeConfig {
    pub threshold: f64,
    pub corner_dead: f64,
}

impl Default for EdgeConfig {
    fn default() -> Self {
        Self {
            threshold: 1.0,
            corner_dead: 10.0,
        }
    }
}

pub struct Engine {
    machine: ControlMachine,
    own_rect: Rect,
    peer_rect: Rect,
    layout: Layout,
    cfg: EdgeConfig,
    /// LOCAL 态本机光标估计（累积相对位移，用于边缘判定）。
    local_pos: Point,
    /// 上次穿越离开本机时的光标位置（已朝屏内回缩 [`RETURN_INSET`]）。
    /// 交还控制权（`WarpCursorReturn`）时据此把本机光标还原到离开处，
    /// 而不是停在 grab 期间被拉去的屏幕中心。
    exit_pos: Point,
    /// REMOTE 态影子光标。
    shadow: Option<ShadowCursor>,
    /// CONTROLLED 态序号门。
    gate: SeqGate,
    /// 被控端注入时的修饰键映射模式（FR-302）。
    mod_mode: ModifierMode,
    /// REMOTE 返回检测迟滞标志：进入 REMOTE 时为 false，影子先离开返回带后置 true，
    /// 之后再进入返回带才真正交还控制。避免「入口点贴边 → 瞬间被弹回」（问题 A）。
    remote_armed: bool,
    /// 本端是否允许主动发起控制对端（从属端置 false，只被控不反控；单向主从）。
    can_initiate: bool,
    /// 注入滚轮时反转纵向方向（被控端本地偏好）。
    invert_scroll: bool,
    /// 控制对端时仅缩放纵向滚轮；小数余量跨同方向事件累积。
    scroll_scaler: ScrollScaler,
    /// 边缘"刻意触发"推挤阈值（像素，T-511，FR-205/207）：贴住朝对端那条边后，须继续
    /// 朝该边累计推够此距离才穿越，避免一碰边就穿越（够不着菜单栏/任务栏）。
    /// `<= 0` 时退化为「贴边即穿越」（旧行为，库默认）；应用层默认设非零值。
    edge_push_dist: f64,
    /// 当前贴边累计的推挤量（达 `edge_push_dist` 即穿越并清零；离开边缘带也清零）。
    edge_push_accum: f64,
    /// 交还控制权"刻意触发"推挤阈值（像素，T-515）：REMOTE 态影子回到返回边后，须继续
    /// 朝该边累计推够此距离才交还控制权，避免贴对端那条边就瞬间弹回（够不着对端菜单栏）。
    /// `<= 0` 时退化为「贴边即交还」（旧行为，库默认）；仅在控制方（REMOTE 态）生效。
    return_push_dist: f64,
    /// 当前返回边累计的推挤量（达 `return_push_dist` 即交还并清零；离开返回带也清零）。
    return_push_accum: f64,
}

impl Engine {
    pub fn new(own_rect: Rect, peer_rect: Rect, layout: Layout, cfg: EdgeConfig) -> Self {
        let local_pos = Point {
            x: own_rect.x + own_rect.w / 2.0,
            y: own_rect.y + own_rect.h / 2.0,
        };
        Self {
            machine: ControlMachine::new(),
            own_rect,
            peer_rect,
            layout,
            cfg,
            local_pos,
            exit_pos: local_pos,
            shadow: None,
            gate: SeqGate::new(),
            mod_mode: ModifierMode::Position,
            remote_armed: false,
            can_initiate: true,
            invert_scroll: false,
            scroll_scaler: ScrollScaler::default(),
            edge_push_dist: 0.0,
            edge_push_accum: 0.0,
            return_push_dist: 0.0,
            return_push_accum: 0.0,
        }
    }

    pub fn state(&self) -> ControlState {
        self.machine.state()
    }

    /// 设置被控端修饰键映射模式（FR-302）。
    pub fn set_modifier_mode(&mut self, mode: ModifierMode) {
        self.mod_mode = mode;
    }

    /// 设为从属端（false）：本端不主动发起控制对端，只接受被控（单向主从）。
    pub fn set_can_initiate(&mut self, yes: bool) {
        self.can_initiate = yes;
    }

    /// 反转注入滚轮的纵向方向。
    pub fn set_invert_scroll(&mut self, yes: bool) {
        self.invert_scroll = yes;
    }

    /// 设置控制 Mac 时的纵向滚轮倍率。倍率变化会清除旧倍率余量。
    pub fn set_scroll_speed(&mut self, factor: f64) {
        self.scroll_scaler.set_factor(factor);
    }

    /// 实时更新屏幕布局（对端方位），用于设置改动即时生效（不必重启互控）。
    pub fn set_layout(&mut self, layout: Layout) {
        self.layout = layout;
    }

    /// 设置边缘"刻意触发"推挤阈值（像素，T-511）。`<= 0` 表示贴边即穿越（关闭推挤）。
    /// 改阈值时清零当前蓄力，避免旧累计跨阈值瞬间穿越。
    pub fn set_edge_push(&mut self, px: f64) {
        self.edge_push_dist = px.max(0.0);
        self.edge_push_accum = 0.0;
    }

    /// 设置交还控制权"刻意触发"推挤阈值（像素，T-515）。`<= 0` 表示贴边即交还（关闭）。
    /// 仅在本端作为控制方（REMOTE 态）时生效：影子回到返回边后须推够此距离才交还。
    pub fn set_return_push(&mut self, px: f64) {
        self.return_push_dist = px.max(0.0);
        self.return_push_accum = 0.0;
    }

    /// 把控制机产出的 Action 翻译成 EngineOut（处理影子初始化与信号→消息）。
    fn apply_actions(&mut self, acts: Vec<Action>, out: &mut EngineOut, entry: Option<Point>) {
        for a in acts {
            match a {
                Action::InitShadow { edge, ratio } => {
                    self.scroll_scaler.reset();
                    let e = entry.unwrap_or_else(|| entry_point(&self.peer_rect, edge, ratio));
                    self.shadow = Some(ShadowCursor::reset(e, self.peer_rect));
                    // 新一次 REMOTE：返回检测复位，须等影子离开返回带后才激活（问题 A 迟滞）
                    self.remote_armed = false;
                }
                Action::Send(sig) => out.to_peer.push(signal_to_msg(sig)),
                Action::LocalReleaseAll => out.inject.push(Inject::ReleaseAll),
                Action::ShowCursorAtEntry { edge, ratio } => {
                    let p = entry_point(&self.own_rect, edge, ratio);
                    out.actions.push(a);
                    out.inject.push(Inject::MoveTo { x: p.x, y: p.y });
                }
                Action::WarpCursorReturn => {
                    self.scroll_scaler.reset();
                    // 交还控制权回本机：把光标还原到离开处（grab 已先解除，不会再被拉回中心）。
                    out.actions.push(a);
                    out.inject.push(Inject::MoveTo {
                        x: self.exit_pos.x,
                        y: self.exit_pos.y,
                    });
                }
                other => out.actions.push(other),
            }
        }
    }

    /// 处理本机捕获的输入事件。
    ///
    /// 注：曾有「连按 3 次裸 Esc 强制夺回控制」的逃生热键（开发期安全网），因严重
    /// 干扰正常 Esc 使用已按用户要求移除（T-614）。失控兜底仍有：断连安全回退
    /// （FR-605）与托盘「暂停互控」。
    pub fn feed_local(&mut self, ev: InputEvent) -> EngineOut {
        let mut out = EngineOut::default();
        match self.machine.state() {
            ControlState::Local => self.local_input(ev, &mut out),
            ControlState::Remote => self.remote_input(ev, &mut out),
            // 被控时本机物理输入忽略（避免与注入冲突）
            ControlState::Controlled => {}
        }
        out
    }

    fn local_input(&mut self, ev: InputEvent, out: &mut EngineOut) {
        if let InputEvent::MouseMove { dx, dy, ax, ay } = ev {
            // LOCAL 用绝对光标位置判边缘（比累积位移可靠）
            self.local_pos = clamp_to_rect(&self.own_rect, Point { x: ax, y: ay });
            // 从属端：不主动发起控制对端（单向主从）
            if !self.can_initiate {
                return;
            }
            let Some(info) = detect_exit(
                &self.own_rect,
                self.local_pos,
                self.layout,
                self.cfg.threshold,
                self.cfg.corner_dead,
            ) else {
                // 离开边缘带：清零蓄力，下次须重新推挤（T-511）
                self.edge_push_accum = 0.0;
                return;
            };
            // 边缘"刻意触发"（T-511，FR-205/207）：贴边后须继续朝该边累计推够
            // `edge_push_dist` 像素才穿越，避免一碰边就穿越（够不着菜单栏/任务栏）。
            // 阈值 <= 0 时退化为「贴边即穿越」（旧行为）。
            if self.edge_push_dist > 0.0 {
                self.edge_push_accum += push_toward(self.layout.peer_position, dx, dy);
                if self.edge_push_accum < self.edge_push_dist {
                    return; // 蓄力未满，光标停在边缘（可正常点菜单栏/任务栏）
                }
                self.edge_push_accum = 0.0; // 穿越，清零蓄力
            }
            let entry = entry_point(&self.peer_rect, info.entry_edge, info.ratio);
            // 记下离开点供交还时还原（朝屏内回缩，避免贴边/立即再次穿越）。
            self.exit_pos = inset_from_edge(
                &self.own_rect,
                self.local_pos,
                self.layout.peer_position,
                RETURN_INSET,
            );
            tracing::info!(
                target: "mouseshare::engine",
                edge = ?info.entry_edge,
                ratio = info.ratio,
                pos_x = self.local_pos.x,
                pos_y = self.local_pos.y,
                push = self.edge_push_dist,
                "本机光标命中边缘并完成推挤 → 进入 REMOTE 控制对端"
            );
            let acts = self.machine.on_event(ControlEvent::EnterRemote {
                edge: info.entry_edge,
                ratio: info.ratio,
            });
            self.apply_actions(acts, out, Some(entry));
        }
        // LOCAL 态键盘/按键由本机 OS 处理，不转发
    }

    fn remote_input(&mut self, ev: InputEvent, out: &mut EngineOut) {
        let Some(shadow) = self.shadow.as_mut() else {
            return;
        };
        match ev {
            InputEvent::MouseMove { dx, dy, .. } => {
                let f = shadow.apply_delta(dx, dy);
                let sp = shadow.pos();
                out.to_peer.push(Msg::from(proto::MouseMove {
                    seq: f.seq,
                    x: f.x,
                    y: f.y,
                }));
                // 影子是否回到「朝向本机的返回边」触发带（peer_rect 上反向布局判定）
                let reversed = Layout {
                    peer_position: opposite(self.layout.peer_position),
                };
                let returning =
                    detect_exit(&self.peer_rect, sp, reversed, self.cfg.threshold, 0.0).is_some();
                // 迟滞（问题 A）：入口点贴在返回边（离边 1px），若进入即检测会被瞬间
                // 弹回。故先要求影子离开返回带（armed=true），之后再进返回带才交还。
                if !self.remote_armed {
                    if !returning {
                        self.remote_armed = true;
                    }
                } else if returning {
                    // 交还"刻意触发"（T-515，与 T-511 镜像）：贴住返回边后，须继续朝该边累计
                    // 推够 `return_push_dist` 才交还，免得贴对端那条边就瞬间弹回（够不着对端
                    // 菜单栏/任务栏）。阈值 <= 0 时退化为「贴边即交还」（旧行为）。
                    if self.return_push_dist > 0.0 {
                        let return_edge = opposite(self.layout.peer_position);
                        self.return_push_accum += push_toward(return_edge, dx, dy);
                        if self.return_push_accum < self.return_push_dist {
                            return; // 蓄力未满，仍留在 REMOTE（光标停在对端返回边）
                        }
                        self.return_push_accum = 0.0; // 交还，清零蓄力
                    }
                    tracing::info!(
                        target: "mouseshare::engine",
                        shadow_x = sp.x,
                        shadow_y = sp.y,
                        push = self.return_push_dist,
                        "影子返回自身屏幕边缘并完成推挤 → 退出 REMOTE 交还控制权"
                    );
                    let acts = self.machine.on_event(ControlEvent::ExitRemote);
                    self.apply_actions(acts, out, None);
                    self.shadow = None;
                } else {
                    // 离开返回带：清零交还蓄力，下次须重新推挤（T-515）
                    self.return_push_accum = 0.0;
                }
            }
            InputEvent::Key { key, down } => {
                let s = shadow.pos();
                out.to_peer.push(Msg::from(proto::KeyEvent {
                    hid: key.0,
                    down,
                    shadow: (s.x as f32, s.y as f32),
                }));
            }
            InputEvent::Button { btn, down } => {
                let s = shadow.pos();
                out.to_peer.push(Msg::from(proto::MouseButton {
                    btn: button_code(btn),
                    down,
                    shadow: (s.x as f32, s.y as f32),
                }));
            }
            InputEvent::Scroll { dx, dy, precise } => {
                // 倍率只决定纵向滚动量，反转只决定最终符号；横向始终原样发送。
                let scaled_dy = self.scroll_scaler.scale_vertical(dy);
                let dy = if self.invert_scroll {
                    -scaled_dy
                } else {
                    scaled_dy
                };
                if dx == 0.0 && dy == 0.0 {
                    return;
                }
                out.to_peer.push(Msg::from(proto::Scroll {
                    dx: dx as f32,
                    dy: dy as f32,
                    precise,
                }));
            }
        }
    }

    /// 处理收到的对端消息。
    pub fn feed_peer(&mut self, msg: Msg) -> EngineOut {
        let mut out = EngineOut::default();
        match msg {
            Msg::EnterScreen(e) => {
                tracing::info!(
                    target: "mouseshare::engine",
                    edge = ?e.entry_edge,
                    ratio = e.ratio,
                    "收到对端 EnterScreen → 进入 CONTROLLED 被对端控制"
                );
                let acts = self.machine.on_event(ControlEvent::PeerEnter {
                    edge: e.entry_edge,
                    ratio: e.ratio,
                });
                self.gate = SeqGate::new();
                self.apply_actions(acts, &mut out, None);
            }
            Msg::LeaveScreen(_) => {
                tracing::info!(
                    target: "mouseshare::engine",
                    state = ?self.machine.state(),
                    "收到对端 LeaveScreen → 退出当前控制关系"
                );
                let acts = self.machine.on_event(ControlEvent::PeerLeave);
                self.apply_actions(acts, &mut out, None);
            }
            Msg::MouseMove(m) => {
                if self.machine.state() == ControlState::Controlled && self.gate.accept(m.seq) {
                    out.inject.push(Inject::MoveTo {
                        x: m.x as f64,
                        y: m.y as f64,
                    });
                }
            }
            Msg::KeyEvent(k) => {
                if self.machine.state() == ControlState::Controlled {
                    // 键盘注入不再强制移动光标：每键 MoveTo 会放大坐标误差、打断输入
                    // （组合键），且键盘输入与光标位置无关。修饰键 flags 由注入端维护。
                    out.inject.push(Inject::Key {
                        hid: map_modifier(self.mod_mode, k.hid),
                        down: k.down,
                    });
                }
            }
            Msg::MouseButton(b) => {
                if self.machine.state() == ControlState::Controlled {
                    out.inject.push(Inject::MoveTo {
                        x: b.shadow.0 as f64,
                        y: b.shadow.1 as f64,
                    });
                    out.inject.push(Inject::Button {
                        btn: b.btn,
                        down: b.down,
                    });
                }
            }
            Msg::Scroll(s) => {
                if self.machine.state() == ControlState::Controlled {
                    // 反转已在控制端发送时处理，被控端原样注入
                    out.inject.push(Inject::Scroll {
                        dx: s.dx,
                        dy: s.dy,
                        precise: s.precise,
                    });
                }
            }
            Msg::ReleaseAll(_) => out.inject.push(Inject::ReleaseAll),
            _ => {}
        }
        out
    }

    /// 断连：强制安全回退（FR-605）。
    pub fn on_disconnect(&mut self) -> EngineOut {
        let mut out = EngineOut::default();
        let acts = self.machine.on_event(ControlEvent::Disconnect);
        self.apply_actions(acts, &mut out, None);
        self.shadow = None;
        self.scroll_scaler.reset();
        out
    }
}

#[derive(Debug)]
struct ScrollScaler {
    factor: f64,
    residual: f64,
    last_direction: i8,
}

impl Default for ScrollScaler {
    fn default() -> Self {
        Self {
            factor: 1.0,
            residual: 0.0,
            last_direction: 0,
        }
    }
}

impl ScrollScaler {
    const EPSILON: f64 = 1e-9;

    fn normalize_factor(value: f64) -> f64 {
        if !value.is_finite() {
            return 1.0;
        }
        ((value * 10.0).round() / 10.0).clamp(1.0, 3.0)
    }

    fn set_factor(&mut self, value: f64) {
        let factor = Self::normalize_factor(value);
        if (factor - self.factor).abs() > Self::EPSILON {
            self.factor = factor;
            self.reset();
        }
    }

    fn reset(&mut self) {
        self.residual = 0.0;
        self.last_direction = 0;
    }

    fn scale_vertical(&mut self, dy: f64) -> f64 {
        if dy == 0.0 {
            return 0.0;
        }
        if !dy.is_finite() {
            self.reset();
            return dy;
        }
        let direction = if dy > 0.0 { 1 } else { -1 };
        if (self.factor - 1.0).abs() <= Self::EPSILON {
            self.residual = 0.0;
            self.last_direction = direction;
            return dy;
        }
        if self.last_direction != 0 && self.last_direction != direction {
            self.residual = 0.0;
        }
        let total = dy * self.factor + self.residual;
        if !total.is_finite() {
            self.reset();
            return dy;
        }
        let whole = if total > 0.0 {
            (total + Self::EPSILON).floor()
        } else {
            (total - Self::EPSILON).ceil()
        };
        self.residual = total - whole;
        if self.residual.abs() <= Self::EPSILON {
            self.residual = 0.0;
        }
        if !self.residual.is_finite() || self.residual.abs() >= 1.0 {
            self.reset();
            return dy;
        }
        self.last_direction = direction;
        whole
    }
}

fn signal_to_msg(sig: Signal) -> Msg {
    match sig {
        Signal::EnterScreen { edge, ratio } => Msg::from(proto::EnterScreen {
            entry_edge: edge,
            ratio,
        }),
        Signal::LeaveScreen => Msg::from(proto::LeaveScreen {}),
        Signal::ReleaseAll => Msg::from(proto::ReleaseAll {}),
    }
}

fn button_code(btn: Button) -> u8 {
    match btn {
        Button::Left => 0,
        Button::Right => 1,
        Button::Middle => 2,
        Button::X1 => 3,
        Button::X2 => 4,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]
    use super::*;
    use ms_platform::HidKey;
    use ms_proto::Edge;

    fn rect(w: f64, h: f64) -> Rect {
        Rect {
            x: 0.0,
            y: 0.0,
            w,
            h,
        }
    }

    /// A 在右、B 在左：A 把鼠标推到右边缘 → 进入 REMOTE、发 EnterScreen；
    /// B 收到后进入 Controlled；A 继续移动/按键 → B 产出对应注入命令。
    #[test]
    fn 端到端边缘切换与注入() {
        // A 的对端(B)在右；B 的对端(A)在左
        let mut a = Engine::new(
            rect(1000.0, 1000.0),
            rect(1000.0, 1000.0),
            Layout {
                peer_position: Edge::Right,
            },
            EdgeConfig {
                threshold: 2.0,
                corner_dead: 10.0,
            },
        );
        let mut b = Engine::new(
            rect(1000.0, 1000.0),
            rect(1000.0, 1000.0),
            Layout {
                peer_position: Edge::Left,
            },
            EdgeConfig {
                threshold: 2.0,
                corner_dead: 10.0,
            },
        );

        // A 把光标移到右边缘（绝对坐标 999,500）
        let out = a.feed_local(InputEvent::MouseMove {
            dx: 600.0,
            dy: 0.0,
            ax: 999.0,
            ay: 500.0,
        });
        assert_eq!(a.state(), ControlState::Remote, "A 应进入 REMOTE");
        // 应发出 EnterScreen
        let enter = out
            .to_peer
            .iter()
            .find(|m| matches!(m, Msg::EnterScreen(_)))
            .expect("应发 EnterScreen");

        // B 收到 EnterScreen
        let bout = b.feed_peer(enter.clone());
        assert_eq!(b.state(), ControlState::Controlled, "B 应被控");
        assert!(
            bout.inject
                .iter()
                .any(|i| matches!(i, Inject::MoveTo { .. })),
            "B 应把光标移到入口"
        );

        // A 向对端屏幕内侧移动（向右，离开返回边）→ 发 MouseMove → B 注入 MoveTo
        // REMOTE 用 dx/dy 累积影子（ax/ay 被忽略）
        let out = a.feed_local(InputEvent::MouseMove {
            dx: 100.0,
            dy: 50.0,
            ax: 0.0,
            ay: 0.0,
        });
        assert_eq!(a.state(), ControlState::Remote, "向内移动应仍在 REMOTE");
        let mv = out
            .to_peer
            .iter()
            .find(|m| matches!(m, Msg::MouseMove(_)))
            .expect("应发 MouseMove")
            .clone();
        let bout = b.feed_peer(mv);
        assert!(matches!(bout.inject.as_slice(), [Inject::MoveTo { .. }]));

        // A 按下 A 键 → B 仅注入 Key（不再前置 MoveTo：键盘输入与光标位置无关）
        let out = a.feed_local(InputEvent::Key {
            key: HidKey(0x04),
            down: true,
        });
        let key = out
            .to_peer
            .iter()
            .find(|m| matches!(m, Msg::KeyEvent(_)))
            .expect("应发 KeyEvent")
            .clone();
        let bout = b.feed_peer(key);
        assert!(
            matches!(
                bout.inject.as_slice(),
                [Inject::Key {
                    hid: 0x04,
                    down: true
                }]
            ),
            "被控端按键注入不再前置 MoveTo，实得 {:?}",
            bout.inject
        );
    }

    /// 交还控制权回本机时，光标应还原到「离开点」（朝屏内回缩 RETURN_INSET），
    /// 而不是停在 grab 期间被拉去的屏幕中心。复现「穿越后光标回到屏幕中心」缺陷。
    #[test]
    fn 交还时光标还原到离开点而非中心() {
        // 对端在下方（Mac 在 Windows 下面）：本机从底边离开。
        let mut a = Engine::new(
            rect(1000.0, 1000.0),
            rect(1000.0, 1000.0),
            Layout {
                peer_position: Edge::Bottom,
            },
            EdgeConfig {
                threshold: 2.0,
                corner_dead: 10.0,
            },
        );

        // 在底边横向 30% 处（300,999）穿越 → 进入 REMOTE。
        a.feed_local(InputEvent::MouseMove {
            dx: 0.0,
            dy: 600.0,
            ax: 300.0,
            ay: 999.0,
        });
        assert_eq!(a.state(), ControlState::Remote, "应进入 REMOTE");

        // 对端发 LeaveScreen 交还 → 退出 REMOTE，应注入还原 MoveTo。
        let out = a.feed_peer(Msg::from(proto::LeaveScreen {}));
        assert_eq!(a.state(), ControlState::Local, "交还后应回 LOCAL");
        let mv = out
            .inject
            .iter()
            .find_map(|i| match i {
                Inject::MoveTo { x, y } => Some((*x, *y)),
                _ => None,
            })
            .expect("交还应注入还原光标的 MoveTo");
        // 离开点 (300,999) 从底边朝屏内回缩 RETURN_INSET(20) → (300, 979)，绝非屏幕中心 (500,500)。
        assert!(
            (mv.0 - 300.0).abs() < f64::EPSILON,
            "X 应保持离开位置，实得 {}",
            mv.0
        );
        assert!(
            (mv.1 - (999.0 - RETURN_INSET)).abs() < f64::EPSILON,
            "Y 应为离开点朝屏内回缩 RETURN_INSET，实得 {}",
            mv.1
        );
    }

    /// 边缘推挤（T-511）：开启 40px 推挤后，贴边但累计推进不够阈值不穿越，推够才穿越。
    #[test]
    fn 边缘推挤_未推够不穿越推够才穿越() {
        let mut a = Engine::new(
            rect(1000.0, 1000.0),
            rect(1000.0, 1000.0),
            Layout {
                peer_position: Edge::Right,
            },
            EdgeConfig {
                threshold: 2.0,
                corner_dead: 10.0,
            },
        );
        a.set_edge_push(40.0);

        // 贴到右边缘，本次朝边推进 10px（累计 10 < 40）→ 不穿越
        let out = a.feed_local(InputEvent::MouseMove {
            dx: 10.0,
            dy: 0.0,
            ax: 999.0,
            ay: 500.0,
        });
        assert_eq!(a.state(), ControlState::Local, "推挤未够应仍在 LOCAL");
        assert!(out.to_peer.is_empty(), "未穿越不应发 EnterScreen");

        // 继续朝右推 20px（累计 30 < 40）→ 仍不穿越
        a.feed_local(InputEvent::MouseMove {
            dx: 20.0,
            dy: 0.0,
            ax: 999.0,
            ay: 500.0,
        });
        assert_eq!(a.state(), ControlState::Local);

        // 再推 15px（累计 45 >= 40）→ 穿越
        let out = a.feed_local(InputEvent::MouseMove {
            dx: 15.0,
            dy: 0.0,
            ax: 999.0,
            ay: 500.0,
        });
        assert_eq!(a.state(), ControlState::Remote, "累计推够阈值应穿越");
        assert!(
            out.to_peer.iter().any(|m| matches!(m, Msg::EnterScreen(_))),
            "穿越应发 EnterScreen"
        );
    }

    /// 边缘推挤（T-511）：贴边推一半后离开边缘带，蓄力应清零，重新贴边须从头推。
    #[test]
    fn 边缘推挤_离开边缘带清零蓄力() {
        let mut a = Engine::new(
            rect(1000.0, 1000.0),
            rect(1000.0, 1000.0),
            Layout {
                peer_position: Edge::Right,
            },
            EdgeConfig {
                threshold: 2.0,
                corner_dead: 10.0,
            },
        );
        a.set_edge_push(40.0);

        // 贴边推 30px（不够）
        a.feed_local(InputEvent::MouseMove {
            dx: 30.0,
            dy: 0.0,
            ax: 999.0,
            ay: 500.0,
        });
        assert_eq!(a.state(), ControlState::Local);
        // 离开边缘带（回屏幕中间）→ 蓄力清零
        a.feed_local(InputEvent::MouseMove {
            dx: -499.0,
            dy: 0.0,
            ax: 500.0,
            ay: 500.0,
        });
        // 再回边缘只推 30px（若没清零 30+30=60 会误穿）→ 应仍不穿越
        a.feed_local(InputEvent::MouseMove {
            dx: 30.0,
            dy: 0.0,
            ax: 999.0,
            ay: 500.0,
        });
        assert_eq!(
            a.state(),
            ControlState::Local,
            "离开边缘带应清零蓄力，须重新推满阈值"
        );
    }

    /// 边缘推挤（T-511）：贴边但鼠标沿/背离边缘移动（朝边分量为 0）不累积，永不误穿。
    #[test]
    fn 边缘推挤_背离方向不蓄力() {
        let mut a = Engine::new(
            rect(1000.0, 1000.0),
            rect(1000.0, 1000.0),
            Layout {
                peer_position: Edge::Right,
            },
            EdgeConfig {
                threshold: 2.0,
                corner_dead: 10.0,
            },
        );
        a.set_edge_push(40.0);
        // 贴右边缘但鼠标朝左/纵向移动（dx<=0）→ push_toward=0，多次也不穿越
        for _ in 0..20 {
            a.feed_local(InputEvent::MouseMove {
                dx: -5.0,
                dy: 3.0,
                ax: 999.0,
                ay: 500.0,
            });
        }
        assert_eq!(
            a.state(),
            ControlState::Local,
            "背离边缘方向不应累积致误穿越"
        );
    }

    /// 交还推挤（T-515）：控制端开启 60px 交还推挤后，影子回到返回边但累计推进不够
    /// 不交还，推够才交还（避免贴对端那条边就瞬间弹回，够不着对端菜单栏）。
    #[test]
    fn 交还推挤_未推够不交还推够才交还() {
        let mut a = Engine::new(
            rect(1000.0, 1000.0),
            rect(1000.0, 1000.0),
            Layout {
                peer_position: Edge::Right,
            },
            EdgeConfig {
                threshold: 2.0,
                corner_dead: 10.0,
            },
        );
        a.set_return_push(60.0);
        // 进入 REMOTE（贴右边缘穿越，edge_push 默认 0 即贴边穿越），影子入口在对端左边（返回边）
        a.feed_local(InputEvent::MouseMove {
            dx: 600.0,
            dy: 0.0,
            ax: 999.0,
            ay: 500.0,
        });
        assert_eq!(a.state(), ControlState::Remote, "应进入 REMOTE 控制对端");
        // 向右移开返回带 → arm（迟滞要求先离开返回带）
        a.feed_local(InputEvent::MouseMove {
            dx: 500.0,
            dy: 0.0,
            ax: 0.0,
            ay: 0.0,
        });
        assert_eq!(a.state(), ControlState::Remote);
        // 移到返回边附近但未进返回带（x≈4 > threshold 2）→ 不交还、蓄力清零
        a.feed_local(InputEvent::MouseMove {
            dx: -497.0,
            dy: 0.0,
            ax: 0.0,
            ay: 0.0,
        });
        assert_eq!(a.state(), ControlState::Remote);
        // 进返回带，朝返回边推 2px（累计 2 < 60）→ 不交还
        a.feed_local(InputEvent::MouseMove {
            dx: -2.0,
            dy: 0.0,
            ax: 0.0,
            ay: 0.0,
        });
        assert_eq!(a.state(), ControlState::Remote, "推挤未够应仍在 REMOTE");
        // 继续推 30px（累计 32 < 60）→ 仍不交还
        a.feed_local(InputEvent::MouseMove {
            dx: -30.0,
            dy: 0.0,
            ax: 0.0,
            ay: 0.0,
        });
        assert_eq!(a.state(), ControlState::Remote);
        // 再推 30px（累计 62 >= 60）→ 交还，回 LOCAL
        a.feed_local(InputEvent::MouseMove {
            dx: -30.0,
            dy: 0.0,
            ax: 0.0,
            ay: 0.0,
        });
        assert_eq!(
            a.state(),
            ControlState::Local,
            "累计推够阈值应交还控制权回 LOCAL"
        );
    }

    #[test]
    fn 功能映射模式被控端注入时交换ctrl_gui() {
        let mut b = Engine::new(
            rect(1000.0, 1000.0),
            rect(1000.0, 1000.0),
            Layout {
                peer_position: Edge::Left,
            },
            EdgeConfig::default(),
        );
        b.set_modifier_mode(ModifierMode::SwapCtrlGui);
        b.feed_peer(Msg::from(proto::EnterScreen {
            entry_edge: Edge::Left,
            ratio: 0.5,
        }));
        // 对端发来 LCtrl(0xE0) 按下 → 注入应映射为 LGUI(0xE3)
        let out = b.feed_peer(Msg::from(proto::KeyEvent {
            hid: 0xE0,
            down: true,
            shadow: (10.0, 10.0),
        }));
        assert!(
            out.inject.contains(&Inject::Key {
                hid: 0xE3,
                down: true
            }),
            "SwapCtrlGui 模式应把 Ctrl 注入为 GUI，实得 {:?}",
            out.inject
        );
    }

    #[test]
    fn 断连时被控端释放注入键() {
        let mut b = Engine::new(
            rect(1000.0, 1000.0),
            rect(1000.0, 1000.0),
            Layout {
                peer_position: Edge::Left,
            },
            EdgeConfig::default(),
        );
        b.feed_peer(Msg::from(proto::EnterScreen {
            entry_edge: Edge::Left,
            ratio: 0.5,
        }));
        assert_eq!(b.state(), ControlState::Controlled);
        let out = b.on_disconnect();
        assert_eq!(b.state(), ControlState::Local);
        assert!(out.inject.contains(&Inject::ReleaseAll), "断连必释放注入键");
    }

    fn controlled_engine() -> Engine {
        let mut e = Engine::new(
            rect(1000.0, 1000.0),
            rect(1000.0, 1000.0),
            Layout {
                peer_position: Edge::Left,
            },
            EdgeConfig::default(),
        );
        e.feed_peer(Msg::from(proto::EnterScreen {
            entry_edge: Edge::Left,
            ratio: 0.5,
        }));
        assert_eq!(e.state(), ControlState::Controlled);
        e
    }

    fn key(hid: u16, down: bool) -> InputEvent {
        InputEvent::Key {
            key: HidKey(hid),
            down,
        }
    }

    #[test]
    fn 迟滞_入口贴边不被瞬间弹回_离带后回边才交还() {
        let mut a = Engine::new(
            rect(1000.0, 1000.0),
            rect(1000.0, 1000.0),
            Layout {
                peer_position: Edge::Right,
            },
            EdgeConfig {
                threshold: 2.0,
                corner_dead: 10.0,
            },
        );
        // 推到右边缘进入 REMOTE：影子从对端左边(x≈1)进入，正贴返回带
        a.feed_local(InputEvent::MouseMove {
            dx: 0.0,
            dy: 0.0,
            ax: 999.0,
            ay: 500.0,
        });
        assert_eq!(a.state(), ControlState::Remote);
        // 进入后微动/不向内移：影子仍贴返回边，但迟滞未 arm，不应被弹回
        a.feed_local(InputEvent::MouseMove {
            dx: 0.0,
            dy: 0.0,
            ax: 0.0,
            ay: 0.0,
        });
        assert_eq!(
            a.state(),
            ControlState::Remote,
            "入口贴返回边，不应瞬间弹回（问题 A）"
        );
        // 向对端内部移动 → 离开返回带，激活 armed
        a.feed_local(InputEvent::MouseMove {
            dx: 500.0,
            dy: 0.0,
            ax: 0.0,
            ay: 0.0,
        });
        assert_eq!(a.state(), ControlState::Remote);
        // 再大幅移回返回边 → 这次才真正交还控制
        a.feed_local(InputEvent::MouseMove {
            dx: -600.0,
            dy: 0.0,
            ax: 0.0,
            ay: 0.0,
        });
        assert_eq!(
            a.state(),
            ControlState::Local,
            "离开返回带后再回到边缘才交还控制"
        );
    }

    #[test]
    fn 从属端光标到边缘不主动发起控制() {
        let mut a = Engine::new(
            rect(1000.0, 1000.0),
            rect(1000.0, 1000.0),
            Layout {
                peer_position: Edge::Right,
            },
            EdgeConfig {
                threshold: 2.0,
                corner_dead: 10.0,
            },
        );
        a.set_can_initiate(false);
        let out = a.feed_local(InputEvent::MouseMove {
            dx: 0.0,
            dy: 0.0,
            ax: 999.0,
            ay: 500.0,
        });
        assert_eq!(
            a.state(),
            ControlState::Local,
            "从属端光标到边缘不应发起控制"
        );
        assert!(out.to_peer.is_empty(), "从属端不应发 EnterScreen");
    }

    fn remote_engine_invert(invert: bool) -> Engine {
        let mut a = Engine::new(
            rect(1000.0, 1000.0),
            rect(1000.0, 1000.0),
            Layout {
                peer_position: Edge::Right,
            },
            EdgeConfig {
                threshold: 2.0,
                corner_dead: 10.0,
            },
        );
        a.set_invert_scroll(invert);
        // 进入 REMOTE（控制端）
        a.feed_local(InputEvent::MouseMove {
            dx: 0.0,
            dy: 0.0,
            ax: 999.0,
            ay: 500.0,
        });
        assert_eq!(a.state(), ControlState::Remote);
        a
    }

    fn sent_scroll_dy(out: &EngineOut) -> f32 {
        out.to_peer
            .iter()
            .find_map(|m| {
                if let Msg::Scroll(s) = m {
                    Some(s.dy)
                } else {
                    None
                }
            })
            .expect("应发 Scroll")
    }

    #[test]
    fn 滚轮反转在控制端发送时翻转纵向() {
        // 勾选反转：控制端发送的纵向取反、横向不变
        let mut a = remote_engine_invert(true);
        let out = a.feed_local(InputEvent::Scroll {
            dx: 1.0,
            dy: 5.0,
            precise: false,
        });
        assert_eq!(sent_scroll_dy(&out), -5.0, "勾选反转：纵向应取反");

        // 未勾选：原样发送
        let mut b = remote_engine_invert(false);
        let out = b.feed_local(InputEvent::Scroll {
            dx: 1.0,
            dy: 5.0,
            precise: false,
        });
        assert_eq!(sent_scroll_dy(&out), 5.0, "未勾选：不反转");
    }

    #[test]
    fn 滚轮倍率各档位累计有效且横向不变() {
        for step in 10..=30 {
            let factor = f64::from(step) / 10.0;
            let mut a = remote_engine_invert(false);
            a.set_scroll_speed(factor);
            let mut sum = 0.0_f32;
            for _ in 0..10 {
                let out = a.feed_local(InputEvent::Scroll {
                    dx: 0.25,
                    dy: 1.0,
                    precise: true,
                });
                let scroll = out.to_peer.iter().find_map(|m| match m {
                    Msg::Scroll(s) => Some(s),
                    _ => None,
                });
                let scroll = scroll.expect("每次普通滚轮输入都应立即输出");
                assert_eq!(scroll.dx, 0.25, "横向不得应用倍率");
                assert!(scroll.precise, "precise 标志应保持");
                sum += scroll.dy;
            }
            assert_eq!(sum, step as f32, "{factor:.1}× 累计量错误");
        }
    }

    #[test]
    fn 滚轮倍率换向与热切换清除余量且反转独立() {
        let mut a = remote_engine_invert(false);
        a.set_scroll_speed(1.5);
        let first = a.feed_local(InputEvent::Scroll {
            dx: 0.0,
            dy: 1.0,
            precise: false,
        });
        assert_eq!(sent_scroll_dy(&first), 1.0);
        let reversed = a.feed_local(InputEvent::Scroll {
            dx: 0.0,
            dy: -1.0,
            precise: false,
        });
        assert_eq!(sent_scroll_dy(&reversed), -1.0, "换向不应继承正向余量");

        a.set_scroll_speed(1.1);
        let old_factor = a.feed_local(InputEvent::Scroll {
            dx: 0.0,
            dy: 1.0,
            precise: false,
        });
        assert_eq!(sent_scroll_dy(&old_factor), 1.0);
        a.set_scroll_speed(3.0);
        a.set_invert_scroll(true);
        let combined = a.feed_local(InputEvent::Scroll {
            dx: 0.0,
            dy: 1.0,
            precise: false,
        });
        assert_eq!(
            sent_scroll_dy(&combined),
            -3.0,
            "倍率决定量、反转只决定符号"
        );
    }
}
