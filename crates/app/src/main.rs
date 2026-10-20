//! Fesilent Mouse 主程序：组装各层 + 托盘（DEVELOPMENT.md §2.2）。
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
mod devices;
mod diag;
mod logging;
mod record;
mod selftest;
mod session_safety;
mod single_instance;
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
                "用法: fesilent-mouse [--no-tray] [--loopback] [--port <u16>] \
                 [--log-dir <dir>] [--record <file>|--replay <file>]"
            );
            return ExitCode::from(2);
        }
    };

    // 隔离预览先于所有副作用入口：不初始化文件日志，也不能意外进入自检、录制或 daemon。
    if ui::ui_preview_enabled() {
        return if ui::run(args, None, None) {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }

    // 自检模式：不需要日志/平台装配，直接跑并按结果设退出码
    if args.selftest {
        return if selftest::run() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }

    // GUI 单实例必须先于日志、配置和输入后台。守卫保留到整个 GUI 调用返回，
    // 重复启动只通知原窗口，不产生第二份托盘、日志写入器或 QUIC/输入会话。
    let mut instance_guard = if single_instance::protects_gui(&args) {
        match single_instance::acquire_or_activate() {
            Ok(single_instance::Launch::Primary(guard)) => Some(guard),
            Ok(single_instance::Launch::Activated) => return ExitCode::SUCCESS,
            Err(error) => {
                #[cfg(target_os = "windows")]
                ms_platform_windows::window::show_startup_error(&format!(
                    "无法打开 Fesilent Mouse：{error}"
                ));
                eprintln!("无法启动工作空间：{error}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        None
    };
    let instance_accepting = instance_guard
        .as_ref()
        .map(single_instance::InstanceGuard::activation_accepting);
    let activation_receiver = instance_guard
        .as_mut()
        .and_then(single_instance::InstanceGuard::take_activation_receiver);

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
        "Fesilent Mouse 启动"
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

    // headless/回环必须真正不进入GUI，不能仅绕过实例锁后再创建额外窗口。
    if !single_instance::protects_gui(&args) {
        return if daemon::run(&args) {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }

    // 普通双击和显式 --tray 共用同一GUI分支，保证实例判定与实际执行一致。
    if ui::run(args, activation_receiver, instance_accepting) {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
