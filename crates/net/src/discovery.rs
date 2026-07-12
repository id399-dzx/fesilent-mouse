//! mDNS/DNS-SD 服务发现（mdns-sd；DEVELOPMENT.md §1.1，FR-101/102）。
//!
//! 服务类型与 TXT 字段见 SPEC.md §2：`_mouseshare._udp.local.`，TXT = ver/id/name。
//! 发现失败的手动 IP 兜底由上层直接用 `Endpoint::connect(addr)` 完成（FR-102）。

use std::collections::HashMap;
use std::net::IpAddr;

use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use tokio::sync::mpsc;

use crate::quic::NetError;

/// mDNS 服务类型（SPEC §2）。
pub const SERVICE_TYPE: &str = "_mouseshare._udp.local.";

const TXT_VER: &str = "ver";
const TXT_ID: &str = "id";
const TXT_NAME: &str = "name";

/// 发现到的对端。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerInfo {
    pub device_id: String,
    pub name: String,
    pub proto_ver: u16,
    pub addrs: Vec<IpAddr>,
    pub port: u16,
}

/// 构造广播 TXT 字段（SPEC §2）。
pub fn build_txt(device_id: &str, name: &str, ver: u16) -> HashMap<String, String> {
    HashMap::from([
        (TXT_VER.to_string(), ver.to_string()),
        (TXT_ID.to_string(), device_id.to_string()),
        (TXT_NAME.to_string(), name.to_string()),
    ])
}

/// 从"键→值"查询函数解析 TXT；缺字段或版本非法返回 None。
pub fn parse_txt(get: impl Fn(&str) -> Option<String>) -> Option<(String, String, u16)> {
    let id = get(TXT_ID)?;
    let name = get(TXT_NAME).unwrap_or_default();
    let ver = get(TXT_VER)?.parse().ok()?;
    Some((id, name, ver))
}

fn peer_from_info(info: &ServiceInfo) -> Option<PeerInfo> {
    let (device_id, name, proto_ver) =
        parse_txt(|k| info.get_property_val_str(k).map(|s| s.to_string()))?;
    let addrs: Vec<IpAddr> = info.get_addresses().iter().copied().collect();
    if addrs.is_empty() {
        return None;
    }
    Some(PeerInfo {
        device_id,
        name,
        proto_ver,
        addrs,
        port: info.get_port(),
    })
}

/// mDNS 守护：广播本机服务 + 浏览对端。
pub struct Discovery {
    daemon: ServiceDaemon,
    instance: String,
}

impl Discovery {
    pub fn new() -> Result<Self, NetError> {
        let daemon = ServiceDaemon::new().map_err(|e| NetError::Discovery(e.to_string()))?;
        Ok(Self {
            daemon,
            instance: String::new(),
        })
    }

    /// 广播本机服务（FR-101）。`device_id` 同时用作实例名以保证唯一。
    pub fn advertise(&mut self, device_id: &str, name: &str, port: u16) -> Result<(), NetError> {
        let props = build_txt(device_id, name, ms_proto::PROTO_VER);
        // host_name 用 device_id，地址用自动检测（传空让 mdns-sd 探测本机网卡）
        let host = format!("{device_id}.local.");
        let info = ServiceInfo::new(SERVICE_TYPE, device_id, &host, "", port, props)
            .map_err(|e| NetError::Discovery(e.to_string()))?
            .enable_addr_auto();
        self.instance = format!("{device_id}.{SERVICE_TYPE}");
        self.daemon
            .register(info)
            .map_err(|e| NetError::Discovery(e.to_string()))?;
        Ok(())
    }

    /// 启动浏览，返回收取已解析对端的异步通道。后台线程把 mdns-sd 的同步事件
    /// 转发到 tokio 通道；自身广播（同 device_id）会被过滤掉。
    pub fn browse(&self, own_device_id: String) -> Result<mpsc::Receiver<PeerInfo>, NetError> {
        let receiver = self
            .daemon
            .browse(SERVICE_TYPE)
            .map_err(|e| NetError::Discovery(e.to_string()))?;
        let (tx, rx) = mpsc::channel(16);
        std::thread::Builder::new()
            .name("ms-mdns-browse".into())
            .spawn(move || {
                while let Ok(event) = receiver.recv() {
                    if let ServiceEvent::ServiceResolved(info) = event
                        && let Some(peer) = peer_from_info(&info)
                        && peer.device_id != own_device_id
                        && tx.blocking_send(peer).is_err()
                    {
                        break; // 接收端已关闭
                    }
                }
            })
            .map_err(NetError::Io)?;
        Ok(rx)
    }

    /// 停止广播与浏览。
    pub fn shutdown(&self) {
        if !self.instance.is_empty() {
            let _ = self.daemon.unregister(&self.instance);
        }
        let _ = self.daemon.shutdown();
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn txt_构造与解析往返() {
        let txt = build_txt("dev-123", "我的 Mac", 1);
        let (id, name, ver) = parse_txt(|k| txt.get(k).cloned()).unwrap();
        assert_eq!(id, "dev-123");
        assert_eq!(name, "我的 Mac");
        assert_eq!(ver, 1);
    }

    #[test]
    fn 缺id或版本非法返回none() {
        // 缺 id
        let t1 = HashMap::from([(TXT_VER.to_string(), "1".to_string())]);
        assert!(parse_txt(|k| t1.get(k).cloned()).is_none());
        // 版本非数字
        let t2 = HashMap::from([
            (TXT_ID.to_string(), "x".to_string()),
            (TXT_VER.to_string(), "abc".to_string()),
        ]);
        assert!(parse_txt(|k| t2.get(k).cloned()).is_none());
    }

    #[test]
    fn 缺name容忍为空() {
        let t = HashMap::from([
            (TXT_ID.to_string(), "x".to_string()),
            (TXT_VER.to_string(), "2".to_string()),
        ]);
        let (id, name, ver) = parse_txt(|k| t.get(k).cloned()).unwrap();
        assert_eq!((id.as_str(), name.as_str(), ver), ("x", "", 2));
    }

    #[test]
    fn 服务类型符合spec() {
        assert_eq!(SERVICE_TYPE, "_mouseshare._udp.local.");
    }
}
