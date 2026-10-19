//! 配置持久化（SPEC §4，T-602）：`config.json`。
//!
//! 顶层带 `config_ver`，所有字段 serde default，保证旧配置可读、新增字段不破坏。
//! 路径：mac `~/Library/Application Support/FesilentMouse/config.json`，
//! Windows `%APPDATA%\FesilentMouse\config.json`。

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::cli::Args;

pub(crate) const DEFAULT_PEER_PORT: u16 = 53728;

/// 空地址启用自动发现；裸 IPv4 补默认端口，显式端口必须为 1..=65535。
/// 不把无效输入拼进错误文案，避免把误粘贴的内容显示到日志或界面。
pub(crate) fn normalize_peer_address(input: &str) -> Result<Option<String>, String> {
    let input = input.trim();
    if input.is_empty() {
        return Ok(None);
    }
    if let Ok(addr) = input.parse::<SocketAddr>() {
        if addr.is_ipv6() {
            return Err(
                "请使用局域网 IPv4 地址，例如 192.168.1.20。当前连接服务暂不支持 IPv6。".into(),
            );
        }
        if addr.port() == 0 {
            return Err("对端端口必须为 1–65535，默认端口是 53728。".into());
        }
        return Ok(Some(input.to_string()));
    }
    if let Ok(ip) = input.parse::<IpAddr>() {
        if ip.is_ipv6() {
            return Err(
                "请使用局域网 IPv4 地址，例如 192.168.1.20。当前连接服务暂不支持 IPv6。".into(),
            );
        }
        return Ok(Some(SocketAddr::new(ip, DEFAULT_PEER_PORT).to_string()));
    }
    Err("请使用有效的局域网 IPv4 地址，例如 192.168.1.20 或 192.168.1.20:53728。Mac 端可留空并启用互控。".into())
}

fn default_config_ver() -> u32 {
    1
}
fn default_side() -> String {
    "right".to_string()
}
fn default_true() -> bool {
    true
}
fn default_edge_push() -> f64 {
    100.0
}
fn default_return_push() -> f64 {
    60.0
}
fn default_clip_history() -> usize {
    crate::cliphist::DEFAULT_CAP
}
fn default_scroll_speed() -> f64 {
    1.0
}

/// 把纵向滚轮倍率规范到 `1.0..=3.0` 的 0.1 档位。
pub(crate) fn normalize_scroll_speed(value: f64) -> f64 {
    if !value.is_finite() {
        return default_scroll_speed();
    }
    ((value * 10.0).round() / 10.0).clamp(1.0, 3.0)
}

/// 持久化的用户设置（对应 daemon 的可配置项）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_config_ver")]
    pub config_ver: u32,
    /// 对端 IPv4 或 `IPv4:端口`；为空则本端监听（serve），否则主动连接。
    #[serde(default)]
    pub connect: Option<String>,
    /// 对端方位 right/left/top/bottom。
    #[serde(default = "default_side")]
    pub side: String,
    /// 从属端：只被控、不主动夺取控制。
    #[serde(default)]
    pub no_takeover: bool,
    /// 反转滚轮上下方向。
    #[serde(default)]
    pub invert_scroll: bool,
    /// Windows 控制 Mac 时的纵向滚轮倍率；与反转滚轮独立。
    #[serde(default = "default_scroll_speed")]
    pub scroll_speed: f64,
    /// 交换 Ctrl↔Cmd（默认开）。
    #[serde(default = "default_true")]
    pub swap_modifiers: bool,
    /// 打开软件即自动开始互控，由设置中的复选框决定。
    #[serde(default)]
    pub auto_start: bool,
    /// 剪贴板同步开关（FR-404，默认开）。
    #[serde(default = "default_true")]
    pub clipboard: bool,
    /// 边缘"刻意触发"推挤距离（像素，T-511/FR-205/207）：贴边后须继续朝该边推够此距离
    /// 才穿越，避免一碰边就穿越。0 = 贴边即穿越。默认 100。
    #[serde(default = "default_edge_push")]
    pub edge_push: f64,
    /// 交还控制权"刻意触发"推挤距离（像素，T-515）：作为控制方时，光标回到返回边后须
    /// 继续朝该边推够此距离才交还控制权，避免贴对端那条边就瞬间弹回（够不着对端菜单栏）。
    /// 0 = 贴边即交还。默认 60。仅在本端作为控制方（反控对端）时生效。
    #[serde(default = "default_return_push")]
    pub return_push: f64,
    /// 剪贴板历史保留条数（T-808，仅内存不落盘；0 = 关闭历史）。
    #[serde(default = "default_clip_history")]
    pub clip_history: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            config_ver: 1,
            connect: None,
            side: default_side(),
            no_takeover: false,
            invert_scroll: false,
            scroll_speed: default_scroll_speed(),
            swap_modifiers: true,
            auto_start: false,
            clipboard: true,
            edge_push: default_edge_push(),
            return_push: default_return_push(),
            clip_history: default_clip_history(),
        }
    }
}

