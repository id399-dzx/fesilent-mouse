//! mouseshare 主程序：组装各层 + 托盘（DEVELOPMENT.md §2.2）。
//!
//! Windows 发布构建用 GUI 子系统：双击/启动**不弹控制台黑窗**，纯托盘运行、托盘退出。
//! 调试构建保留控制台，便于看 println/日志。

// Windows release：GUI 子系统，不弹控制台窗口（纯托盘）。
#![cfg_attr(
    all(target_os = "windows", not(debug_assertions)),
    windows_subsystem = "windows"
)]

mod cli;
mod clip;
mod cliphist;
mod config;
mod daemon;
mod diag;
mod logging;
mod record;
mod selftest;
mod status;
mod transfers;
mod ui;
mod xfer;

use std::process::ExitCode;

fn main() -> ExitCode {
    // Windows：进程一启动就声明 Per-Monitor DPI 感知，统一坐标系为物理像素，
    // 消除低级钩子 pt(物理) 与 GetSystemMetrics/SetCursorPos(逻辑) 混用导致的
    // 边缘误判与 grab 漂移。必须在任何坐标/窗口 API 之前（即 main 最早）。
    #[cfg(target_os = "windows")]
    {
        // GUI 子系统下无有效 stdout/stderr：先把标准流重定向到 NUL，避免 println! panic。
        // 调试构建有真实控制台，保留输出不重定向。
        if !cfg!(debug_assertions) {
            ms_platform_windows::redirect_std_to_null();
        }
        ms_platform_windows::set_process_dpi_aware();
    }

    let args = match cli::parse(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            eprintln!(
                "用法: mouseshare [--no-tray] [--loopback] [--port <u16>] \
                 [--log-dir <dir>] [--record <file>|--replay <file>]"
            );
            return ExitCode::from(2);
        }
    };

    // 自检模式：不需要日志/平台装配，直接跑并按结果设退出码
    if args.selftest {
        return if selftest::run() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }

    let log_dir = args
        .log_dir
        .clone()
        .or_else(logging::default_log_dir)
        .unwrap_or_else(|| "./logs".into());
    let _log_guard = match logging::init(&log_dir) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("日志初始化失败（目录 {}）: {e}", log_dir.display());
            return ExitCode::FAILURE;
        }
    };

    tracing::info!(
        target: "mouseshare::app",
        version = env!("CARGO_PKG_VERSION"),
        proto_ver = ms_proto::PROTO_VER,
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        ?args,
        "MouseShare 启动"
    );

    // 录制/回放模式（T-110，开发工具；触发 H-1/H-5 实测）
    if let Some(path) = args.record.clone() {
        return match record::run_record(&path) {
            Ok(_) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("录制失败：{e}");
                ExitCode::FAILURE
            }
        };
    }
    if let Some(path) = args.replay.clone() {
        return match record::run_replay(&path) {
            Ok(_) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("回放失败：{e}");
                ExitCode::FAILURE
            }
        };
    }

    // 托盘 UI 模式：托盘 + eframe 设置窗口（图形化启停 + 全部设置）
    if args.tray {
        return if ui::run(args) {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }

    // 守护进程模式：监听或连接对端，运行互控主循环
    if args.serve || args.connect.is_some() {
        return if daemon::run(&args) {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }

    // 无显式 CLI 模式（含双击 .app / 裸运行）：默认启动托盘 UI
    if ui::run(args) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
