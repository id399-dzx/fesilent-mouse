//! 影子光标模型（TECH-REVIEW A-4，DEVELOPMENT.md §4.3）。
//!
//! REMOTE 期间，控制端用本机原始相对位移累积维护一个**影子光标**（被控端虚拟屏
//! 坐标系，控制端做 clamp），Datagram 发送绝对坐标 `MouseMove{seq, x, y}`。
//! 被控端按 seq 丢弃乱序帧——丢包只丢中间轨迹点，最终位置永远正确，无累积漂移。

use ms_platform::Rect;

use crate::layout::{Point, clamp_to_rect};

/// 控制端影子光标：累积相对位移为被控端绝对坐标，自增序号。
#[derive(Debug)]
pub struct ShadowCursor {
    pos: Point,
    seq: u32,
    peer_rect: Rect,
}

/// 一帧鼠标移动（绝对影子坐标，走 Datagram）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MoveFrame {
    pub seq: u32,
    pub x: f32,
    pub y: f32,
}

impl ShadowCursor {
    /// 进入 REMOTE 时重置到入口坐标，序号清零（与 ResetDeltaRef 对应）。
    pub fn reset(entry: Point, peer_rect: Rect) -> Self {
        Self {
            pos: clamp_to_rect(&peer_rect, entry),
            seq: 0,
            peer_rect,
        }
    }

    /// 累积一次相对位移，clamp 到被控端虚拟屏，自增序号，返回要发送的帧。
    pub fn apply_delta(&mut self, dx: f64, dy: f64) -> MoveFrame {
        self.pos = clamp_to_rect(
            &self.peer_rect,
            Point {
                x: self.pos.x + dx,
                y: self.pos.y + dy,
            },
        );
        self.seq = self.seq.wrapping_add(1);
        MoveFrame {
            seq: self.seq,
            x: self.pos.x as f32,
            y: self.pos.y as f32,
        }
    }

    /// 当前影子位置（控制端用于边缘判定 layout::detect_exit）。
    pub fn pos(&self) -> Point {
        self.pos
    }

    pub fn seq(&self) -> u32 {
        self.seq
    }
}

/// 被控端序号门：丢弃乱序/重复帧（只接受更新的 seq）。
#[derive(Debug, Default)]
pub struct SeqGate {
    last: Option<u32>,
}

impl SeqGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// 是否接受该 seq。接受则更新 last 并返回 true；旧/重复帧返回 false。
    /// 用环形序号比较（RFC 1982 思路）处理 u32 回绕。
    pub fn accept(&mut self, seq: u32) -> bool {
        let ok = match self.last {
            None => true,
            Some(last) => is_newer(seq, last),
        };
        if ok {
            self.last = Some(seq);
        }
        ok
    }

    pub fn last(&self) -> Option<u32> {
        self.last
    }
}

/// 环形比较：`seq` 是否比 `last` 新（处理回绕，半程窗口）。
fn is_newer(seq: u32, last: u32) -> bool {
    seq != last && seq.wrapping_sub(last) < 0x8000_0000
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer() -> Rect {
        Rect {
            x: 0.0,
            y: 0.0,
            w: 1920.0,
            h: 1080.0,
        }
    }

    #[test]
    fn 累积位移并自增序号() {
        let mut s = ShadowCursor::reset(Point { x: 100.0, y: 100.0 }, peer());
        let f1 = s.apply_delta(10.0, 5.0);
        assert_eq!((f1.seq, f1.x, f1.y), (1, 110.0, 105.0));
        let f2 = s.apply_delta(-20.0, 0.0);
        assert_eq!((f2.seq, f2.x, f2.y), (2, 90.0, 105.0));
    }

    #[test]
    fn 越界被clamp到虚拟屏() {
        let mut s = ShadowCursor::reset(Point { x: 10.0, y: 10.0 }, peer());
        let f = s.apply_delta(-9999.0, -9999.0);
        assert_eq!((f.x, f.y), (0.0, 0.0));
        let f = s.apply_delta(99999.0, 99999.0);
        // 钉边在屏内最后像素 w-1/h-1（T-616：1920/1080 差一越界，被控端边缘 UI 判定失效）
        assert_eq!((f.x, f.y), (1919.0, 1079.0));
    }

    #[test]
    fn 序号门丢弃乱序与重复() {
        let mut g = SeqGate::new();
        assert!(g.accept(1));
        assert!(g.accept(2));
        assert!(!g.accept(2), "重复 seq 丢弃");
        assert!(!g.accept(1), "更旧 seq 丢弃");
        assert!(g.accept(5), "跳号但更新，接受");
    }

    #[test]
    fn 序号回绕处理() {
        let mut g = SeqGate::new();
        assert!(g.accept(u32::MAX - 1));
        assert!(g.accept(u32::MAX));
        assert!(g.accept(0), "回绕后 0 比 MAX 新");
        assert!(g.accept(1));
        assert!(!g.accept(u32::MAX), "回绕前的旧值丢弃");
    }

    /// 核心不变量：丢中间帧后，最终位置 = 完整序列的最终位置（零漂移）。
    #[test]
    fn 丢包乱序后最终位置零漂移() {
        let deltas = [
            (5.0, 3.0),
            (10.0, -2.0),
            (-4.0, 8.0),
            (20.0, 20.0),
            (-7.0, -1.0),
        ];

        // 控制端：完整产生所有帧
        let mut s = ShadowCursor::reset(Point { x: 500.0, y: 500.0 }, peer());
        let frames: Vec<MoveFrame> = deltas
            .iter()
            .map(|(dx, dy)| s.apply_delta(*dx, *dy))
            .collect();
        let final_pos = s.pos();

        // 被控端：模拟丢失中间帧（只收到第 2、5 帧）+ 一个乱序重放（第 3 帧迟到）
        let mut gate = SeqGate::new();
        let mut injected = Point { x: 500.0, y: 500.0 };
        let arrival = [frames[1], frames[4], frames[2]]; // 丢 0、3；末帧最新；旧帧迟到
        for f in arrival {
            if gate.accept(f.seq) {
                injected = Point {
                    x: f.x as f64,
                    y: f.y as f64,
                };
            }
        }
        // 最终注入位置应等于控制端最终影子位置（迟到的旧帧被丢，不回退）
        assert_eq!(injected.x, final_pos.x);
        assert_eq!(injected.y, final_pos.y);
    }

    #[test]
    fn 按键冗余坐标可校正位置() {
        // 模拟按键事件携带的影子坐标用于注入前校正（DEVELOPMENT §4.3）
        let mut s = ShadowCursor::reset(Point { x: 0.0, y: 0.0 }, peer());
        s.apply_delta(300.0, 200.0);
        let shadow = s.pos();
        // 按键消息冗余携带 shadow，被控端注入按键前先 move 到该处
        assert_eq!(shadow, Point { x: 300.0, y: 200.0 });
    }
}