impl Config {
    /// 从 CLI 参数提炼一份配置（首次启动用 CLI 值初始化配置文件）。
    pub fn from_args(args: &Args) -> Self {
        Self {
            config_ver: 1,
            connect: args.connect.clone(),
            side: args.side.clone(),
            no_takeover: args.no_takeover,
            invert_scroll: args.invert_scroll,
            scroll_speed: default_scroll_speed(),
            swap_modifiers: args.swap_modifiers,
            auto_start: false,
            clipboard: !args.no_clipboard,
            edge_push: args.edge_push,
            return_push: args.return_push,
            clip_history: default_clip_history(),
        }
    }

    /// 把配置转成一份 daemon 运行参数；`log_dir` 单独传入（运行时项不进配置）。
    pub fn to_args(&self, log_dir: Option<PathBuf>) -> Args {
        // 非法值保留到 daemon 启动前统一报错，不能静默退回自动发现。
        let connect = match normalize_peer_address(self.connect.as_deref().unwrap_or_default()) {
            Ok(connect) => connect,
            Err(_) => self.connect.clone(),
        };
        Args {
            serve: connect.is_none(),
            connect,
            side: self.side.clone(),
            no_takeover: self.no_takeover,
            invert_scroll: self.invert_scroll,
            swap_modifiers: self.swap_modifiers,
            no_clipboard: !self.clipboard,
            edge_push: self.edge_push,
            return_push: self.return_push,
            log_dir,
            screen: (1920.0, 1080.0),
            peer_screen: (1920.0, 1080.0),
            ..Default::default()
        }
    }
}

/// 配置文件路径（无法确定平台目录时返回 None）。
pub fn config_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join("Library/Application Support/FesilentMouse/config.json"))
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("FesilentMouse\\config.json"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        None
    }
}

/// 读取配置；文件缺失/损坏时返回默认值（保证总能启动）。
pub fn load() -> Config {
    let mut cfg: Config = config_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    cfg.scroll_speed = normalize_scroll_speed(cfg.scroll_speed);
    cfg
}

