//! 已知设备与四向相邻关系；这是布局数据，不表示同时存在多条网络会话。
//!
//! [`DeviceId`] 使用已认证证书的完整 SHA-256 指纹，区别于协议 Hello 内的 UUID。
//! 发现广播中的名称和 ID 不能直接成为受信任身份。配置编辑不在输入事件热路径中。

use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use ms_proto::Edge;

use crate::layout::opposite;

/// 证书身份键；必须由 TLS 认证/人工配对后的指纹构造。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeviceId([u8; 32]);

impl DeviceId {
    pub const fn from_fingerprint(fingerprint: [u8; 32]) -> Self {
        Self(fingerprint)
    }

    pub const fn fingerprint(self) -> [u8; 32] {
        self.0
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl FromStr for DeviceId {
    type Err = TopologyError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(TopologyError::InvalidIdentity);
        }
        let mut bytes = [0; 32];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
                .map_err(|_| TopologyError::InvalidIdentity)?;
        }
        Ok(Self(bytes))
    }
}

/// 虚拟显示区域的真实物理像素尺寸（不包含操作系统桌面原点）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DisplaySize {
    pub width: f64,
    pub height: f64,
}

impl DisplaySize {
    pub fn new(width: f64, height: f64) -> Result<Self, TopologyError> {
        let size = Self { width, height };
        size.validate()?;
        Ok(size)
    }

    pub fn validate(self) -> Result<(), TopologyError> {
        if !self.width.is_finite()
            || !self.height.is_finite()
            || self.width <= 0.0
            || self.height <= 0.0
        {
            return Err(TopologyError::InvalidDisplaySize);
        }
        Ok(())
    }

    pub fn along_edge(self, side: Edge) -> f64 {
        match side {
            Edge::Left | Edge::Right => self.height,
            Edge::Top | Edge::Bottom => self.width,
        }
    }
}

/// 对端方位和沿共同边的偏移。
///
/// 非零偏移表示对端的顶端/左端相对来源顶端/左端的物理像素位移；正值向下/右。
/// 零值保留旧双机按比例映射语义，不能用它推断两块不同尺寸屏幕的真实接触长度。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub side: Edge,
    pub offset_px: f64,
}

impl Placement {
    pub fn validate(self) -> Result<(), TopologyError> {
        if !self.offset_px.is_finite() {
            return Err(TopologyError::InvalidOffset);
        }
        Ok(())
    }

    pub fn reversed(self) -> Self {
        Self {
            side: opposite(self.side),
            offset_px: -self.offset_px,
        }
    }

