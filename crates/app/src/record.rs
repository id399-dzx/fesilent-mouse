//! 录制/回放工具（DEVELOPMENT.md §11.2，T-110）。
//!
//! - 录制：启动捕获（grab=false，本机仍可正常使用），把每个 `InputEvent` 连同
//!   毫秒时间戳写成 jsonl。默认录制窗口 20s，可用环境变量 `MS_RECORD_SECS` 覆盖。
//! - 回放：读取 jsonl 按时间间隔注入。鼠标相对位移在回放端累积为绝对坐标
//!   （从屏幕中心起步并 clamp），用于 M1 出口 record→replay 冒烟验证（精度归 H-5）。

use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use ms_platform::{InputEvent, Platform};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct Recorded {
    t_ms: u64,
    ev: InputEvent,
}

fn make_platform() -> impl Platform {
    #[cfg(target_os = "macos")]
    {
        ms_platform_macos::MacPlatform::new()
    }
    #[cfg(target_os = "windows")]
    {
        ms_platform_windows::WinPlatform::new()
    }
}

fn record_secs() -> u64 {
    std::env::var("MS_RECORD_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20)
}

/// 录制到 `path`。返回写入的事件数。
pub fn run_record(path: &Path) -> Result<usize, String> {
    let platform = make_platform();
    let mut capture = platform.input_capture().map_err(|e| e.to_string())?;

    let (tx, rx) = mpsc::channel::<Recorded>();
    let start = Instant::now();
    capture
        .start(
            Box::new(move |ev| {
                let _ = tx.send(Recorded {
                    t_ms: start.elapsed().as_millis() as u64,
                    ev,
                });
            }),
            false,
        )
        .map_err(|e| e.to_string())?;

    let secs = record_secs();
    println!("● 录制中：请在 {secs} 秒内操作键盘鼠标……（本机仍正常响应）");
    let deadline = Instant::now() + Duration::from_secs(secs);

    let file = File::create(path).map_err(|e| e.to_string())?;
    let mut w = BufWriter::new(file);
    let mut count = 0usize;
    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        match rx.recv_timeout(deadline - now) {
            Ok(rec) => {
                let line = serde_json::to_string(&rec).map_err(|e| e.to_string())?;
                writeln!(w, "{line}").map_err(|e| e.to_string())?;
                count += 1;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => break,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let _ = capture.stop();
    w.flush().map_err(|e| e.to_string())?;
    println!("✓ 录制完成：{count} 个事件 → {}", path.display());
    Ok(count)
}

/// 回放 `path`。返回注入的事件数。
pub fn run_replay(path: &Path) -> Result<usize, String> {
    let platform = make_platform();
    let mut inject = platform.input_inject().map_err(|e| e.to_string())?;

    let file = File::open(path).map_err(|e| e.to_string())?;
    let mut events: Vec<Recorded> = Vec::new();
    for line in BufReader::new(file).lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        events.push(serde_json::from_str(&line).map_err(|e| e.to_string())?);
    }
    events.sort_by_key(|r| r.t_ms);
    println!("● 回放 {} 个事件……", events.len());

    // 鼠标相对位移累积为绝对坐标（从 500,500 起步，简单 clamp）
    let (mut px, mut py) = (500.0_f64, 500.0_f64);
    let base = events.first().map(|r| r.t_ms).unwrap_or(0);
    let start = Instant::now();

    for rec in &events {
        let target = Duration::from_millis(rec.t_ms - base);
        let elapsed = start.elapsed();
        if target > elapsed {
            std::thread::sleep(target - elapsed);
        }
        let r = match rec.ev {
            InputEvent::Key { key, down } => inject.key(key, down),
            InputEvent::Button { btn, down } => inject.mouse_button(btn, down),
            InputEvent::Scroll { dx, dy, precise } => inject.scroll(dx, dy, precise),
            InputEvent::MouseMove { dx, dy, .. } => {
                px = (px + dx).clamp(0.0, 4000.0);
                py = (py + dy).clamp(0.0, 4000.0);
                inject.mouse_move_to(px, py)
            }
        };
        if let Err(e) = r {
            eprintln!("注入失败（已跳过）：{e}");
        }
    }
    println!("✓ 回放完成");
    Ok(events.len())
}