/// 写入配置（自动建目录）。
pub fn save(cfg: &Config) -> Result<(), String> {
    let path = config_path().ok_or_else(|| "无法确定配置目录".to_string())?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let mut normalized = cfg.clone();
    normalized.scroll_speed = normalize_scroll_speed(normalized.scroll_speed);
    let json = serde_json::to_string_pretty(&normalized).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn 默认配置可序列化往返() {
        let cfg = Config::default();
        let json = serde_json::to_string(&cfg).unwrap();
        let back: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(back.config_ver, 1);
        assert_eq!(back.side, "right");
        assert!(back.swap_modifiers);
        assert!(!back.no_takeover);
        assert_eq!(back.scroll_speed, 1.0);
    }

    #[test]
    fn 缺字段用默认值补齐() {
        // 仅给部分字段的旧配置，缺失字段应走 serde default
        let json = r#"{"config_ver":1,"side":"top","no_takeover":true}"#;
        let cfg: Config = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.side, "top");
        assert!(cfg.no_takeover);
        assert!(cfg.swap_modifiers, "缺失的 swap_modifiers 应默认 true");
        assert_eq!(cfg.connect, None);
        assert_eq!(cfg.edge_push, 100.0, "缺失的 edge_push 应默认 100");
        assert_eq!(cfg.return_push, 60.0, "缺失的 return_push 应默认 60");
        assert_eq!(cfg.scroll_speed, 1.0, "旧配置缺倍率时应保持原速");
    }

    #[test]
    fn 滚轮倍率按范围和步进规范化() {
        assert_eq!(normalize_scroll_speed(f64::NAN), 1.0);
        assert_eq!(normalize_scroll_speed(f64::INFINITY), 1.0);
        assert_eq!(normalize_scroll_speed(0.99), 1.0);
        assert_eq!(normalize_scroll_speed(1.04), 1.0);
        assert_eq!(normalize_scroll_speed(1.05), 1.1);
        assert_eq!(normalize_scroll_speed(2.94), 2.9);
        assert_eq!(normalize_scroll_speed(2.95), 3.0);
        assert_eq!(normalize_scroll_speed(3.01), 3.0);
    }

    #[test]
    fn connect有值则to_args为连接模式() {
        let cfg = Config {
            connect: Some("192.168.0.5:53728".into()),
            side: "bottom".into(),
            ..Config::default()
        };
        let args = cfg.to_args(None);
        assert!(!args.serve, "有 connect 应为连接模式");
        assert_eq!(args.connect.as_deref(), Some("192.168.0.5:53728"));
        assert_eq!(args.side, "bottom");

        let serve_cfg = Config::default();
        assert!(serve_cfg.to_args(None).serve, "无 connect 应为监听模式");
    }

    #[test]
    fn 对端地址留空启用自动发现() {
        for input in ["", "   ", "\t\r\n"] {
            assert_eq!(normalize_peer_address(input).unwrap(), None);
            let cfg = Config {
                connect: Some(input.into()),
                ..Config::default()
            };
            let args = cfg.to_args(None);
            assert!(args.serve);
            assert_eq!(args.connect, None);
        }
    }

    #[test]
    fn 裸ipv4补默认端口() {
        for (input, expected) in [
            (" 192.168.1.20 ", "192.168.1.20:53728"),
            ("127.0.0.1", "127.0.0.1:53728"),
        ] {
            assert_eq!(
                normalize_peer_address(input).unwrap().as_deref(),
                Some(expected)
            );
            let cfg = Config {
                connect: Some(input.into()),
                ..Config::default()
            };
            assert_eq!(cfg.to_args(None).connect.as_deref(), Some(expected));
        }
    }

    #[test]
    fn ipv6提示使用局域网ipv4而非接受无法连接的地址() {
        for input in [
            "::1",
            "2001:db8::123",
            "[::1]:53728",
            "[2001:db8::1]:65535",
            "[::1]:0",
        ] {
            let error = normalize_peer_address(input).unwrap_err();
            assert!(error.contains("请使用局域网 IPv4 地址"), "{input}: {error}");
        }
    }

    #[test]
    fn 显式有效端口保留且拒绝非法地址() {
        for input in ["127.0.0.1:1", "192.168.1.20:65535"] {
            assert_eq!(
                normalize_peer_address(input).unwrap().as_deref(),
                Some(input)
            );
        }
        for input in [
            "127.0.0.1:0",
            "[::1]:0",
            "127.0.0.1:65536",
            "127.0.0.1:-1",
            "127.0.0.1:",
            "999.1.1.1",
            "localhost:53728",
            "https://192.168.1.20",
            "[::1]",
            "[::1]:abc",
            "192.168.1.20:53728:1",
            "private pasted text",
        ] {
            assert!(normalize_peer_address(input).is_err(), "应拒绝 {input}");
        }
        let error = normalize_peer_address("private pasted text").unwrap_err();
        assert!(!error.contains("private pasted text"));
        let cfg = Config {
            connect: Some("invalid".into()),
            ..Config::default()
        };
        let args = cfg.to_args(None);
        assert!(!args.serve, "非法地址不能静默变成监听模式");
        assert_eq!(args.connect.as_deref(), Some("invalid"));
    }
}
