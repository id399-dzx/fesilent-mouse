//! 已认证设备清单的配置 DTO；随 Config 一起保存，不另建配置文件或并发写入者。
//!
//! 清单只保存名称、地址和布局偏好，既不是信任列表，也不是在线设备列表。
//! 只有 TLS/SAS 认证成功后才能调用 `upsert_authenticated`，连接仍由 daemon 串行管理。

use std::collections::BTreeSet;

use ms_core::topology::{DeviceId, Placement, Topology};
use ms_proto::Edge;
use serde::{Deserialize, Serialize};

use crate::config::normalize_peer_address;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DevicePlacement {
    pub side: Edge,
    /// 0 保持原来的按比例跨屏；非零为沿边实际物理像素偏移。
    #[serde(default)]
    pub offset_px: f64,
}

impl Default for DevicePlacement {
    fn default() -> Self {
        Self {
            side: Edge::Right,
            offset_px: 0.0,
        }
    }
}

impl From<DevicePlacement> for Placement {
    fn from(value: DevicePlacement) -> Self {
        Self {
            side: value.side,
            offset_px: value.offset_px,
        }
    }
}

/// 指纹使用完整 64 位十六进制，不能用日志中的 16 位短串冒充唯一身份。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeviceRecord {
    pub fingerprint: String,
    pub name: String,
    pub address: Option<String>,
    /// 未摆放设备仍可保存；一台本机最多放置四个直接相邻设备。
    pub placement: Option<DevicePlacement>,
}

