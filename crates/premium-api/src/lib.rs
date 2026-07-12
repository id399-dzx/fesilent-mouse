//! 付费功能能力接口（开放核心 + 闭源付费模块架构）。
//!
//! 开源核心只依赖本 crate 的 [`PremiumProvider`] trait 与免费 stub [`FreeProvider`]；
//! 真实付费功能实现（文件传输、多设备）位于**闭源** `ms-premium` crate，仅在
//! `--features premium` 构建时注入。开源仓库不含 premium 实现，因此免费版无法被
//! 自行编译解锁付费功能——这是「开放核心」模式的收费壁垒所在。
//!
//! license 校验（Ed25519 离线签名）的算法与公钥**可公开**：壁垒是付费功能的实现
//! 闭源，而非校验逻辑保密。即便有人篡改开源侧的功能判断，也拿不到真实实现。
//! 校验实现见 `license` 模块（后续任务落地）。

use serde::{Deserialize, Serialize};

/// 授权等级。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    /// 免费版：键鼠共享 + 剪贴板 + 最多 2 台。
    Free,
    /// 专业版：解锁文件传输、多设备等。
    Pro,
}

impl Tier {
    /// 中文展示名。
    pub fn label(self) -> &'static str {
        match self {
            Tier::Free => "免费版",
            Tier::Pro => "专业版",
        }
    }
}

/// 付费功能位（可按位叠加）。
pub mod features {
    /// 文件传输。
    pub const FILE_TRANSFER: u32 = 1 << 0;
    /// 多设备（3 台及以上）。
    pub const MULTI_DEVICE: u32 = 1 << 1;
}

/// 一份已校验的授权信息（来自合法 license key 的 payload）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LicenseInfo {
    /// 授权等级。
    pub tier: Tier,
    /// 最大同时互联设备数（含本机）。
    pub max_devices: u32,
    /// 解锁的功能位（[`features`] 常量按位或）。
    pub features: u32,
    /// 被授权人标识（邮箱/名称，仅展示）。
    pub licensee: String,
    /// 过期时间（Unix 秒）；`None` = 永久。
    pub expires_at: Option<i64>,
}

impl LicenseInfo {
    /// 是否解锁某功能位。
    pub fn has_feature(&self, bit: u32) -> bool {
        self.features & bit != 0
    }

    /// `now`（Unix 秒）是否在有效期内。
    pub fn is_valid_at(&self, now: i64) -> bool {
        self.expires_at.map(|e| now <= e).unwrap_or(true)
    }
}

/// 激活 / 校验错误。
#[derive(Debug, thiserror::Error)]
pub enum LicenseError {
    /// 当前是免费版二进制，没有付费实现可激活。
    #[error("当前为免费版，请下载专业版后再激活")]
    PremiumBuildRequired,
    /// 授权码无法解析（格式/编码错误）。
    #[error("授权码格式无效")]
    Malformed,
    /// 签名校验未通过（伪造或损坏）。
    #[error("授权码签名校验失败")]
    BadSignature,
    /// 授权码已过期。
    #[error("授权码已过期")]
    Expired,
}

/// 付费能力提供者。核心通过此 trait 查询功能是否解锁；免费版用 [`FreeProvider`]，
/// 专业版由闭源 `ms-premium` 注入真实实现。
pub trait PremiumProvider: Send + Sync {
    /// 当前授权等级。
    fn tier(&self) -> Tier;
    /// 最大同时互联设备数（含本机）。免费版恒为 [`FREE_MAX_DEVICES`]。
    fn max_devices(&self) -> u32;
    /// 文件传输是否解锁。
    fn file_transfer_enabled(&self) -> bool;
    /// 当前已加载的授权信息（未激活 / 免费为 `None`）。
    fn license_info(&self) -> Option<LicenseInfo>;
    /// 用 license key 激活；成功返回授权信息并持久化生效。
    /// 免费版（未带 premium 实现）返回 [`LicenseError::PremiumBuildRequired`]。
    fn activate(&self, key: &str) -> Result<LicenseInfo, LicenseError>;
}

/// 免费版设备上限（含本机）。
pub const FREE_MAX_DEVICES: u32 = 2;

/// 免费版能力：所有付费功能关闭，限 2 台，无法激活。
#[derive(Debug, Default, Clone, Copy)]
pub struct FreeProvider;

impl PremiumProvider for FreeProvider {
    fn tier(&self) -> Tier {
        Tier::Free
    }
    fn max_devices(&self) -> u32 {
        FREE_MAX_DEVICES
    }
    fn file_transfer_enabled(&self) -> bool {
        false
    }
    fn license_info(&self) -> Option<LicenseInfo> {
        None
    }
    fn activate(&self, _key: &str) -> Result<LicenseInfo, LicenseError> {
        Err(LicenseError::PremiumBuildRequired)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 免费版能力受限() {
        let p = FreeProvider;
        assert_eq!(p.tier(), Tier::Free);
        assert_eq!(p.max_devices(), 2);
        assert!(!p.file_transfer_enabled());
        assert!(p.license_info().is_none());
        assert!(matches!(
            p.activate("any"),
            Err(LicenseError::PremiumBuildRequired)
        ));
    }

    #[test]
    fn 授权信息功能位与有效期() {
        let info = LicenseInfo {
            tier: Tier::Pro,
            max_devices: 5,
            features: features::FILE_TRANSFER | features::MULTI_DEVICE,
            licensee: "a@b.com".into(),
            expires_at: Some(100),
        };
        assert!(info.has_feature(features::FILE_TRANSFER));
        assert!(info.has_feature(features::MULTI_DEVICE));
        assert!(info.is_valid_at(100), "等于过期时刻仍有效");
        assert!(!info.is_valid_at(101), "超过即失效");

        let perpetual = LicenseInfo {
            expires_at: None,
            ..info
        };
        assert!(perpetual.is_valid_at(i64::MAX), "永久授权恒有效");
    }
}
