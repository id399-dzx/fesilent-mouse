//! macOS 开机自启（T-604，FR-805）。
//!
//! 用 per-user **LaunchAgent**：在 `~/Library/LaunchAgents/com.mouseshare.app.plist`
//! 写一份 plist，`RunAtLoad=true` 使其在用户登录时拉起托盘进程。启用时另尝试
//! `launchctl load -w` 让本次会话即时生效（失败仅告警，下次登录仍会按 plist 生效）。
//!
//! 启动项指向**当前可执行文件**（`current_exe`）并附 `--tray`：装在 .app 里时即指向
//! `MouseShare.app/Contents/MacOS/mouseshare`，辅助功能授权随该二进制延续。

use std::path::{Path, PathBuf};
use std::process::Command;

use ms_platform::{Autostart, PlatformError, Result};

/// LaunchAgent 的 Label（与 bundle id 一致，SPEC §4）。
const LABEL: &str = "com.mouseshare.app";

pub struct MacAutostart {
    /// 目标 plist 路径（`~/Library/LaunchAgents/<LABEL>.plist`）。
    plist_path: PathBuf,
    /// 写入 plist 的启动程序路径（默认当前可执行文件）。
    program: PathBuf,
    /// 是否调用 `launchctl` 即时加载/卸载（单测置 false，避免动系统 launchd）。
    manage_launchctl: bool,
}

impl MacAutostart {
    pub fn new() -> Result<Self> {
        let home = std::env::var_os("HOME")
            .ok_or_else(|| PlatformError::Api("无法读取 HOME 环境变量".into()))?;
        let plist_path = PathBuf::from(home)
            .join("Library/LaunchAgents")
            .join(format!("{LABEL}.plist"));
        let program = std::env::current_exe().map_err(PlatformError::Io)?;
        Ok(Self {
            plist_path,
            program,
            manage_launchctl: true,
        })
    }

    /// 测试构造器：完全指定路径/程序，且不触碰 launchctl。
    #[cfg(test)]
    fn with_parts(plist_path: PathBuf, program: PathBuf) -> Self {
        Self {
            plist_path,
            program,
            manage_launchctl: false,
        }
    }

    /// 生成 LaunchAgent plist 文本。
    fn plist_contents(&self) -> String {
        let prog = xml_escape(&self.program.to_string_lossy());
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{prog}</string>
    <string>--tray</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>ProcessType</key><string>Interactive</string>
</dict>
</plist>
"#
        )
    }
}

impl Autostart for MacAutostart {
    fn set_enabled(&mut self, on: bool) -> Result<()> {
        if on {
            if let Some(dir) = self.plist_path.parent() {
                std::fs::create_dir_all(dir).map_err(PlatformError::Io)?;
            }
            std::fs::write(&self.plist_path, self.plist_contents()).map_err(PlatformError::Io)?;
            if self.manage_launchctl {
                launchctl(&["load", "-w"], &self.plist_path);
            }
        } else {
            if self.manage_launchctl && self.plist_path.exists() {
                launchctl(&["unload", "-w"], &self.plist_path);
            }
            match std::fs::remove_file(&self.plist_path) {
                Ok(()) => {}
                // 已不存在视作成功（幂等）
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(PlatformError::Io(e)),
            }
        }
        Ok(())
    }

    fn is_enabled(&mut self) -> Result<bool> {
        Ok(self.plist_path.exists())
    }
}

/// 调用 `launchctl <args> <plist>`，失败仅告警（即时生效是尽力而为）。
fn launchctl(args: &[&str], plist: &Path) {
    let res = Command::new("launchctl").args(args).arg(plist).status();
    match res {
        Ok(s) if s.success() => {}
        Ok(s) => tracing::warn!(
            target: "mouseshare::mac",
            code = ?s.code(),
            "launchctl {args:?} 非零退出（下次登录仍按 plist 生效）"
        ),
        Err(e) => tracing::warn!(
            target: "mouseshare::mac",
            error = %e,
            "launchctl 调用失败（下次登录仍按 plist 生效）"
        ),
    }
}

/// 转义 XML 文本节点中的特殊字符（路径可能含 & < > " '）。
fn xml_escape(s: &str) -> String {
    s.chars()
        .flat_map(|c| match c {
            '&' => "&amp;".chars().collect::<Vec<_>>(),
            '<' => "&lt;".chars().collect(),
            '>' => "&gt;".chars().collect(),
            '"' => "&quot;".chars().collect(),
            '\'' => "&apos;".chars().collect(),
            other => vec![other],
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn tmp_plist() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        // 进程内原子计数器保证唯一：SystemTime 纳秒在 macOS 精度不足，并行测试会撞名
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let mut p = std::env::temp_dir();
        let uniq = format!(
            "ms-autostart-test-{}-{}.plist",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        );
        p.push(uniq);
        p
    }

    #[test]
    fn 启用写plist_禁用删除_幂等() {
        let path = tmp_plist();
        let mut a = MacAutostart::with_parts(path.clone(), PathBuf::from("/tmp/mouseshare"));

        assert!(!a.is_enabled().unwrap(), "初始未启用");
        a.set_enabled(true).unwrap();
        assert!(a.is_enabled().unwrap(), "启用后 plist 应存在");

        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("/tmp/mouseshare"), "plist 应含程序路径");
        assert!(body.contains("--tray"), "plist 应以 --tray 启动");
        assert!(body.contains(LABEL), "plist 应含 Label");
        assert!(body.contains("<key>RunAtLoad</key><true/>"), "应 RunAtLoad");

        a.set_enabled(false).unwrap();
        assert!(!a.is_enabled().unwrap(), "禁用后 plist 应删除");
        // 再禁用一次应幂等成功
        a.set_enabled(false).unwrap();
    }

    #[test]
    fn 程序路径中的特殊字符被转义() {
        let path = tmp_plist();
        let weird = PathBuf::from("/Users/a&b/My <App>/mouseshare");
        let mut a = MacAutostart::with_parts(path.clone(), weird);
        a.set_enabled(true).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("a&amp;b"), "& 应转义");
        assert!(body.contains("My &lt;App&gt;"), "<> 应转义");
        assert!(!body.contains("My <App>"), "不应出现未转义的尖括号");
        a.set_enabled(false).unwrap();
    }

    #[test]
    fn xml转义覆盖五类字符() {
        assert_eq!(xml_escape(r#"&<>"'"#), "&amp;&lt;&gt;&quot;&apos;");
        assert_eq!(xml_escape("normal/path"), "normal/path");
    }
}
