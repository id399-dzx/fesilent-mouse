//! 只读取本机已连接的物理网卡，不向外部地址探测；网卡枚举放在后台线程。

use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

fn command_output(command: &mut std::process::Command) -> Option<std::process::Output> {
    use std::process::Stdio;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().ok(),
            Ok(None) if started.elapsed() < Duration::from_secs(4) => {
                std::thread::sleep(Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

fn lan_address(address: Ipv4Addr) -> bool {
    address.is_private()
}

fn normalized_addresses(mut addresses: Vec<Ipv4Addr>) -> Vec<String> {
    addresses.retain(|address| lan_address(*address));
    addresses.sort_unstable();
    addresses.dedup();
    addresses
        .into_iter()
        .map(|address| address.to_string())
        .collect()
}

pub(super) fn connection_addresses(addresses: &[String], port: u16) -> Vec<String> {
    addresses
        .iter()
        .filter_map(|address| address.parse::<Ipv4Addr>().ok())
        .filter(|address| lan_address(*address))
        .map(|address| SocketAddrV4::new(address, port).to_string())
        .collect()
}

#[cfg(any(target_os = "macos", test))]
fn parse_ifconfig(output: &str) -> Vec<String> {
    #[derive(Default)]
    struct Interface {
        physical: bool,
        up: bool,
        running: bool,
        active: Option<bool>,
        addresses: Vec<Ipv4Addr>,
    }
    impl Interface {
        fn take_addresses(&mut self, destination: &mut Vec<Ipv4Addr>) {
            if self.physical && self.up && self.running && self.active != Some(false) {
                destination.append(&mut self.addresses);
            }
        }
    }
    let mut interface = Interface::default();
    let mut addresses = Vec::new();
    for line in output.lines() {
        if !line.starts_with(char::is_whitespace) {
            interface.take_addresses(&mut addresses);
            interface = Interface::default();
            let Some((name, details)) = line.split_once(':') else {
                continue;
            };
            // macOS 的 Wi-Fi、以太网及 USB/雷雳网卡均使用 enN；排除 utun、bridge、
            // vmnet、awdl、llw、lo 等 VPN/虚拟/回环接口，即使它们也有私有 IPv4。
            interface.physical = name.strip_prefix("en").is_some_and(|suffix| {
                !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit())
            });
            let flags = details
                .split_once('<')
                .and_then(|(_, suffix)| suffix.split_once('>'))
                .map(|(flags, _)| flags)
                .unwrap_or_default();
            interface.up = flags.split(',').any(|flag| flag == "UP");
            interface.running = flags.split(',').any(|flag| flag == "RUNNING");
            continue;
        }
        let mut fields = line.split_whitespace();
        match fields.next() {
            Some("inet") => {
                if let Some(address) = fields.next().and_then(|text| text.parse().ok()) {
                    interface.addresses.push(address);
                }
            }
            Some("status:") => interface.active = Some(fields.next() == Some("active")),
            _ => {}
        }
    }
    interface.take_addresses(&mut addresses);
    normalized_addresses(addresses)
}

#[cfg(any(target_os = "windows", test))]
fn parse_address_lines(output: &str) -> Vec<String> {
    normalized_addresses(
        output
            .lines()
            .filter_map(|line| line.trim().parse::<Ipv4Addr>().ok())
            .collect(),
    )
}

pub(super) fn addresses() -> Vec<String> {
    #[cfg(target_os = "macos")]
    {
        command_output(&mut std::process::Command::new("/sbin/ifconfig"))
            .filter(|output| output.status.success())
            .map(|output| parse_ifconfig(&String::from_utf8_lossy(&output.stdout)))
            .unwrap_or_default()
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        // 对象属性不受系统语言影响，避免解析本地化 ipconfig 文案；只读取已连接
        // 物理网卡上 Preferred 状态的 IPv4，跳过断开、VPN/虚拟接口和失效旧地址。
        const QUERY: &str = "$ErrorActionPreference = 'Stop'; \
            Get-NetAdapter -Physical | Where-Object { $_.Status -eq 'Up' } | \
            ForEach-Object { Get-NetIPAddress -InterfaceIndex $_.ifIndex \
                -AddressFamily IPv4 -ErrorAction SilentlyContinue | \
                Where-Object { $_.AddressState -eq 'Preferred' } | \
                Select-Object -ExpandProperty IPAddress }";
        command_output(
            std::process::Command::new("powershell.exe")
                .args([
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    QUERY,
                ])
                .creation_flags(0x0800_0000),
        )
        .filter(|output| output.status.success())
        .map(|output| parse_address_lines(&String::from_utf8_lossy(&output.stdout)))
        .unwrap_or_default()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Vec::new()
    }
}

pub(super) fn monitor() -> Receiver<Vec<String>> {
    // 隐藏工作台时可能暂不消费消息，使用容量1的通道避免长期后台运行积压。
    let (sender, receiver) = mpsc::sync_channel(1);
    let _ = std::thread::Builder::new()
        .name("fesilent-local-network".into())
        .spawn(move || {
            while let Ok(()) | Err(mpsc::TrySendError::Full(_)) = sender.try_send(addresses()) {
                std::thread::sleep(Duration::from_secs(5));
            }
        });
    receiver
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 网卡地址仅保留私有ipv4并排序去重() {
        assert_eq!(
            parse_address_lines(
                "192.168.1.20\n10.0.0.2\n172.16.1.4\n192.168.1.20\n127.0.0.1\n\
                 169.254.1.2\n8.8.8.8\n0.0.0.0\n224.0.0.1\n255.255.255.255\n::1\n"
            ),
            ["10.0.0.2", "172.16.1.4", "192.168.1.20"]
        );
    }

    #[test]
    fn mac仅保留已连接物理网卡排除vpn虚拟和断开地址() {
        let output = "lo0: flags=8049<UP,LOOPBACK,RUNNING,MULTICAST> mtu 16384
\tinet 127.0.0.1 netmask 0xff000000
en0: flags=8863<UP,BROADCAST,SMART,RUNNING,SIMPLEX,MULTICAST> mtu 1500
\tinet 192.168.1.20 netmask 0xffffff00 broadcast 192.168.1.255
\tstatus: active
en1: flags=8863<UP,BROADCAST,SMART,RUNNING,SIMPLEX,MULTICAST> mtu 1500
\tinet 192.168.2.20 netmask 0xffffff00 broadcast 192.168.2.255
\tstatus: inactive
utun0: flags=8051<UP,POINTOPOINT,RUNNING,MULTICAST> mtu 1380
\tinet 10.8.0.1 netmask 0xffffff00
bridge100: flags=8863<UP,BROADCAST,SMART,RUNNING,SIMPLEX,MULTICAST> mtu 1500
\tinet 192.168.64.1 netmask 0xffffff00
en2: flags=8822<BROADCAST,SMART,SIMPLEX,MULTICAST> mtu 1500
\tinet 10.0.0.9 netmask 0xffffff00
en3: flags=8863<UP,BROADCAST,SMART,RUNNING,SIMPLEX,MULTICAST> mtu 1500
\tinet 8.8.8.8 netmask 0xffffff00
\tstatus: active
en4: flags=8863<UP,BROADCAST,SMART,RUNNING,SIMPLEX,MULTICAST> mtu 1500
\tinet 172.20.1.4 netmask 0xffffff00
\tstatus: active
";
        assert_eq!(parse_ifconfig(output), ["172.20.1.4", "192.168.1.20"]);
    }

    #[test]
    fn 网卡变化后不会保留已消失的旧ip() {
        let before = "en0: flags=8863<UP,BROADCAST,RUNNING> mtu 1500\n\tinet 192.168.1.20\n\tstatus: active\n";
        let after = "en0: flags=8863<UP,BROADCAST,RUNNING> mtu 1500\n\tinet 192.168.2.30\n\tstatus: active\n";
        assert_eq!(parse_ifconfig(before), ["192.168.1.20"]);
        assert_eq!(parse_ifconfig(after), ["192.168.2.30"]);
        assert!(parse_ifconfig("").is_empty());
    }

    #[test]
    fn 可复制连接地址使用实际监听端口() {
        let addresses = vec!["192.168.1.20".into(), "8.8.8.8".into()];
        assert_eq!(
            connection_addresses(&addresses, 53728),
            ["192.168.1.20:53728"]
        );
        assert_eq!(
            connection_addresses(&addresses, 60000),
            ["192.168.1.20:60000"]
        );
    }
}