    /// 真实尺寸已知时检查非零偏移是否还存在可穿越的共同边。
    pub fn validate_overlap(
        self,
        source: DisplaySize,
        target: DisplaySize,
    ) -> Result<(), TopologyError> {
        self.validate()?;
        source.validate()?;
        target.validate()?;
        if self.offset_px != 0.0
            && (self.offset_px >= source.along_edge(self.side)
                || self.offset_px <= -target.along_edge(self.side))
        {
            return Err(TopologyError::NoSharedEdge);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Link {
    pub from: DeviceId,
    pub to: DeviceId,
    pub placement: Placement,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Neighbor {
    pub device: DeviceId,
    pub placement: Placement,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TopologyError {
    #[error("设备身份必须是完整的 64 位 SHA-256 指纹")]
    InvalidIdentity,
    #[error("设备不在已知设备清单中")]
    UnknownDevice,
    #[error("设备不能与自身相邻")]
    SelfLink,
    #[error("该方向已有设备，请先移动或移除原设备")]
    SideOccupied,
    #[error("显示偏移必须是有限数值")]
    InvalidOffset,
    #[error("显示区域宽高必须是有限的正数")]
    InvalidDisplaySize,
    #[error("两块显示区域没有可穿越的共同边")]
    NoSharedEdge,
    #[error("所选设备尚未放置在本机的四个方向上")]
    NotLocalNeighbor,
    #[error("不能移除本机设备")]
    CannotRemoveLocal,
}

/// 可保存多台设备，但任一设备的同一侧最多一个邻居；活动目标最多一个。
#[derive(Debug, Clone, PartialEq)]
pub struct Topology {
    local: DeviceId,
    devices: BTreeSet<DeviceId>,
    links: Vec<Link>,
    active_target: Option<DeviceId>,
}

impl Topology {
    pub fn new(local: DeviceId) -> Self {
        Self {
            local,
            devices: BTreeSet::from([local]),
            links: Vec::new(),
            active_target: None,
        }
    }

    pub const fn local(&self) -> DeviceId {
        self.local
    }

    pub fn device_ids(&self) -> impl Iterator<Item = DeviceId> + '_ {
        self.devices.iter().copied()
    }

    pub fn add_device(&mut self, device: DeviceId) -> bool {
        self.devices.insert(device)
    }

    pub fn links(&self) -> &[Link] {
        &self.links
    }

    pub const fn active_target(&self) -> Option<DeviceId> {
        self.active_target
    }

    pub fn neighbor(&self, device: DeviceId, side: Edge) -> Option<Neighbor> {
        self.links.iter().find_map(|link| {
            let neighbor = if link.from == device {
                Neighbor {
                    device: link.to,
                    placement: link.placement,
                }
            } else if link.to == device {
                Neighbor {
                    device: link.from,
                    placement: link.placement.reversed(),
                }
            } else {
                return None;
            };
            (neighbor.placement.side == side).then_some(neighbor)
        })
    }

    pub fn placement(&self, from: DeviceId, to: DeviceId) -> Option<Placement> {
        [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom]
            .into_iter()
            .filter_map(|side| self.neighbor(from, side))
            .find(|neighbor| neighbor.device == to)
            .map(|neighbor| neighbor.placement)
    }

    /// 同一对设备重新放置会替换原关系；冲突或无效输入不改变原拓扑。
    pub fn set_neighbor(
        &mut self,
        from: DeviceId,
        to: DeviceId,
        placement: Placement,
    ) -> Result<(), TopologyError> {
        placement.validate()?;
        if from == to {
            return Err(TopologyError::SelfLink);
        }
        if !self.devices.contains(&from) || !self.devices.contains(&to) {
            return Err(TopologyError::UnknownDevice);
        }
        for (source, target, side) in [
            (from, to, placement.side),
            (to, from, opposite(placement.side)),
        ] {
            if self
                .neighbor(source, side)
                .is_some_and(|neighbor| neighbor.device != target)
            {
                return Err(TopologyError::SideOccupied);
            }
        }
        self.links.retain(|link| {
            !((link.from == from && link.to == to) || (link.from == to && link.to == from))
        });
        self.links.push(Link {
            from,
            to,
            placement,
        });
        Ok(())
    }

    /// None 表示回到本机；选择行为本身不建立或伪造网络连接。
    pub fn select_target(&mut self, target: Option<DeviceId>) -> Result<(), TopologyError> {
        if let Some(target) = target {
            if !self.devices.contains(&target) {
                return Err(TopologyError::UnknownDevice);
            }
            if self.placement(self.local, target).is_none() {
                return Err(TopologyError::NotLocalNeighbor);
            }
        }
        self.active_target = target;
        Ok(())
    }

    pub fn unlink(&mut self, from: DeviceId, to: DeviceId) -> bool {
        let count = self.links.len();
        self.links.retain(|link| {
            !((link.from == from && link.to == to) || (link.from == to && link.to == from))
        });
        if self
            .active_target
            .is_some_and(|target| self.placement(self.local, target).is_none())
        {
            self.active_target = None;
        }
        self.links.len() != count
    }

    /// 删除当前目标时回退本机，不静默改选另一台设备。
    pub fn remove_device(&mut self, device: DeviceId) -> Result<bool, TopologyError> {
        if device == self.local {
            return Err(TopologyError::CannotRemoveLocal);
        }
        let removed = self.devices.remove(&device);
        self.links
            .retain(|link| link.from != device && link.to != device);
        if self.active_target == Some(device) {
            self.active_target = None;
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn id(value: u8) -> DeviceId {
        DeviceId::from_fingerprint([value; 32])
    }

    fn placement(side: Edge, offset_px: f64) -> Placement {
        Placement { side, offset_px }
    }

    #[test]
    fn 身份使用完整指纹且拒绝短串与非十六进制() {
        let text = id(0xab).to_string();
        assert_eq!(text.len(), 64);
        assert_eq!(text.to_uppercase().parse(), Ok(id(0xab)));
        for invalid in ["", "abababababababab", &"g".repeat(64), &"é".repeat(32)] {
            assert_eq!(
                invalid.parse::<DeviceId>(),
                Err(TopologyError::InvalidIdentity)
            );
        }
    }

    #[test]
    fn 相邻关系互反且重新放置不残留旧边() {
        let mut topology = Topology::new(id(1));
        topology.add_device(id(2));
        topology
            .set_neighbor(id(1), id(2), placement(Edge::Right, 120.0))
            .unwrap();
        assert_eq!(
            topology
                .neighbor(id(2), Edge::Left)
                .unwrap()
                .placement
                .offset_px,
            -120.0
        );
        topology
            .set_neighbor(id(1), id(2), placement(Edge::Top, -50.0))
            .unwrap();
        assert!(topology.neighbor(id(1), Edge::Right).is_none());
        assert_eq!(topology.links().len(), 1);
    }

    #[test]
    fn 双方边位冲突失败保持原拓扑() {
        let mut topology = Topology::new(id(1));
        for device in [id(2), id(3), id(4)] {
            topology.add_device(device);
        }
        topology
            .set_neighbor(id(1), id(2), placement(Edge::Right, 0.0))
            .unwrap();
        topology
            .set_neighbor(id(3), id(4), placement(Edge::Right, 0.0))
            .unwrap();
        let before = topology.clone();
        assert_eq!(
            topology.set_neighbor(id(1), id(3), placement(Edge::Right, 0.0)),
            Err(TopologyError::SideOccupied)
        );
        assert_eq!(
            topology.set_neighbor(id(1), id(3), placement(Edge::Left, 0.0)),
            Err(TopologyError::SideOccupied)
        );
        assert_eq!(topology, before);
    }

    #[test]
    fn 删除与取消相邻关系回退本机且不自动改选() {
        let mut topology = Topology::new(id(1));
        topology.add_device(id(2));
        topology.add_device(id(3));
        assert_eq!(
            topology.select_target(Some(id(2))),
            Err(TopologyError::NotLocalNeighbor)
        );
        topology
            .set_neighbor(id(1), id(2), placement(Edge::Right, 0.0))
            .unwrap();
        topology.select_target(Some(id(2))).unwrap();
        assert!(topology.unlink(id(1), id(2)));
        assert_eq!(topology.active_target(), None);
        topology
            .set_neighbor(id(1), id(2), placement(Edge::Bottom, 0.0))
            .unwrap();
        topology.select_target(Some(id(2))).unwrap();
        assert!(topology.remove_device(id(2)).unwrap());
        assert_eq!(topology.active_target(), None);
        assert!(topology.links().is_empty());
        assert_eq!(
            topology.remove_device(id(1)),
            Err(TopologyError::CannotRemoveLocal)
        );
    }

    #[test]
    fn 无效偏移及身份关系不可进入拓扑() {
        let mut topology = Topology::new(id(1));
        topology.add_device(id(2));
        assert_eq!(
            topology.set_neighbor(id(1), id(1), placement(Edge::Left, 0.0)),
            Err(TopologyError::SelfLink)
        );
        assert_eq!(
            topology.set_neighbor(id(1), id(3), placement(Edge::Left, 0.0)),
            Err(TopologyError::UnknownDevice)
        );
        for offset in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(
                topology.set_neighbor(id(1), id(2), placement(Edge::Left, offset)),
                Err(TopologyError::InvalidOffset)
            );
        }
        assert!(topology.links().is_empty());
    }

    #[test]
    fn 非零物理偏移必须存在共同边而零偏移保持旧比例模式() {
        let source = DisplaySize::new(1920.0, 1080.0).unwrap();
        let target = DisplaySize::new(2560.0, 1440.0).unwrap();
        assert!(
            placement(Edge::Right, 1079.0)
                .validate_overlap(source, target)
                .is_ok()
        );
        assert!(
            placement(Edge::Right, -1439.0)
                .validate_overlap(source, target)
                .is_ok()
        );
        assert_eq!(
            placement(Edge::Right, 1080.0).validate_overlap(source, target),
            Err(TopologyError::NoSharedEdge)
        );
        assert_eq!(
            placement(Edge::Right, -1440.0).validate_overlap(source, target),
            Err(TopologyError::NoSharedEdge)
        );
        assert!(
            placement(Edge::Top, 1919.0)
                .validate_overlap(source, target)
                .is_ok()
        );
        assert!(
            placement(Edge::Right, 0.0)
                .validate_overlap(source, target)
                .is_ok()
        );
        assert_eq!(
            DisplaySize::new(0.0, 100.0),
            Err(TopologyError::InvalidDisplaySize)
        );
    }
}
