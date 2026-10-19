//! 屏幕布局与坐标映射（DEVELOPMENT.md §4.3，FR-201~208）。
//!
//! 每端把自己的多显示器并集抽象为虚拟屏幕矩形（左上原点、物理像素，SPEC §8）。
//! 切换时只传 `(entry_edge, ratio)`：ratio 是离开点在源屏该边上的归一化位置
//! （0.0–1.0），目标端按自己边长换算进入坐标——天然解决分辨率/DPI 差异（FR-203）。

use ms_platform::Rect;
use ms_proto::{Edge, Layout};

/// 坐标点（左上原点，物理像素）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// 边缘检测结果：对端入口边（= 本端离开边的对侧）与比例。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExitInfo {
    pub entry_edge: Edge,
    pub ratio: f32,
}

/// 非零沿边偏移以来源边起点为0、目标顶/左为offset；仅重叠段可以跨屏。
/// offset=0的旧比例映射由调用方保留，不改变已有配置手感。
pub fn offset_entry_ratio(
    own: &Rect,
    peer: &Rect,
    pos: Point,
    side: Edge,
    offset: f64,
) -> Option<f32> {
    let (along, own_len, peer_len) = match side {
        Edge::Left | Edge::Right => (pos.y - own.y, own.h, peer.h),
        Edge::Top | Edge::Bottom => (pos.x - own.x, own.w, peer.w),
    };
    if !along.is_finite()
        || !offset.is_finite()
        || !peer_len.is_finite()
        || !own_len.is_finite()
        || peer_len <= 0.0
        || own_len <= 0.0
    {
        return None;
    }
    let target = along - offset;
    (along >= 0.0 && along < own_len && target >= 0.0 && target < peer_len)
        .then_some((target / peer_len) as f32)
}

/// 边的对侧。
pub fn opposite(edge: Edge) -> Edge {
    match edge {
        Edge::Left => Edge::Right,
        Edge::Right => Edge::Left,
        Edge::Top => Edge::Bottom,
        Edge::Bottom => Edge::Top,
    }
}

/// 一次相对位移 `(dx, dy)` 中"朝向 `edge` 方向"的分量（像素，仅取正向，背离边缘为 0）。
///
/// 用于边缘"推挤穿越"（T-511，FR-205/207）：贴边后须继续朝该边推够阈值距离才穿越，
/// 避免一碰边就穿越导致够不着系统菜单栏（mac 顶部）/任务栏（Windows 底部）。
pub fn push_toward(edge: Edge, dx: f64, dy: f64) -> f64 {
    let v = match edge {
        Edge::Right => dx,
        Edge::Left => -dx,
        Edge::Top => -dy,
        Edge::Bottom => dy,
    };
    v.max(0.0)
}

fn right(r: &Rect) -> f64 {
    r.x + r.w
}
fn bottom(r: &Rect) -> f64 {
    r.y + r.h
}

fn clamp01(v: f64) -> f32 {
    v.clamp(0.0, 1.0) as f32
}

/// 检测本机光标是否命中"朝向对端那条边"，命中则返回切换信息。
///
/// - `edge_threshold`：边缘触发带宽（像素），光标进入该带宽即判定命中。
/// - `corner_dead`：四角死区边长（像素），靠近任一角不触发（FR-208，避免误触
///   触发角/任务视图）。
pub fn detect_exit(
    rect: &Rect,
    pos: Point,
    layout: Layout,
    edge_threshold: f64,
    corner_dead: f64,
) -> Option<ExitInfo> {
    let exit_edge = layout.peer_position;

    // 是否贴住该边
    let at_edge = match exit_edge {
        Edge::Right => pos.x >= right(rect) - edge_threshold,
        Edge::Left => pos.x <= rect.x + edge_threshold,
        Edge::Top => pos.y <= rect.y + edge_threshold,
        Edge::Bottom => pos.y >= bottom(rect) - edge_threshold,
    };
    if !at_edge {
        return None;
    }

    // 四角死区：靠近任一角不触发
    let near_x_corner = pos.x <= rect.x + corner_dead || pos.x >= right(rect) - corner_dead;
    let near_y_corner = pos.y <= rect.y + corner_dead || pos.y >= bottom(rect) - corner_dead;
    if near_x_corner && near_y_corner {
        return None;
    }

    // 沿边比例
    let ratio = match exit_edge {
        Edge::Left | Edge::Right => clamp01((pos.y - rect.y) / rect.h),
        Edge::Top | Edge::Bottom => clamp01((pos.x - rect.x) / rect.w),
    };

    Some(ExitInfo {
        entry_edge: opposite(exit_edge),
        ratio,
    })
}

