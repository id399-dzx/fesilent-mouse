//! 本机设备布局预设；身份只用于匹配当前清单，不能新增设备或授予信任。

use std::collections::BTreeSet;

use ms_core::topology::{DeviceId, Placement};
use ms_proto::Edge;
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::devices::DevicePlacement;

pub const MAX_PROFILES: usize = 20;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileDevice {
    pub fingerprint: String,
    pub placement: Option<DevicePlacement>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutProfile {
    pub name: String,
    pub devices: Vec<ProfileDevice>,
    #[serde(default)]
    pub active_target: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ApplySummary {
    pub restored: usize,
    pub missing: usize,
}

fn name_key(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 40 || name.chars().any(char::is_control) {
        return Err("方案名称须为 1–40 个字符，且不能包含换行。".into());
    }
    Ok(name.to_lowercase())
}

fn configured_side(config: &Config) -> Result<Edge, String> {
    match config.side.as_str() {
        "left" => Ok(Edge::Left),
        "right" => Ok(Edge::Right),
        "top" => Ok(Edge::Top),
        "bottom" => Ok(Edge::Bottom),
        _ => Err("当前屏幕方位无效，请先选择方位。".into()),
    }
}

impl LayoutProfile {
    pub fn capture(name: &str, config: &Config) -> Result<Self, String> {
        name_key(name)?;
        if config.devices.records.is_empty() {
            return Err("请先配对并设置设备，再保存布局方案。".into());
        }
        // 主画布修改的方位会在帧末持久化；保存按钮同帧点击也须捕获新方位。
        let mut catalog = config.devices.clone();
        if let Some(active) = catalog.active_target.clone() {
            catalog.set_placement(
                &active,
                Some(DevicePlacement {
                    side: configured_side(config)?,
                    offset_px: config.display_offset,
                }),
            )?;
        }
        catalog.validate()?;
        let profile = Self {
            name: name.trim().into(),
            devices: catalog
                .records
                .iter()
                .map(|record| {
                    Ok(ProfileDevice {
                        fingerprint: record.id()?.to_string(),
                        placement: record.placement,
                    })
                })
                .collect::<Result<_, String>>()?,
            active_target: catalog.active().map(|record| record.fingerprint.clone()),
        };
        profile.validate()?;
        Ok(profile)
    }

    pub fn validate(&self) -> Result<(), String> {
        name_key(&self.name)?;
        let mut ids = BTreeSet::new();
        let mut sides = Vec::new();
        for device in &self.devices {
            let id = device
                .fingerprint
                .parse::<DeviceId>()
                .map_err(|e| e.to_string())?;
            if !ids.insert(id) {
                return Err("布局方案中存在重复设备。".into());
            }
            if let Some(placement) = device.placement {
                Placement::from(placement)
                    .validate()
                    .map_err(|e| e.to_string())?;
                if sides.contains(&placement.side) {
                    return Err("布局方案中有两台设备占用同一方向。".into());
                }
                sides.push(placement.side);
            }
        }
        if self.devices.is_empty() {
            return Err("布局方案中没有设备。".into());
        }
        if let Some(target) = &self.active_target {
            let target = target.parse::<DeviceId>().map_err(|e| e.to_string())?;
            if !self.devices.iter().any(|device| {
                device.fingerprint.parse::<DeviceId>() == Ok(target) && device.placement.is_some()
            }) {
                return Err("布局方案中的连接目标尚未设置方位。".into());
            }
        }
        Ok(())
    }

    /// 返回新配置；整个快照验证成功才提交，支持两台设备原子交换方位。
    /// 新配对的设备保留原位，冲突时报错；已移除设备跳过，不恢复地址或信任。
    pub fn apply(&self, config: &Config) -> Result<(Config, ApplySummary), String> {
        self.validate()?;
        config.devices.validate()?;
        let mut next = config.clone();
        let mut restored = 0;
        for device in &self.devices {
            let id = device
                .fingerprint
                .parse::<DeviceId>()
                .map_err(|e| e.to_string())?;
            if let Some(record) = next
                .devices
                .records
                .iter_mut()
                .find(|record| record.id() == Ok(id))
            {
                record.placement = device.placement;
                restored += 1;
            }
        }
        if restored == 0 {
            return Err("方案中的设备均已移除，请配对设备后重新保存方案。".into());
        }
        next.devices.active_target = self
            .active_target
            .as_deref()
            .and_then(|target| next.devices.record(target))
            .filter(|record| record.placement.is_some())
            .map(|record| record.fingerprint.clone());
        next.devices
            .validate()
            .map_err(|error| format!("无法应用方案：{error}"))?;
        next.connect = None;
        if let Some(active) = next.devices.active() {
            let placement = active.placement.ok_or("连接目标尚未设置方位。")?;
            next.connect = active.address.clone();
            next.side = match placement.side {
                Edge::Left => "left",
                Edge::Right => "right",
                Edge::Top => "top",
                Edge::Bottom => "bottom",
            }
            .into();
            next.display_offset = placement.offset_px;
        }
        Ok((
            next,
            ApplySummary {
                restored,
                missing: self.devices.len() - restored,
            },
        ))
    }
}

/// 同名（忽略首尾空格和大小写）禁止隐式覆盖；更新只能由对应方案的按钮触发。
pub fn save(config: &mut Config, name: &str, replace: bool) -> Result<(), String> {
    let key = name_key(name)?;
    let index = config
        .layout_profiles
        .iter()
        .position(|profile| name_key(&profile.name).is_ok_and(|other| other == key));
    if index.is_some() && !replace {
        return Err("已有同名方案；请使用该方案下的“更新为当前布局”。".into());
    }
    if replace && index.is_none() {
        return Err("要更新的方案已不存在。".into());
    }
    if index.is_none() && config.layout_profiles.len() >= MAX_PROFILES {
        return Err("最多保存 20 个布局方案，请先删除不再使用的方案。".into());
    }
    let profile = LayoutProfile::capture(name, config)?;
    if let Some(index) = index {
        config.layout_profiles[index] = profile;
    } else {
        config.layout_profiles.push(profile);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn fp(value: u8) -> String {
        DeviceId::from_fingerprint([value; 32]).to_string()
    }
    fn config() -> Config {
        let mut config = Config::default();
        for (id, side) in [(1, Edge::Right), (2, Edge::Left)] {
            config
                .devices
                .upsert_authenticated([id; 32], "测试电脑", Some("192.168.1.20"), side)
                .unwrap();
        }
        config.devices.select(&fp(1)).unwrap();
        config
    }

    #[test]
    fn 旧配置默认无方案且往返仅保存身份方位目标() {
        let old: Config = serde_json::from_str(r#"{"config_ver":1}"#).unwrap();
        assert!(old.layout_profiles.is_empty());
        let mut config = config();
        config.side = "top".into();
        config.display_offset = -120.0;
        save(&mut config, " 办公室 ", false).unwrap();
        let text = serde_json::to_string(&config.layout_profiles[0]).unwrap();
        assert!(!text.contains("192.168"));
        assert!(!text.contains("测试电脑"));
        let back: LayoutProfile = serde_json::from_str(&text).unwrap();
        assert_eq!(back.name, "办公室");
        assert_eq!(
            back.devices[0].placement.unwrap(),
            DevicePlacement {
                side: Edge::Top,
                offset_px: -120.0
            }
        );
        let restored: Config =
            serde_json::from_str(&serde_json::to_string(&config).unwrap()).unwrap();
        assert_eq!(restored.layout_profiles, config.layout_profiles);
    }

    #[test]
    fn 交换方位一次提交且使用最新地址保留偏好() {
        let mut config = config();
        let profile = LayoutProfile::capture("书房", &config).unwrap();
        config.devices.records[0].placement = Some(DevicePlacement {
            side: Edge::Left,
            offset_px: 8.0,
        });
        config.devices.records[1].placement = Some(DevicePlacement {
            side: Edge::Right,
            offset_px: 0.0,
        });
        config.devices.records[0].address = Some("192.168.9.88:60000".into());
        config.devices.records[0].name = "新名称".into();
        config.clipboard = false;
        config.mac_control = true;
        let (next, summary) = profile.apply(&config).unwrap();
        assert_eq!(
            summary,
            ApplySummary {
                restored: 2,
                missing: 0
            }
        );
        assert_eq!(next.devices.records[0].placement.unwrap().side, Edge::Right);
        assert_eq!(next.devices.records[1].placement.unwrap().side, Edge::Left);
        assert_eq!(next.connect.as_deref(), Some("192.168.9.88:60000"));
        assert_eq!(next.devices.records[0].name, "新名称");
        assert!(!next.clipboard);
        assert!(next.mac_control);
    }

    #[test]
    fn 已移除的目标不复活且新设备保留() {
        let mut config = config();
        let profile = LayoutProfile::capture("办公室", &config).unwrap();
        config.devices.remove(&fp(1));
        config
            .devices
            .upsert_authenticated([3; 32], "新增电脑", None, Edge::Bottom)
            .unwrap();
        config.devices.select(&fp(3)).unwrap();
        let (next, summary) = profile.apply(&config).unwrap();
        assert_eq!(
            summary,
            ApplySummary {
                restored: 1,
                missing: 1
            }
        );
        assert_eq!(next.devices.records.len(), 2);
        assert!(next.devices.record(&fp(1)).is_none());
        assert!(next.devices.active_target.is_none());
        assert!(next.connect.is_none());
        assert_eq!(
            next.devices.record(&fp(3)).unwrap().placement.unwrap().side,
            Edge::Bottom
        );
    }

    #[test]
    fn 与新增设备冲突或损坏快照不改变原配置() {
        let mut config = config();
        let profile = LayoutProfile::capture("办公室", &config).unwrap();
        config.devices.records[0].placement = Some(DevicePlacement {
            side: Edge::Top,
            offset_px: 0.0,
        });
        config
            .devices
            .upsert_authenticated([3; 32], "新设备", None, Edge::Right)
            .unwrap();
        let before = serde_json::to_string(&config).unwrap();
        assert!(profile.apply(&config).is_err());
        assert_eq!(serde_json::to_string(&config).unwrap(), before);
        let mut invalid = profile.clone();
        invalid.devices[0].placement.as_mut().unwrap().offset_px = f64::NAN;
        assert!(invalid.apply(&config).is_err());
        let mut invalid = profile.clone();
        invalid.devices.push(invalid.devices[0].clone());
        assert!(invalid.apply(&config).is_err());
        let mut invalid = profile.clone();
        invalid.active_target = Some(fp(99));
        assert!(invalid.apply(&config).is_err());
        config.devices.records.clear();
        config.devices.active_target = None;
        assert!(profile.apply(&config).is_err());
    }

    #[test]
    fn 命名覆盖和容量限制不会丢失方案() {
        let mut config = config();
        save(&mut config, "Office", false).unwrap();
        assert!(save(&mut config, " office ", false).is_err());
        assert!(save(&mut config, "不存在", true).is_err());
        config.display_offset = 60.0;
        save(&mut config, "Office", true).unwrap();
        assert_eq!(config.layout_profiles.len(), 1);
        assert_eq!(
            config.layout_profiles[0].devices[0]
                .placement
                .unwrap()
                .offset_px,
            60.0
        );
        for invalid in ["  ", "含\n换行", &"字".repeat(41)] {
            assert!(save(&mut config, invalid, false).is_err());
        }
        for index in 1..MAX_PROFILES {
            save(&mut config, &format!("方案{index}"), false).unwrap();
        }
        assert!(save(&mut config, "超额方案", false).is_err());
        assert_eq!(config.layout_profiles.len(), MAX_PROFILES);
        save(&mut config, "Office", true).unwrap();
    }

    #[test]
    fn 大小写指纹匹配且无目标方案应用后保持暂停选择() {
        let mut config = config();
        config.devices.select_local();
        config.devices.records[0].fingerprint = fp(0xab);
        let mut profile = LayoutProfile::capture("只排布局", &config).unwrap();
        profile.devices[0].fingerprint = profile.devices[0].fingerprint.to_uppercase();
        assert!(profile.devices[0].fingerprint.contains("AB"));
        config.devices.select(&fp(2)).unwrap();
        let (next, _) = profile.apply(&config).unwrap();
        assert_eq!(next.devices.active_target, None);
        assert_eq!(next.connect, None);
    }
}
