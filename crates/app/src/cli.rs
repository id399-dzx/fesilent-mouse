//! CLI 参数解析（SPEC.md §7 调试参数）。
//! 参数极少，手写解析以避免引入基线外依赖（CLAUDE.md §2）。

use std::path::PathBuf;

#[derive(Debug, Default, Clone)]
pub struct Args {
    /// 不启动托盘（headless 测试）
    pub no_tray: bool,
    /// 回环模式：本机双实例互为对端（DEVELOPMENT.md §11.2）
    pub loopback: bool,
    /// QUIC 监听端口覆盖（默认 SPEC §2 的 53728）
    pub port: Option<u16>,
    /// 日志目录覆盖（默认 SPEC §4 平台路径；开发期传 ./logs）
    pub log_dir: Option<PathBuf>,
    /// 录制捕获事件到文件
    pub record: Option<PathBuf>,
    /// 回放事件文件
    pub replay: Option<PathBuf>,
    /// 自检：验证平台能力（当前：安全存储读写往返），打印 PASS/FAIL 后退出
    pub selftest: bool,
    /// 守护进程：监听等待对端连接
    pub serve: bool,
    /// 守护进程：主动连接对端 IP:端口
    pub connect: Option<String>,
    /// 对端方位（right/left/top/bottom，默认 right）
    pub side: String,
    /// 本机屏幕尺寸 WxH（v1 手动指定，默认 1920x1080）
    pub screen: (f64, f64),
    /// 对端屏幕尺寸 WxH（默认 1920x1080）
    pub peer_screen: (f64, f64),
    /// 修饰键功能映射：交换 Ctrl↔Cmd（默认开，符合跨平台直觉：Win 的 Ctrl+C
    /// 在 mac 触发 Cmd+C）。传 --no-swap-modifiers 关闭，改为按物理位置原样映射。
    pub swap_modifiers: bool,
    /// 从属端：只接受对端控制，本端光标到边缘**不**主动发起穿越（单向主从）。
    pub no_takeover: bool,
    /// 反转滚轮上下方向（注入端对纵向滚动取反）。
    pub invert_scroll: bool,
    /// 边缘"刻意触发"推挤距离（像素，T-511/FR-205/207）：贴住朝对端那条边后，须继续
    /// 朝该边推够此距离才穿越，避免一碰边就穿越（够不着菜单栏/任务栏）。0 = 贴边即穿越。
    /// 默认 100。
    pub edge_push: f64,
    /// 交还控制权"刻意触发"推挤距离（像素，T-515）：作为控制方时，光标回到返回边后须
    /// 继续推够此距离才交还控制权，避免贴对端那条边就瞬间弹回。0 = 贴边即交还。默认 60。
    pub return_push: f64,
    /// 关闭剪贴板同步（FR-404，默认开启同步）。
    pub no_clipboard: bool,
    /// 启动系统托盘 UI（图形化启停 daemon）；需以 tray feature 构建。
    pub tray: bool,
}

#[derive(Debug, thiserror::Error)]
#[error("参数错误: {0}")]
pub struct ArgError(String);

pub fn parse(argv: impl Iterator<Item = String>) -> Result<Args, ArgError> {
    let mut args = Args {
        side: "right".to_string(),
        screen: (1920.0, 1080.0),
        peer_screen: (1920.0, 1080.0),
        swap_modifiers: true,
        edge_push: 100.0,
        return_push: 60.0,
        ..Default::default()
    };
    let mut it = argv.peekable();
    while let Some(flag) = it.next() {
        match flag.as_str() {
            "--no-tray" => args.no_tray = true,
            "--loopback" => args.loopback = true,
            "--port" => {
                let v = take_value(&mut it, &flag)?;
                args.port = Some(
                    v.parse()
                        .map_err(|_| ArgError(format!("--port 需要 1-65535 的整数，得到 {v}")))?,
                );
            }
            "--log-dir" => args.log_dir = Some(take_value(&mut it, &flag)?.into()),
            "--record" => args.record = Some(take_value(&mut it, &flag)?.into()),
            "--replay" => args.replay = Some(take_value(&mut it, &flag)?.into()),
            "--selftest" => args.selftest = true,
            "--serve" => args.serve = true,
            "--connect" => args.connect = Some(take_value(&mut it, &flag)?),
            "--side" => args.side = take_value(&mut it, &flag)?,
            "--no-swap-modifiers" => args.swap_modifiers = false,
            "--no-takeover" => args.no_takeover = true,
            "--invert-scroll" => args.invert_scroll = true,
            "--edge-push" => {
                let v = take_value(&mut it, &flag)?;
                args.edge_push = v
                    .parse::<f64>()
                    .ok()
                    .filter(|n| *n >= 0.0)
                    .ok_or_else(|| ArgError(format!("--edge-push 需要 ≥0 的数值，得到 {v}")))?;
            }
            "--return-push" => {
                let v = take_value(&mut it, &flag)?;
                args.return_push =
                    v.parse::<f64>().ok().filter(|n| *n >= 0.0).ok_or_else(|| {
                        ArgError(format!("--return-push 需要 ≥0 的数值，得到 {v}"))
                    })?;
            }
            "--no-clipboard" => args.no_clipboard = true,
            "--tray" => args.tray = true,
            "--screen" => args.screen = parse_wxh(&take_value(&mut it, &flag)?)?,
            "--peer-screen" => args.peer_screen = parse_wxh(&take_value(&mut it, &flag)?)?,
            other => return Err(ArgError(format!("未知参数 {other}"))),
        }
    }
    if args.record.is_some() && args.replay.is_some() {
        return Err(ArgError("--record 与 --replay 不能同时使用".into()));
    }
    Ok(args)
}

