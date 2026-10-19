//! 工作空间的四向布局与拖拽判定。与窗口绘制分开，避免显示和真实配置方向脱节。

use egui::{Pos2, Rect, Vec2};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Side {
    Left,
    Top,
    Right,
    Bottom,
}

impl Side {
    pub(super) const ALL: [Self; 4] = [Self::Left, Self::Top, Self::Right, Self::Bottom];

    pub(super) fn from_config(value: &str) -> Self {
        match value {
            "left" => Self::Left,
            "top" => Self::Top,
            "bottom" => Self::Bottom,
            _ => Self::Right,
        }
    }

    pub(super) fn key(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Top => "top",
            Self::Right => "right",
            Self::Bottom => "bottom",
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Left => "左",
            Self::Top => "上",
            Self::Right => "右",
            Self::Bottom => "下",
        }
    }

    pub(super) fn opposite(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Top => Self::Bottom,
            Self::Right => Self::Left,
            Self::Bottom => Self::Top,
        }
    }

    pub(super) fn vertical(self) -> bool {
        matches!(self, Self::Top | Self::Bottom)
    }

    fn vector(self) -> Vec2 {
        match self {
            Self::Left => Vec2::new(-1.0, 0.0),
            Self::Top => Vec2::new(0.0, -1.0),
            Self::Right => Vec2::new(1.0, 0.0),
            Self::Bottom => Vec2::new(0.0, 1.0),
        }
    }
}

/// 指针离本机屏幕中心过近时保持原方向，避免单击或拖到中心时意外切换。
pub(super) fn snapped_side(local_center: Pos2, pointer: Pos2, previous: Side) -> Side {
    let delta = pointer - local_center;
    if delta.length_sq() < 28.0 * 28.0 {
        return previous;
    }
    if delta.x.abs() >= delta.y.abs() {
        if delta.x < 0.0 {
            Side::Left
        } else {
            Side::Right
        }
    } else if delta.y < 0.0 {
        Side::Top
    } else {
        Side::Bottom
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct DeviceRects {
    pub windows: Rect,
    pub mac: Rect,
}

/// 矩形是屏幕面板本身，底部额外保留 54px 放设备名、角色和支架。
pub(super) fn device_rects(canvas: Rect, side: Side) -> DeviceRects {
    let width = (canvas.width() * 0.29).clamp(112.0, 172.0);
    let size = Vec2::new(width, 96.0);
    let distance = if side.vertical() {
        93.0
    } else {
        width / 2.0 + 32.0
    };
    let center = canvas.center() - Vec2::new(0.0, 18.0);
    let offset = side.vector() * distance;
    DeviceRects {
        windows: Rect::from_center_size(center - offset, size),
        mac: Rect::from_center_size(center + offset, size),
    }
}

pub(super) fn crossing_segment(screen: Rect, side: Side) -> [Pos2; 2] {
    let center = match side {
        Side::Left => screen.left_center(),
        Side::Top => screen.center_top(),
        Side::Right => screen.right_center(),
        Side::Bottom => screen.center_bottom(),
    };
    let offset = if side.vertical() {
        Vec2::new(18.0, 0.0)
    } else {
        Vec2::new(0.0, 18.0)
    };
    [center - offset, center + offset]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 拖拽按主要轴吸附四向且中心保持原方向() {
        let origin = Pos2::new(100.0, 100.0);
        for (pointer, expected) in [
            (Pos2::new(10.0, 130.0), Side::Left),
            (Pos2::new(180.0, 60.0), Side::Right),
            (Pos2::new(90.0, 10.0), Side::Top),
            (Pos2::new(120.0, 190.0), Side::Bottom),
        ] {
            assert_eq!(snapped_side(origin, pointer, Side::Right), expected);
        }
        assert_eq!(
            snapped_side(origin, Pos2::new(110.0, 110.0), Side::Left),
            Side::Left
        );
        assert_eq!(
            snapped_side(origin, Pos2::new(128.0, 100.0), Side::Left),
            Side::Right
        );
    }

    #[test]
    fn 各窗宽下设备不重叠且配置方向一致() {
        for width in [420.0, 632.0, 920.0] {
            for side in Side::ALL {
                let height = if side.vertical() { 360.0 } else { 244.0 };
                let canvas = Rect::from_min_size(Pos2::new(30.0, 50.0), Vec2::new(width, height));
                let rects = device_rects(canvas, side);
                assert!(canvas.contains_rect(rects.windows));
                assert!(canvas.contains_rect(rects.mac));
                assert!(!rects.windows.intersects(rects.mac));
                for screen in [rects.windows, rects.mac] {
                    assert!(canvas.contains(screen.center_bottom() + Vec2::new(0.0, 53.0)));
                }
                assert_eq!(
                    snapped_side(rects.windows.center(), rects.mac.center(), side),
                    side
                );
                let edge = crossing_segment(rects.windows, side);
                for point in edge {
                    assert!(rects.windows.contains(point));
                }
                assert_eq!(side.opposite().opposite(), side);
            }
        }
    }
}
