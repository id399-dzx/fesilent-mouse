//! 诊断导出（T-810，NEXT-PLAN §2.C.6）：一键把本机日志（最近 7 天）+ config.json
//! + 版本/OS 信息汇集到桌面并压成 zip，便于用户回报问题时一次拖走。
//!
//! - 压包调系统自带 bsdtar（macOS 与 Windows 10 1803+ 都内置 `tar`，`-a` 按扩展名
//!   出 zip），**不引入新依赖**（SPEC §6 基线不动）；tar 不可用时降级保留文件夹。
//! - 只导本机：拉取对端日志需新增协议消息，受前向兼容约束（T-705 教训：旧端收到
//!   未知消息会解码失败拆连）暂不做，待协议前向兼容落地后再议。

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 日志收集窗口：只带最近 7 天内修改过的日志文件（滚动日志按天分文件，SPEC §7）。
const LOG_WINDOW: Duration = Duration::from_secs(7 * 24 * 3600);

/// 文件修改时间是否落在收集窗口内。mtime 晚于 now（时钟回拨等）视为在窗口内。
fn within_window(mtime: SystemTime, now: SystemTime) -> bool {
    now.duration_since(mtime)
        .map(|d| d <= LOG_WINDOW)
        .unwrap_or(true)
}

/// unix 秒 → (年, 月, 日, 时, 分, 秒)（UTC，civil_from_days 算法）。
/// 仅用于生成可读且可排序的文件名，不做本地时区换算（std 无时区能力，不为此引依赖）。
fn utc_parts(secs: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (h, mi, s) = (
        (rem / 3600) as u32,
        ((rem % 3600) / 60) as u32,
        (rem % 60) as u32,
    );
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d, h, mi, s)
}

/// 生成文件名时间戳（UTC）：`YYYYMMDD-HHMMSS`。
fn stamp(now: SystemTime) -> String {
    let secs = now
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, mo, d, h, mi, s) = utc_parts(secs);
    format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}")
}

/// 桌面目录（与 `ms_net::transfer::default_receive_dir` 同源的 HOME/USERPROFILE 推导）。
fn desktop_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from);
    #[cfg(not(target_os = "windows"))]
    let home = std::env::var_os("HOME").map(PathBuf::from);
    home.map(|h| h.join("Desktop"))
}

/// 汇集诊断文件到 `staging` 目录（纯文件操作，可单测）：
/// `logs/` = `log_dir` 下窗口内的日志文件；`config.json`；`info.txt`（版本/系统/时间）。
/// 返回收集到的文件数（不含 info.txt）。
pub fn assemble(
    staging: &Path,
    log_dir: Option<&Path>,
    config_path: Option<&Path>,
    now: SystemTime,
) -> std::io::Result<usize> {
    std::fs::create_dir_all(staging)?;
    let mut count = 0usize;

    if let Some(dir) = log_dir
        && let Ok(rd) = std::fs::read_dir(dir)
    {
        let logs_out = staging.join("logs");
        for e in rd.flatten() {
            let p = e.path();
            if !p.is_file() {
                continue;
            }
            let recent = e
                .metadata()
                .and_then(|m| m.modified())
                .map(|t| within_window(t, now))
                .unwrap_or(true); // 读不到 mtime 时宁多勿漏
            if !recent {
                continue;
            }
            std::fs::create_dir_all(&logs_out)?;
            if let Some(name) = p.file_name()
                && std::fs::copy(&p, logs_out.join(name)).is_ok()
            {
                count += 1;
            }
        }
    }

    if let Some(cp) = config_path
        && cp.is_file()
        && std::fs::copy(cp, staging.join("config.json")).is_ok()
    {
        count += 1;
    }

    let info = format!(
        "Fesilent Mouse 诊断包\n版本: {}\n系统: {} {}\n导出时间(UTC): {}\n日志目录: {}\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        stamp(now),
        log_dir.map_or_else(|| "（未定位）".into(), |p| p.display().to_string()),
    );
    std::fs::write(staging.join("info.txt"), info)?;
    Ok(count)
}