fn take_value(
    it: &mut std::iter::Peekable<impl Iterator<Item = String>>,
    flag: &str,
) -> Result<String, ArgError> {
    it.next().ok_or_else(|| ArgError(format!("{flag} 缺少值")))
}

/// 解析 "1920x1080" 为 (宽, 高)。
fn parse_wxh(s: &str) -> Result<(f64, f64), ArgError> {
    let (w, h) = s
        .split_once(['x', 'X'])
        .ok_or_else(|| ArgError(format!("屏幕尺寸需 WxH 格式，得到 {s}")))?;
    let w: f64 = w
        .trim()
        .parse()
        .map_err(|_| ArgError(format!("无效宽度: {w}")))?;
    let h: f64 = h
        .trim()
        .parse()
        .map_err(|_| ArgError(format!("无效高度: {h}")))?;
    if !w.is_finite()
        || !h.is_finite()
        || !(2.0..=100_000.0).contains(&w)
        || !(2.0..=100_000.0).contains(&h)
    {
        return Err(ArgError("屏幕宽高必须是 2–100000 范围内的有限数值".into()));
    }
    Ok((w, h))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn p(s: &[&str]) -> Result<Args, ArgError> {
        parse(s.iter().map(|s| s.to_string()))
    }

    #[test]
    fn 默认值() {
        let a = p(&[]).unwrap();
        assert!(!a.no_tray && !a.loopback && a.port.is_none());
    }

    #[test]
    fn 全参数() {
        let a = p(&[
            "--no-tray",
            "--loopback",
            "--port",
            "60000",
            "--log-dir",
            "./logs",
        ])
        .unwrap();
        assert!(a.no_tray && a.loopback);
        assert_eq!(a.port, Some(60000));
        assert_eq!(a.log_dir.unwrap(), PathBuf::from("./logs"));
    }

    #[test]
    fn 非法端口与未知参数() {
        assert!(p(&["--port", "abc"]).is_err());
        assert!(p(&["--wat"]).is_err());
        assert!(p(&["--port"]).is_err());
    }

    #[test]
    fn 录制回放互斥() {
        assert!(p(&["--record", "a", "--replay", "b"]).is_err());
    }

    #[test]
    fn 屏幕参数拒绝非法边界避免回退尺寸进入引擎() {
        for flag in ["--screen", "--peer-screen"] {
            for size in [
                "0x900",
                "1920x0",
                "-1x900",
                "1x900",
                "NaNx900",
                "1920xinf",
                "100001x900",
                "1920x100001",
            ] {
                assert!(p(&[flag, size]).is_err(), "{flag} {size} 必须拒绝");
            }
            let parsed = p(&[flag, "2560X1440"]).unwrap();
            assert_eq!(
                if flag == "--screen" {
                    parsed.screen
                } else {
                    parsed.peer_screen
                },
                (2560.0, 1440.0)
            );
        }
        assert_eq!(
            p(&["--screen", "2x100000"]).unwrap().screen,
            (2.0, 100000.0)
        );
        assert_eq!(
            p(&["--peer-screen", "2560x1440"]).unwrap().peer_screen,
            (2560.0, 1440.0)
        );
    }

    #[test]
    fn 边缘推挤默认与解析() {
        assert_eq!(p(&[]).unwrap().edge_push, 100.0, "默认推挤 100px");
        assert_eq!(
            p(&["--edge-push", "0"]).unwrap().edge_push,
            0.0,
            "0 关闭推挤"
        );
        assert_eq!(p(&["--edge-push", "60"]).unwrap().edge_push, 60.0);
        assert!(p(&["--edge-push", "-5"]).is_err(), "负值非法");
        assert!(p(&["--edge-push", "abc"]).is_err());
    }

    #[test]
    fn 交还推挤默认与解析() {
        assert_eq!(p(&[]).unwrap().return_push, 60.0, "默认交还推挤 60px");
        assert_eq!(
            p(&["--return-push", "0"]).unwrap().return_push,
            0.0,
            "0 关闭交还推挤"
        );
        assert_eq!(p(&["--return-push", "80"]).unwrap().return_push, 80.0);
        assert!(p(&["--return-push", "-5"]).is_err(), "负值非法");
    }
}