/// 把收到的 `(entry_edge, ratio)` 映射为本机虚拟屏幕内的进入坐标。
/// 进入点离边 1px，避免立刻又触发对侧边缘判定。
pub fn entry_point(rect: &Rect, entry_edge: Edge, ratio: f32) -> Point {
    let r = ratio.clamp(0.0, 1.0) as f64;
    match entry_edge {
        Edge::Left => Point {
            x: rect.x + 1.0,
            y: rect.y + r * rect.h,
        },
        Edge::Right => Point {
            x: right(rect) - 1.0,
            y: rect.y + r * rect.h,
        },
        Edge::Top => Point {
            x: rect.x + r * rect.w,
            y: rect.y + 1.0,
        },
        Edge::Bottom => Point {
            x: rect.x + r * rect.w,
            y: bottom(rect) - 1.0,
        },
    }
}

/// 把 `pos` 从 `edge` 这条边朝屏幕内侧回缩 `inset` 像素，并 clamp 到矩形内。
///
/// 用于交还控制权时还原本机光标：穿越离开点正贴在边缘上，直接还原会贴边且易再次触发
/// 穿越；先朝内回缩一小段更稳。`edge` 是离开时朝向的那条边（= 对端方位）。
pub fn inset_from_edge(rect: &Rect, pos: Point, edge: Edge, inset: f64) -> Point {
    let p = match edge {
        Edge::Right => Point {
            x: pos.x - inset,
            y: pos.y,
        },
        Edge::Left => Point {
            x: pos.x + inset,
            y: pos.y,
        },
        Edge::Top => Point {
            x: pos.x,
            y: pos.y + inset,
        },
        Edge::Bottom => Point {
            x: pos.x,
            y: pos.y - inset,
        },
    };
    clamp_to_rect(rect, p)
}