/// 一键导出到桌面。`explicit_log_dir` 为 CLI `--log-dir`（开发期），
/// 缺省回退 SPEC §4 平台默认日志目录。成功返回 zip（tar 不可用时为文件夹）路径。
pub fn export(explicit_log_dir: Option<&Path>) -> Result<PathBuf, String> {
    let desktop = desktop_dir().ok_or_else(|| "无法定位桌面目录".to_string())?;
    std::fs::create_dir_all(&desktop).map_err(|e| e.to_string())?;

    let log_dir = explicit_log_dir
        .map(Path::to_path_buf)
        .or_else(crate::logging::default_log_dir);
    let name = format!("FesilentMouse-诊断-{}", stamp(SystemTime::now()));
    let staging = ms_net::transfer::unique_path(&desktop, &name);
    let n = assemble(
        &staging,
        log_dir.as_deref(),
        crate::config::config_path().as_deref(),
        SystemTime::now(),
    )
    .map_err(|e| format!("汇集诊断文件失败: {e}"))?;
    tracing::info!(target: "mouseshare::app", files = n, dir = %staging.display(), "诊断包已汇集");

    // 压 zip：系统 bsdtar。folder name 刚由 unique_path 生成，file_name 必然存在。
    let Some(folder) = staging.file_name() else {
        return Ok(staging);
    };
    let zip = ms_net::transfer::unique_path(&desktop, &format!("{}.zip", folder.to_string_lossy()));
    let ok = std::process::Command::new("tar")
        .arg("-a")
        .arg("-c")
        .arg("-f")
        .arg(&zip)
        .arg("-C")
        .arg(&desktop)
        .arg(folder)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok && zip.is_file() {
        let _ = std::fs::remove_dir_all(&staging);
        Ok(zip)
    } else {
        tracing::warn!(target: "mouseshare::app", "系统 tar 压包失败，保留诊断文件夹");
        Ok(staging)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_parts_已知时间点() {
        assert_eq!(utc_parts(0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(utc_parts(86_400), (1970, 1, 2, 0, 0, 0));
        // 2024-01-01 00:00:00 UTC
        assert_eq!(utc_parts(1_704_067_200), (2024, 1, 1, 0, 0, 0));
        // 闰日 2000-02-29 00:00:00 UTC
        assert_eq!(utc_parts(951_782_400), (2000, 2, 29, 0, 0, 0));
        assert_eq!(
            stamp(UNIX_EPOCH + Duration::from_secs(1_704_067_200 + 3661)),
            "20240101-010101"
        );
    }

    #[test]
    fn 窗口判定_七天内含七天外不含() {
        let now = UNIX_EPOCH + Duration::from_secs(100 * 86_400);
        let six_days = now - Duration::from_secs(6 * 86_400);
        let eight_days = now - Duration::from_secs(8 * 86_400);
        let future = now + Duration::from_secs(60);
        assert!(within_window(six_days, now));
        assert!(!within_window(eight_days, now));
        assert!(within_window(future, now), "时钟回拨的未来 mtime 宁多勿漏");
    }

    #[test]
    fn assemble_收集日志与配置并写信息文件() {
        let base = std::env::temp_dir().join(format!("ms-diag-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let logs = base.join("logs-src");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(logs.join("mouseshare.log.2026-07-06"), "log-a").unwrap();
        std::fs::write(logs.join("mouseshare.log.2026-07-05"), "log-b").unwrap();
        let cfg = base.join("config.json");
        std::fs::write(&cfg, "{}").unwrap();

        let staging = base.join("out");
        let n = assemble(&staging, Some(&logs), Some(&cfg), SystemTime::now()).unwrap();
        assert_eq!(n, 3, "2 个日志 + 1 个配置");
        assert!(staging.join("logs/mouseshare.log.2026-07-06").is_file());
        assert!(staging.join("logs/mouseshare.log.2026-07-05").is_file());
        assert!(staging.join("config.json").is_file());
        let info = std::fs::read_to_string(staging.join("info.txt")).unwrap();
        assert!(
            info.contains(env!("CARGO_PKG_VERSION")),
            "info.txt 应含版本号"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn assemble_无日志目录也能出信息文件() {
        let base = std::env::temp_dir().join(format!("ms-diag-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let staging = base.join("out");
        let n = assemble(&staging, None, None, SystemTime::now()).unwrap();
        assert_eq!(n, 0);
        assert!(staging.join("info.txt").is_file());
        let _ = std::fs::remove_dir_all(&base);
    }
}