impl DeviceRecord {
    pub fn id(&self) -> Result<DeviceId, String> {
        self.fingerprint
            .parse::<DeviceId>()
            .map_err(|e| e.to_string())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeviceCatalog {
    pub records: Vec<DeviceRecord>,
    /// 所选连接目标；None 为尚未选择/回到本机，不能据此显示为已连接。
    pub active_target: Option<String>,
}

impl DeviceCatalog {
    /// 配置导入/加载后验证，再让保存或连接操作使用它。
    /// 不检查 trust.json：清单中的记录本身不能授予连接权限。
    pub fn validate(&self) -> Result<(), String> {
        let mut identities = BTreeSet::new();
        let mut sides = Vec::new();
        for record in &self.records {
            let id = record.id()?;
            if !identities.insert(id) {
                return Err("设备清单存在重复身份，请移除重复记录。".into());
            }
            if record.name.trim().is_empty() {
                return Err("设备名称不能为空。".into());
            }
            if let Some(address) = &record.address {
                if normalize_peer_address(address)?.is_none() {
                    return Err("设备地址留空时请使用自动发现，不要保存空地址。".into());
                }
            }
            if let Some(placement) = record.placement {
                Placement::from(placement)
                    .validate()
                    .map_err(|error| error.to_string())?;
                if sides.contains(&placement.side) {
                    return Err("该方向已有设备，请先移动或移除原设备。".into());
                }
                sides.push(placement.side);
            }
        }
        if let Some(target) = &self.active_target {
            let record = self
                .record(target)
                .ok_or_else(|| "所选设备已不在设备清单中。".to_string())?;
            if record.placement.is_none() {
                return Err("所选设备尚未设置屏幕方位。".into());
            }
        }
        Ok(())
    }

    pub fn record(&self, fingerprint: &str) -> Option<&DeviceRecord> {
        let id = fingerprint.parse::<DeviceId>().ok()?;
        self.records.iter().find(|record| record.id() == Ok(id))
    }

    pub fn active(&self) -> Option<&DeviceRecord> {
        self.active_target
            .as_deref()
            .and_then(|fingerprint| self.record(fingerprint))
    }

    /// 首个真实认证的对端沿用旧单 peer 的 side；占用时仅保存身份、不伪造可用邻居。
    /// 既有设备重连只更新地址，保留用户命名、位置和选择。
    pub fn upsert_authenticated(
        &mut self,
        fingerprint: [u8; 32],
        name: &str,
        address: Option<&str>,
        legacy_side: Edge,
    ) -> Result<(), String> {
        self.validate()?;
        let id = DeviceId::from_fingerprint(fingerprint);
        let address = address.map(normalize_peer_address).transpose()?.flatten();
        let name = if name.trim().is_empty() {
            "未命名设备".to_string()
        } else {
            name.trim().to_string()
        };
        if let Some(record) = self.records.iter_mut().find(|record| record.id() == Ok(id)) {
            if address.is_some() {
                record.address = address;
            }
            return Ok(());
        }
        let occupied = self.records.iter().any(|record| {
            record
                .placement
                .is_some_and(|placement| placement.side == legacy_side)
        });
        self.records.push(DeviceRecord {
            fingerprint: id.to_string(),
            name,
            address,
            placement: (!occupied).then_some(DevicePlacement {
                side: legacy_side,
                offset_px: 0.0,
            }),
        });
        Ok(())
    }

    /// 只选择目标；调用方须安全停止旧会话，再使用其地址/方位建立新会话。
    pub fn select(&mut self, fingerprint: &str) -> Result<(), String> {
        self.validate()?;
        let record = self
            .record(fingerprint)
            .ok_or_else(|| "设备不在已知设备清单中。".to_string())?;
        if record.placement.is_none() {
            return Err("请先为这台设备设置屏幕方位。".into());
        }
        self.active_target = Some(record.id()?.to_string());
        Ok(())
    }

    pub fn select_local(&mut self) {
        self.active_target = None;
    }

    /// 冲突失败不覆盖旧位置；移出工作台时若为当前目标则回退本机。
    pub fn set_placement(
        &mut self,
        fingerprint: &str,
        placement: Option<DevicePlacement>,
    ) -> Result<(), String> {
        let id = fingerprint.parse::<DeviceId>().map_err(|e| e.to_string())?;
        let index = self
            .records
            .iter()
            .position(|record| record.id() == Ok(id))
            .ok_or_else(|| "设备不在已知设备清单中。".to_string())?;
        if let Some(placement) = placement {
            Placement::from(placement)
                .validate()
                .map_err(|error| error.to_string())?;
            if self.records.iter().enumerate().any(|(other, record)| {
                other != index
                    && record
                        .placement
                        .is_some_and(|existing| existing.side == placement.side)
            }) {
                return Err("该方向已有设备，请先移动或移除原设备。".into());
            }
        }
        self.records[index].placement = placement;
        if placement.is_none() && self.active().is_some_and(|record| record.id() == Ok(id)) {
            self.active_target = None;
        }
        Ok(())
    }

    /// 清单移除与信任撤销是两件事；调用方需另行撤销 trust.json 中的对应指纹。
    pub fn remove(&mut self, fingerprint: &str) -> bool {
        let Ok(id) = fingerprint.parse::<DeviceId>() else {
            return false;
        };
        let before = self.records.len();
        self.records.retain(|record| record.id() != Ok(id));
        if self
            .active_target
            .as_deref()
            .is_some_and(|active| active.parse::<DeviceId>() == Ok(id))
        {
            self.active_target = None;
        }
        self.records.len() != before
    }

    /// 为本机生成真实相邻关系；保存但未摆放的设备没有跨屏入口。
    pub fn topology(&self, local_fingerprint: [u8; 32]) -> Result<Topology, String> {
        self.validate()?;
        let local = DeviceId::from_fingerprint(local_fingerprint);
        let mut topology = Topology::new(local);
        for record in &self.records {
            let id = record.id()?;
            if id == local {
                return Err("设备清单不能把本机添加为对端。".into());
            }
            topology.add_device(id);
            if let Some(placement) = record.placement {
                topology
                    .set_neighbor(local, id, placement.into())
                    .map_err(|error| error.to_string())?;
            }
        }
        if let Some(target) = &self.active_target {
            topology
                .select_target(Some(target.parse::<DeviceId>().map_err(|e| e.to_string())?))
                .map_err(|error| error.to_string())?;
        }
        Ok(topology)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn fp(value: u8) -> String {
        DeviceId::from_fingerprint([value; 32]).to_string()
    }

    fn pair(catalog: &mut DeviceCatalog, value: u8, side: Edge) {
        catalog
            .upsert_authenticated([value; 32], "测试电脑", Some("192.168.1.20"), side)
            .unwrap();
    }

    #[test]
    fn 旧配置缺字段可默认且新增字段完整往返() {
        let empty: DeviceCatalog = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, DeviceCatalog::default());
        let mut catalog = empty;
        pair(&mut catalog, 1, Edge::Top);
        catalog
            .set_placement(
                &fp(1),
                Some(DevicePlacement {
                    side: Edge::Top,
                    offset_px: -120.0,
                }),
            )
            .unwrap();
        catalog.select(&fp(1)).unwrap();
        let json = serde_json::to_vec(&catalog).unwrap();
        let restored: DeviceCatalog = serde_json::from_slice(&json).unwrap();
        assert_eq!(restored, catalog);
        restored.validate().unwrap();
        assert_eq!(
            restored.active().unwrap().address.as_deref(),
            Some("192.168.1.20:53728")
        );
    }

    #[test]
    fn 旧单peer方位迁移等认证完成且占用时保留未摆放设备() {
        let mut catalog = DeviceCatalog::default();
        pair(&mut catalog, 1, Edge::Left);
        pair(&mut catalog, 2, Edge::Left);
        assert_eq!(catalog.records[0].placement.unwrap().side, Edge::Left);
        assert_eq!(catalog.records[0].placement.unwrap().offset_px, 0.0);
        assert!(catalog.records[1].placement.is_none());
        assert_eq!(catalog.records.len(), 2);
        assert!(catalog.select(&fp(2)).is_err());
        assert!(catalog.active().is_none());
    }

    #[test]
    fn 重连更新地址且保留用户命名独立位置和目标() {
        let mut catalog = DeviceCatalog::default();
        pair(&mut catalog, 1, Edge::Right);
        catalog.records[0].name = "书房 Mac".into();
        catalog
            .set_placement(
                &fp(1),
                Some(DevicePlacement {
                    side: Edge::Bottom,
                    offset_px: 55.0,
                }),
            )
            .unwrap();
        catalog.select(&fp(1)).unwrap();
        catalog
            .upsert_authenticated(
                [1; 32],
                " 改名电脑 ",
                Some("192.168.1.30:60000"),
                Edge::Left,
            )
            .unwrap();
        let record = catalog.active().unwrap();
        assert_eq!(record.name, "书房 Mac");
        assert_eq!(record.address.as_deref(), Some("192.168.1.30:60000"));
        assert_eq!(
            record.placement.unwrap(),
            DevicePlacement {
                side: Edge::Bottom,
                offset_px: 55.0
            }
        );
        assert_eq!(catalog.records.len(), 1);
    }

    #[test]
    fn 占位冲突不覆盖且移除目标不自动改选() {
        let mut catalog = DeviceCatalog::default();
        pair(&mut catalog, 1, Edge::Right);
        pair(&mut catalog, 2, Edge::Left);
        catalog.select(&fp(1)).unwrap();
        let before = catalog.clone();
        assert!(
            catalog
                .set_placement(
                    &fp(2),
                    Some(DevicePlacement {
                        side: Edge::Right,
                        offset_px: 0.0
                    })
                )
                .is_err()
        );
        assert_eq!(catalog, before);
        assert!(catalog.remove(&fp(1).to_uppercase()));
        assert_eq!(catalog.active_target, None);
        assert_eq!(catalog.records.len(), 1);
        assert!(!catalog.remove("短ID"));
    }

    #[test]
    fn 取消当前摆放回退本机且拓扑排除未摆放设备() {
        let mut catalog = DeviceCatalog::default();
        pair(&mut catalog, 1, Edge::Right);
        pair(&mut catalog, 2, Edge::Right);
        catalog.select(&fp(1)).unwrap();
        let topology = catalog.topology([99; 32]).unwrap();
        assert_eq!(topology.links().len(), 1);
        assert_eq!(topology.device_ids().count(), 3);
        assert_eq!(topology.active_target().unwrap().to_string(), fp(1));
        catalog.set_placement(&fp(1), None).unwrap();
        assert!(catalog.active_target.is_none());
        assert!(catalog.topology([99; 32]).unwrap().links().is_empty());
        assert!(catalog.topology([1; 32]).is_err());
    }

    #[test]
    fn 导入清单拒绝重复短指纹无效地址和非有限偏移() {
        let mut catalog = DeviceCatalog::default();
        pair(&mut catalog, 1, Edge::Right);
        let mut invalid = catalog.clone();
        invalid.records[0].fingerprint.truncate(16);
        assert!(invalid.validate().is_err());
        let mut invalid = catalog.clone();
        invalid.records.push(invalid.records[0].clone());
        assert!(invalid.validate().is_err());
        let mut invalid = catalog.clone();
        invalid.records[0].address = Some("not an IP".into());
        assert!(invalid.validate().is_err());
        let before = catalog.clone();
        assert!(
            catalog
                .set_placement(
                    &fp(1),
                    Some(DevicePlacement {
                        side: Edge::Top,
                        offset_px: f64::NAN
                    })
                )
                .is_err()
        );
        assert_eq!(catalog, before);
        catalog.active_target = Some(fp(2));
        assert!(catalog.validate().is_err());
    }
}
