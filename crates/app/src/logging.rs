//! tracing 日志初始化：stderr + 按天滚动文件（SPEC.md §4/§7）。
//! 保留 7 天滚动由 tracing-appender 的 Rotation::DAILY + 后续清理任务保障
//! （清理在 T-702 浸泡工具中实现，当前先按天分文件）。

use std::path::{Path, PathBuf};

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

/// 默认日志目录（SPEC.md §4）。开发期应以 --log-dir ./logs 覆盖（CLAUDE.md §1.7）。
pub fn default_log_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Library/Logs/FesilentMouse"))
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("FesilentMouse/logs"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        None
    }
}

/// 初始化全局日志。返回的 guard 必须存活到进程结束，否则文件日志会丢尾。
pub fn init(log_dir: &Path) -> std::io::Result<WorkerGuard> {
    std::fs::create_dir_all(log_dir)?;
    let file = tracing_appender::rolling::daily(log_dir, "mouseshare.log");
    let (file_writer, guard) = tracing_appender::non_blocking(file);

    // RUST_LOG 优先；默认 mouseshare=info（SPEC §7：target 统一以 mouseshare:: 前缀）
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("mouseshare=info"));

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_writer(std::io::stderr))
        .with(fmt::layer().with_ansi(false).with_writer(file_writer))
        .init();
    Ok(guard)
}