/// 把坐标 clamp 到虚拟屏幕内（影子光标累积时用，TECH-REVIEW A-4）。
///
/// 上界取 `w-1`/`h-1` 而非 `w`/`h`：屏幕最后一个像素是 w-1/h-1，钳到 w/h 差一越界。
/// 被控 mac 的 WindowServer 对越界坐标的事件**不做边缘判定**——本机注入实验证实
/// 贴底边 y=900 恒唤不出自动隐藏的程序坞、y=899 恒成功，此差一即「Dock 时好时坏」
/// 的根因（T-616；T-610 的 delta/warp 假设均与此无关）。
pub fn clamp_to_rect(rect: &Rect, pos: Point) -> Point {
    Point {
        x: pos.x.clamp(rect.x, (right(rect) - 1.0).max(rect.x)),
        y: pos.y.clamp(rect.y, (bottom(rect) - 1.0).max(rect.y)),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn rect(w: f64, h: f64) -> Rect {
        Rect {
            x: 0.0,
            y: 0.0,
            w,
            h,
        }
    }

    fn layout(peer: Edge) -> Layout {
        Layout {
            peer_position: peer,
        }
    }

    #[test]
    fn 右边缘命中返回左入口与纵向比例() {
        let r = rect(2560.0, 1440.0);
        let pos = Point {
            x: 2559.0,
            y: 720.0,
        }; // 贴右边，纵向中点
        let info = detect_exit(&r, pos, layout(Edge::Right), 2.0, 50.0).unwrap();
        assert_eq!(info.entry_edge, Edge::Left);
        assert!(
            (info.ratio - 0.5).abs() < 0.01,
            "纵向中点 ratio≈0.5，实得 {}",
            info.ratio
        );
    }

    #[test]
    fn 未贴边不触发() {
        let r = rect(2560.0, 1440.0);
        let pos = Point {
            x: 1000.0,
            y: 720.0,
        };
        assert!(detect_exit(&r, pos, layout(Edge::Right), 2.0, 50.0).is_none());
    }

    #[test]
    fn 四角死区不触发() {
        let r = rect(2560.0, 1440.0);
        // 贴右边但在右上角死区内
        let pos = Point { x: 2559.0, y: 10.0 };
        assert!(detect_exit(&r, pos, layout(Edge::Right), 2.0, 50.0).is_none());
    }

    #[test]
    fn 朝向对端以外的边不触发() {
        let r = rect(2560.0, 1440.0);
        // 贴左边，但对端在右 → 不触发
        let pos = Point { x: 1.0, y: 720.0 };
        assert!(detect_exit(&r, pos, layout(Edge::Right), 2.0, 50.0).is_none());
    }

    /// DPI/分辨率矩阵：源端某比例离开，目标端不同尺寸下按比例进入，纵向比例守恒。
    #[test]
    fn dpi分辨率矩阵_比例守恒() {
        // 源屏尺寸 × 目标屏尺寸（模拟 100/125/150/200% 缩放下的物理像素）
        let sizes = [
            (1920.0, 1080.0),
            (2400.0, 1350.0),
            (2880.0, 1620.0),
            (3840.0, 2160.0),
        ];
        for (sw, sh) in sizes {
            for (tw, th) in sizes {
                let src = rect(sw, sh);
                let dst = rect(tw, th);
                // 源端在右边 40% 高度处离开
                let pos = Point {
                    x: sw - 1.0,
                    y: sh * 0.4,
                };
                let info = detect_exit(&src, pos, layout(Edge::Right), 2.0, 1.0).unwrap();
                let entry = entry_point(&dst, info.entry_edge, info.ratio);
                // 目标端进入点纵向比例应仍≈0.4（与目标尺寸无关）
                let entry_ratio = entry.y / th;
                assert!(
                    (entry_ratio - 0.4).abs() < 0.01,
                    "src {sw}x{sh} → dst {tw}x{th}: 入口比例 {entry_ratio} 偏离 0.4"
                );
                // 进入点在左边内侧
                assert!((entry.x - 1.0).abs() < f64::EPSILON);
            }
        }
    }

    #[test]
    fn clamp约束到矩形内() {
        let r = Rect {
            x: 100.0,
            y: 100.0,
            w: 800.0,
            h: 600.0,
        };
        assert_eq!(
            clamp_to_rect(
                &r,
                Point {
                    x: -50.0,
                    y: 9999.0
                }
            ),
            // 上界是屏内最后像素 h-1（T-616：clamp 到 h 差一越界，Dock 唤不出）
            Point { x: 100.0, y: 699.0 }
        );
    }

    #[test]
    fn 推挤分量只取朝边正向() {
        // 朝向边缘（正向）按位移大小计；背离边缘记 0；正交分量不影响。
        assert_eq!(push_toward(Edge::Right, 5.0, 3.0), 5.0);
        assert_eq!(push_toward(Edge::Right, -5.0, 0.0), 0.0);
        assert_eq!(push_toward(Edge::Left, -4.0, 2.0), 4.0);
        assert_eq!(push_toward(Edge::Left, 4.0, 0.0), 0.0);
        assert_eq!(push_toward(Edge::Top, 1.0, -6.0), 6.0);
        assert_eq!(push_toward(Edge::Top, 0.0, 6.0), 0.0);
        assert_eq!(push_toward(Edge::Bottom, 0.0, 7.0), 7.0);
        assert_eq!(push_toward(Edge::Bottom, 0.0, -7.0), 0.0);
    }

    #[test]
    fn 上下边走横向比例() {
        let r = rect(1000.0, 800.0);
        let pos = Point { x: 250.0, y: 799.0 }; // 贴底边，横向 25%
        let info = detect_exit(&r, pos, layout(Edge::Bottom), 2.0, 10.0).unwrap();
        assert_eq!(info.entry_edge, Edge::Top);
        assert!((info.ratio - 0.25).abs() < 0.01);
    }
}
