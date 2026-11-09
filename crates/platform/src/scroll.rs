//! 系统滚动 API 的整数边界适配：精密滚动保留每轴小数余量，避免慢滑事件被逐帧舍弃。
//! 不改变线路单位；macOS 的像素/行和 Windows 的 WHEEL_DELTA 由调用方换算。

#[derive(Debug, Default)]
struct Axis {
    residual: f64,
    direction: i8,
}

impl Axis {
    fn quantize(&mut self, delta: f64, precise: bool) -> i32 {
        if !delta.is_finite() {
            *self = Self::default();
            return 0;
        }
        if !precise {
            *self = Self::default();
            return delta.round() as i32;
        }
        if delta == 0.0 {
            return 0;
        }
        let direction = if delta > 0.0 { 1 } else { -1 };
        if self.direction != 0 && self.direction != direction {
            self.residual = 0.0;
        }
        self.direction = direction;
        let total = delta + self.residual;
        if total >= f64::from(i32::MAX) || total <= f64::from(i32::MIN) {
            self.residual = 0.0;
            return total as i32;
        }
        // 只在最后的系统整数单位边界取整；补偿浮点加法接近整数时的表示误差。
        let whole = if total > 0.0 {
            (total + 1e-9).floor()
        } else {
            (total - 1e-9).ceil()
        };
        self.residual = total - whole;
        if self.residual.abs() <= 1e-9 {
            self.residual = 0.0;
        }
        whole as i32
    }
}

/// 每个注入会话各自持有，无锁、无分配。`release_all` 时必须清空，防止跨设备残留。
#[derive(Debug, Default)]
pub struct ScrollAccumulator {
    x: Axis,
    y: Axis,
}

impl ScrollAccumulator {
    /// 返回系统 API 的 (横向, 纵向) 整数值。普通滚轮沿用逐次四舍五入。
    pub fn quantize(&mut self, dx: f64, dy: f64, precise: bool, units: f64) -> (i32, i32) {
        if !units.is_finite() || units <= 0.0 {
            self.reset();
            return (0, 0);
        }
        (
            self.x.quantize(dx * units, precise),
            self.y.quantize(dy * units, precise),
        )
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 慢速像素滚动在两个轴上累计而不丢失() {
        let mut scroll = ScrollAccumulator::default();
        let mut total = (0, 0);
        for _ in 0..10 {
            let delta = scroll.quantize(0.2, -0.3, true, 1.0);
            total.0 += delta.0;
            total.1 += delta.1;
        }
        assert_eq!(total, (2, -3));
    }

    #[test]
    fn windows小于一个wheel单位的位移也能累计() {
        let mut scroll = ScrollAccumulator::default();
        assert_eq!(scroll.quantize(0.0025, 0.0025, true, 120.0), (0, 0));
        assert_eq!(scroll.quantize(0.0025, 0.0025, true, 120.0), (0, 0));
        assert_eq!(scroll.quantize(0.0025, 0.0025, true, 120.0), (0, 0));
        assert_eq!(scroll.quantize(0.0025, 0.0025, true, 120.0), (1, 1));
    }

    #[test]
    fn 单轴换向丢弃旧方向余量且不影响另一轴() {
        let mut scroll = ScrollAccumulator::default();
        assert_eq!(scroll.quantize(0.8, 0.8, true, 1.0), (0, 0));
        assert_eq!(scroll.quantize(-0.3, 0.3, true, 1.0), (0, 1));
        assert_eq!(scroll.quantize(-0.7, 0.0, true, 1.0), (-1, 0));
    }

    #[test]
    fn 普通滚轮保持原四舍五入且模式切换清空精密余量() {
        let mut scroll = ScrollAccumulator::default();
        assert_eq!(scroll.quantize(0.9, 0.9, true, 1.0), (0, 0));
        assert_eq!(scroll.quantize(0.5, -0.5, false, 1.0), (1, -1));
        assert_eq!(scroll.quantize(0.2, 0.2, true, 1.0), (0, 0));
        assert_eq!(scroll.quantize(1.0, -1.0, false, 120.0), (120, -120));
    }

    #[test]
    fn 会话释放后不携带旧余量() {
        let mut scroll = ScrollAccumulator::default();
        scroll.quantize(0.9, -0.9, true, 1.0);
        scroll.reset();
        assert_eq!(scroll.quantize(0.2, -0.2, true, 1.0), (0, 0));
    }

    #[test]
    fn 非有限输入不会注入无效位移且各轴隔离() {
        let mut scroll = ScrollAccumulator::default();
        scroll.quantize(0.9, 0.9, true, 1.0);
        assert_eq!(scroll.quantize(f64::NAN, 0.1, true, 1.0), (0, 1));
        assert_eq!(scroll.quantize(0.2, f64::INFINITY, true, 1.0), (0, 0));
        assert_eq!(scroll.quantize(1.0, 1.0, true, f64::NAN), (0, 0));
    }

    #[test]
    fn 超出整数范围饱和且不积累巨大余量() {
        let mut scroll = ScrollAccumulator::default();
        assert_eq!(
            scroll.quantize(1e20, -1e20, true, 1.0),
            (i32::MAX, i32::MIN)
        );
        assert_eq!(scroll.quantize(0.2, -0.2, true, 1.0), (0, 0));
    }
}
