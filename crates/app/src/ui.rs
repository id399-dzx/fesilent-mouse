//! 设置窗口（egui，glow 后端）+ 系统托盘，跑在 **winit 原生事件循环**上（不用 eframe）。
//!
//! 为什么不用 eframe：eframe 把"是否刷新"绑定在窗口可见性上，窗口隐藏后刷新停摆 →
//! 投递给刷新循环的命令（如"显示窗口"）没人执行 → 托盘整个失效。自管 winit 后：
//! 托盘点击 → `MenuEvent` 回调 → `EventLoopProxy::send_event` 唤醒事件循环 → `user_event`
//! 里**直接** `window.set_visible(true)`，同步生效、与刷新无关。退出直接结束进程。
//!
//! GL 上下文创建（glutin）必须 unsafe，集中在 `GlutinWindowContext`，每处有 SAFETY。
//! 启动注入系统 CJK 字体（mac PingFang / Windows 雅黑），否则中文显示为豆腐块。

use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use glutin::context::NotCurrentGlContext as _;
use glutin::display::{GetGlDisplay as _, GlDisplay as _};
use glutin::prelude::GlSurface as _;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use winit::application::ApplicationHandler;
use winit::event::{StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::raw_window_handle::HasWindowHandle as _;
use winit::window::{Window, WindowId};

use crate::cli::Args;
use crate::cliphist;
use crate::config::{self, Config};
use crate::daemon;
use crate::diag;
use crate::status::ConnStatus;
use crate::transfers::{self, Dir, Phase};

/// 方位九宫格的单元尺寸（T-606）。
const SIDE_CELL: [f32; 2] = [40.0, 24.0];

/// 九宫格空位（占位对齐用）。
fn blank_cell(ui: &mut egui::Ui) {
    ui.add_sized(SIDE_CELL, egui::Label::new(""));
}

/// 九宫格方位单元：选中态按钮，点击把配置键写回 `side`。
fn side_cell(ui: &mut egui::Ui, side: &mut String, dirty: &mut bool, key: &str, label: &str) {
    if ui
        .add_sized(
            SIDE_CELL,
            egui::Button::selectable(side.as_str() == key, label),
        )
        .clicked()
    {
        *side = key.to_string();
        *dirty = true;
    }
}

// ===================== GL 窗口（glutin，参照 egui_glow pure_glow 示例） =====================

struct GlutinWindowContext {
    window: Window,
    gl_context: glutin::context::PossiblyCurrentContext,
    gl_display: glutin::display::Display,
    gl_surface: glutin::surface::Surface<glutin::surface::WindowSurface>,
}

impl GlutinWindowContext {
    /// 创建隐藏的 GL 窗口（之后按需 set_visible）。
    ///
    /// # Safety
    /// 调用 glutin 的 GL 上下文/表面创建 FFI；入参均由本函数构造的合法句柄。
    // GUI 初始化失败即程序不可用，此处 unwrap/expect 直接 panic 可接受
    #[allow(clippy::unwrap_used, clippy::expect_used)]
    unsafe fn new(event_loop: &ActiveEventLoop) -> Self {
        let mut win_attrs = Window::default_attributes()
            .with_resizable(true)
            .with_inner_size(winit::dpi::LogicalSize::new(460.0, 560.0))
            .with_title("MouseShare 设置")
            .with_visible(false);
        // 自测用：MS_UI_POS="x,y" 指定窗口位置（避开遮挡好截图）；发布无此 env 不生效
        if let Some(p) = std::env::var_os("MS_UI_POS").and_then(|s| s.into_string().ok()) {
            let mut it = p.split(',').filter_map(|x| x.trim().parse::<f64>().ok());
            if let (Some(x), Some(y)) = (it.next(), it.next()) {
                win_attrs = win_attrs.with_position(winit::dpi::LogicalPosition::new(x, y));
            }
        }

        let template = glutin::config::ConfigTemplateBuilder::new()
            .prefer_hardware_accelerated(None)
            .with_depth_size(0)
            .with_stencil_size(0)
            .with_transparency(false);

        let (mut window, gl_config) = glutin_winit::DisplayBuilder::new()
            .with_preference(glutin_winit::ApiPreference::FallbackEgl)
            .with_window_attributes(Some(win_attrs.clone()))
            .build(event_loop, template, |mut it| {
                it.next().expect("无可用 GL 配置")
            })
            .expect("创建 GL 配置失败");

        let gl_display = gl_config.display();
        let raw = window
            .as_ref()
            .map(|w| w.window_handle().expect("取窗口句柄失败").as_raw());
        let ctx_attrs = glutin::context::ContextAttributesBuilder::new().build(raw);
        let fallback_attrs = glutin::context::ContextAttributesBuilder::new()
            .with_context_api(glutin::context::ContextApi::Gles(None))
            .build(raw);
        // SAFETY: gl_config/raw 句柄由上方合法构造；失败回退 GLES
        let not_current = unsafe {
            gl_display
                .create_context(&gl_config, &ctx_attrs)
                .or_else(|_| gl_display.create_context(&gl_config, &fallback_attrs))
                .expect("创建 GL 上下文失败")
        };

        let window = window.take().unwrap_or_else(|| {
            glutin_winit::finalize_window(event_loop, win_attrs.clone(), &gl_config)
                .expect("finalize_window 失败")
        });

        let (w, h): (u32, u32) = window.inner_size().into();
        let surf_attrs =
            glutin::surface::SurfaceAttributesBuilder::<glutin::surface::WindowSurface>::new()
                .build(
                    window.window_handle().expect("取窗口句柄失败").as_raw(),
                    NonZeroU32::new(w).unwrap_or(NonZeroU32::MIN),
                    NonZeroU32::new(h).unwrap_or(NonZeroU32::MIN),
                );
        // SAFETY: surf_attrs 引用 window 的合法句柄，window 比 surface 活得久
        let gl_surface = unsafe {
            gl_display
                .create_window_surface(&gl_config, &surf_attrs)
                .expect("创建 GL 表面失败")
        };
        let gl_context = not_current
            .make_current(&gl_surface)
            .expect("make_current 失败");
        let _ = gl_surface.set_swap_interval(
            &gl_context,
            glutin::surface::SwapInterval::Wait(NonZeroU32::MIN),
        );

        Self {
            window,
            gl_context,
            gl_display,
            gl_surface,
        }
    }

    fn window(&self) -> &Window {
        &self.window
    }

    fn resize(&self, size: winit::dpi::PhysicalSize<u32>) {
        self.gl_surface.resize(
            &self.gl_context,
            NonZeroU32::new(size.width).unwrap_or(NonZeroU32::MIN),
            NonZeroU32::new(size.height).unwrap_or(NonZeroU32::MIN),
        );
    }

    fn swap_buffers(&self) {
        let _ = self.gl_surface.swap_buffers(&self.gl_context);
    }

    fn get_proc_address(&self, addr: &std::ffi::CStr) -> *const std::ffi::c_void {
        self.gl_display.get_proc_address(addr)
    }
}

// ===================== daemon 句柄 =====================

struct DaemonHandle {
    config: Arc<Mutex<Config>>,
    log_dir: Option<PathBuf>,
    running: Arc<AtomicBool>,
    status: Arc<ConnStatus>,
    thread: Option<JoinHandle<()>>,
}

impl DaemonHandle {
    fn new(config: Arc<Mutex<Config>>, log_dir: Option<PathBuf>) -> Self {
        Self {
            config,
            log_dir,
            running: Arc::new(AtomicBool::new(false)),
            status: Arc::new(ConnStatus::new()),
            thread: None,
        }
    }

    fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    fn status_summary(&self) -> String {
        if self.is_running() {
            self.status.summary()
        } else {
            "○ 未启用".to_string()
        }
    }

    fn start(&mut self) {
        if self.is_running() {
            return;
        }
        let Ok(cfg) = self.config.lock().map(|c| c.clone()) else {
            return;
        };
        let args = cfg.to_args(self.log_dir.clone());
        let running = Arc::new(AtomicBool::new(true));
        self.running = Arc::clone(&running);
        let status = Arc::clone(&self.status);
        // 把共享配置交给 daemon：设置界面改动后引擎设置即时生效（无需重启互控）。
        let config = Arc::clone(&self.config);
        self.thread = Some(std::thread::spawn(move || {
            tracing::info!(target: "mouseshare::ui", "互控已启用（后台线程）");
            daemon::run_until(&args, config, Arc::clone(&running), status);
            running.store(false, Ordering::Release);
            tracing::info!(target: "mouseshare::ui", "互控已停止");
        }));
    }

    fn stop(&mut self) {
        self.running.store(false, Ordering::Release);
        if let Some(h) = self.thread.take() {
            std::thread::spawn(move || {
                let _ = h.join();
            });
        }
    }
}

// ===================== 辅助 =====================

/// 注入系统 CJK 字体（egui 内置字体无中文）。
fn install_cjk_font(ctx: &egui::Context) {
    let mut candidates: Vec<&str> = Vec::new();
    #[cfg(target_os = "macos")]
    {
        candidates.push("/System/Library/Fonts/PingFang.ttc");
        candidates.push("/System/Library/Fonts/STHeiti Medium.ttc");
        candidates.push("/System/Library/Fonts/Hiragino Sans GB.ttc");
    }
    #[cfg(target_os = "windows")]
    {
        candidates.push("C:\\Windows\\Fonts\\msyh.ttc");
        candidates.push("C:\\Windows\\Fonts\\simhei.ttf");
        candidates.push("C:\\Windows\\Fonts\\simsun.ttc");
    }
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            let mut fonts = egui::FontDefinitions::default();
            fonts.font_data.insert(
                "cjk".to_owned(),
                Arc::new(egui::FontData::from_owned(bytes)),
            );
            if let Some(f) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
                f.insert(0, "cjk".to_owned());
            }
            if let Some(f) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
                f.push("cjk".to_owned());
            }
            ctx.set_fonts(fonts);
            tracing::info!(target: "mouseshare::ui", font = path, "已加载中文字体");
            return;
        }
    }
    tracing::warn!(target: "mouseshare::ui", "未找到系统中文字体，界面中文可能为方块");
}

fn load_tray_icon() -> Option<Icon> {
    let bytes: &[u8] = include_bytes!("../icons/tray.png");
    let decoder = png::Decoder::new(bytes);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf[..info.buffer_size()].to_vec(),
        png::ColorType::Rgb => buf[..info.buffer_size()]
            .chunks(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        _ => return None,
    };
    Icon::from_rgba(rgba, info.width, info.height).ok()
}

/// 构建一份托盘菜单（toggle 文字随 running 变）。
///
/// 切换状态时整体重建并 `set_menu` 替换，而非 `MenuItem::set_text`——后者对已挂载到
/// 系统托盘的菜单项更新在部分平台/版本不可靠（真机回报：点暂停后文字不变）。
fn build_tray_menu(running: bool) -> Menu {
    let toggle_label = if running {
        "暂停互控"
    } else {
        "启用互控"
    };
    let toggle = MenuItem::with_id("toggle", toggle_label, true, None);
    let settings = MenuItem::with_id("settings", "设置…", true, None);
    let quit = MenuItem::with_id("quit", "退出 MouseShare", true, None);
    let menu = Menu::new();
    let _ = menu.append_items(&[&toggle, &settings, &PredefinedMenuItem::separator(), &quit]);
    menu
}

fn platform_autostart() -> Option<Box<dyn ms_platform::Autostart>> {
    use ms_platform::Platform as _;
    #[cfg(target_os = "macos")]
    {
        ms_platform_macos::MacPlatform::new().autostart().ok()
    }
    #[cfg(target_os = "windows")]
    {
        ms_platform_windows::WinPlatform::new().autostart().ok()
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        None
    }
}

fn autostart_enabled() -> bool {
    platform_autostart()
        .and_then(|mut a| a.is_enabled().ok())
        .unwrap_or(false)
}

// ===================== 事件 + App =====================

/// winit 自定义事件。
#[derive(Debug)]
enum UserEvent {
    /// 托盘菜单 / 本地自测触发的菜单动作 id。
    Menu(String),
    /// egui 请求在某延迟后重绘。
    Repaint(Duration),
    /// 请求显示设置窗口（大文件待确认时由传输中心触发，隐藏态也能弹出）。
    ShowWindow,
}

struct App {
    proxy: EventLoopProxy<UserEvent>,
    args: Args,
    first_run: bool,
    // GUI（resumed 时创建）
    gl_window: Option<GlutinWindowContext>,
    gl: Option<Arc<egui_glow::glow::Context>>,
    egui_glow: Option<egui_glow::EguiGlow>,
    tray: Option<TrayIcon>,
    repaint_after: Duration,
    // 业务
    handle: DaemonHandle,
    config: Arc<Mutex<Config>>,
    peer_ip: String,
    autostart_on: bool,
    toggle_label: &'static str,
    /// ⌘W/Ctrl+W 按下待处理：draw 期间窗口句柄已移出 self，隐藏动作延到 redraw 末尾执行（T-607）。
    hide_requested: bool,
    /// 最近一次诊断导出的结果提示（T-810），显示在按钮下方。
    diag_result: Option<String>,
    /// 最近一次剪贴板历史回填的结果提示（T-808）。
    clip_result: Option<String>,
}

impl App {
    fn new(args: Args, first_run: bool, proxy: EventLoopProxy<UserEvent>) -> Self {
        let mut cfg = config::load();
        if first_run {
            cfg = Config::from_args(&args);
            let _ = config::save(&cfg);
        }
        let peer_ip = cfg.connect.clone().unwrap_or_default();
        let config = Arc::new(Mutex::new(cfg));
        let handle = DaemonHandle::new(Arc::clone(&config), args.log_dir.clone());
        // 剪贴板历史容量按配置生效（T-808；0=关闭，改 config.json 后重启生效）
        cliphist::set_cap(
            config
                .lock()
                .map(|c| c.clip_history)
                .unwrap_or(cliphist::DEFAULT_CAP),
        );
        Self {
            proxy,
            args,
            first_run,
            gl_window: None,
            gl: None,
            egui_glow: None,
            tray: None,
            repaint_after: Duration::MAX,
            handle,
            config,
            peer_ip,
            autostart_on: autostart_enabled(),
            toggle_label: "启用互控",
            hide_requested: false,
            diag_result: None,
            clip_result: None,
        }
    }

    /// 显示设置窗口。mac 上置顶但**不抢键盘焦点**（orderFrontRegardless，T-611）：
    /// winit `set_visible(true)` 在 mac 是 makeKeyAndOrderFront，窗口一显示即成
    /// key window，对端控制本机时注入按键全被面板捕获、无法向其他应用输入；
    /// 想在面板内输入时点击面板即可正常获焦。其余平台保持 set_visible+focus。
    fn show_window(&self) {
        let Some(w) = &self.gl_window else { return };
        // T-613 取证：窗口显隐落日志，便于与「键盘链路计数」按时间对齐
        tracing::info!(target: "mouseshare::ui", "设置窗口显示");
        #[cfg(target_os = "macos")]
        {
            use winit::raw_window_handle::RawWindowHandle;
            if let Ok(h) = w.window().window_handle()
                && let RawWindowHandle::AppKit(ah) = h.as_raw()
            {
                ms_platform_macos::window::order_front_without_focus(ah.ns_view.as_ptr());
                return;
            }
            w.window().set_visible(true);
        }
        #[cfg(not(target_os = "macos"))]
        {
            w.window().set_visible(true);
            w.window().focus_window();
        }
    }

    /// 处理一次菜单动作（托盘或自测触发，均在主线程）。
    fn on_menu(&mut self, id: &str) {
        match id {
            "toggle" => {
                let running = if self.handle.is_running() {
                    self.handle.stop();
                    false
                } else {
                    self.handle.start();
                    true
                };
                if running {
                    self.remember_auto_start();
                }
                self.set_toggle_text(running);
            }
            "settings" => {
                self.show_window();
                // 主动渲染一帧：不等系统 RedrawRequested，规避"被遮挡窗口不重绘"导致的空白
                self.redraw();
            }
            "quit" => {
                self.handle.stop();
                std::process::exit(0);
            }
            _ => {}
        }
    }

    fn set_toggle_text(&mut self, running: bool) {
        let label = if running {
            "暂停互控"
        } else {
            "启用互控"
        };
        if self.toggle_label == label {
            return;
        }
        self.toggle_label = label;
        // 整体重建菜单替换（见 build_tray_menu 注释：set_text 真机不可靠）
        if let Some(tray) = &self.tray {
            tray.set_menu(Some(Box::new(build_tray_menu(running))));
        }
    }

    /// 用户启用互控后记住意图：之后每次打开软件自动开始互控（任务2）。
    fn remember_auto_start(&self) {
        if let Ok(mut cfg) = self.config.lock()
            && !cfg.auto_start
        {
            cfg.auto_start = true;
            let _ = config::save(&cfg);
        }
    }

    /// 渲染一帧设置界面。
    fn redraw(&mut self) {
        let (Some(mut egui_glow), Some(gl_window)) = (self.egui_glow.take(), self.gl_window.take())
        else {
            return;
        };
        egui_glow.run(gl_window.window(), |ctx| self.draw(ctx));
        if let Some(gl) = &self.gl {
            use egui_glow::glow::HasContext as _;
            // SAFETY: gl 由 resumed 初始化，当前上下文有效
            unsafe {
                gl.clear_color(0.12, 0.12, 0.12, 1.0);
                gl.clear(egui_glow::glow::COLOR_BUFFER_BIT);
            }
        }
        egui_glow.paint(gl_window.window());
        gl_window.swap_buffers();
        if self.hide_requested {
            self.hide_requested = false;
            tracing::info!(target: "mouseshare::ui", "设置窗口隐藏（快捷键）");
            gl_window.window().set_visible(false);
        }
        self.egui_glow = Some(egui_glow);
        self.gl_window = Some(gl_window);
    }

    fn draw(&mut self, ctx: &egui::Context) {
        // 托盘文字随状态同步（隐藏时 on_menu 已即时设过，这里覆盖窗口可见时的情况）
        let running = self.handle.is_running();
        self.set_toggle_text(running);

        // ⌘W（mac）/ Ctrl+W（Windows）关闭窗口 = 隐藏到托盘，不退出程序（T-607）。
        // egui 的 Modifiers::COMMAND 在 mac 映射 ⌘、其余平台映射 Ctrl，一处判断覆盖两端。
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::W)) {
            self.hide_requested = true;
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("MouseShare");

            // 首次使用权限引导（FR-803，H-1）：未授权辅助功能时互控无法工作，显著提示+一键授权。
            // 每帧查询（UI 200ms 轮询），授权后横幅自动消失。
            if !daemon::input_permission_granted() {
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(90, 30, 30))
                    .inner_margin(8)
                    .corner_radius(6)
                    .show(ui, |ui| {
                        ui.colored_label(
                            egui::Color32::from_rgb(255, 210, 120),
                            "⚠ 尚未授权「辅助功能」——互控（键鼠穿越）无法工作",
                        );
                        ui.small(
                            "需在 系统设置 → 隐私与安全性 → 辅助功能 勾选 MouseShare。\n\
                             点下面按钮会把本应用加入列表并打开该页面，勾选后即可使用（无需重启）。",
                        );
                        if ui.button("打开系统设置并授权").clicked() {
                            daemon::open_input_permission_settings();
                        }
                    });
                ui.add_space(6.0);
            }

            // 启停按钮 + 同行醒目连接状态（T-609）：绿=已连接、琥珀=运行中未连接、灰=未启用
            ui.horizontal(|ui| {
                if ui
                    .button(if running {
                        "⏸ 暂停互控"
                    } else {
                        "▶ 启用互控"
                    })
                    .clicked()
                {
                    if running {
                        self.handle.stop();
                    } else {
                        self.handle.start();
                        self.remember_auto_start();
                    }
                }
                let status = self.handle.status_summary();
                let color = if status.starts_with('●') {
                    egui::Color32::from_rgb(76, 195, 138)
                } else if running {
                    egui::Color32::from_rgb(239, 159, 39)
                } else {
                    ui.visuals().weak_text_color()
                };
                ui.colored_label(color, egui::RichText::new(status).strong());
            });
            ui.separator();

            // 文件传输（FR-503/504）：进度条、取消、大文件确认。
            let xfers = transfers::center().snapshot();
            if !xfers.is_empty() {
                ui.label("文件传输");
                for t in &xfers {
                    let arrow = match t.dir {
                        Dir::Send => "↑发送",
                        Dir::Recv => "↓接收",
                    };
                    match t.phase {
                        Phase::Confirm => {
                            ui.colored_label(
                                egui::Color32::from_rgb(255, 210, 120),
                                format!("{arrow} {}（{}）大文件，确认发送？", t.name, human_size(t.total)),
                            );
                            ui.horizontal(|ui| {
                                if ui.button("确认发送").clicked() {
                                    transfers::center().decide(t.id, true);
                                }
                                if ui.button("取消").clicked() {
                                    transfers::center().decide(t.id, false);
                                }
                            });
                        }
                        Phase::Active => {
                            ui.horizontal(|ui| {
                                ui.label(format!("{arrow} {}", t.name));
                                if ui.button("取消").clicked() {
                                    transfers::center().cancel(t.id);
                                }
                            });
                            let frac = if t.total > 0 {
                                (t.transferred as f32 / t.total as f32).clamp(0.0, 1.0)
                            } else {
                                0.0
                            };
                            // 进度 + 平滑速率 + 剩余时间（T-809）
                            let mut txt =
                                format!("{} / {}", human_size(t.transferred), human_size(t.total));
                            if t.rate_bps > 0.0 {
                                txt.push_str(&format!(" · {}/s", human_size(t.rate_bps as u64)));
                                if let Some(eta) = t.eta_secs {
                                    txt.push_str(&format!(" · 剩余{}", human_secs(eta)));
                                }
                            }
                            ui.add(egui::ProgressBar::new(frac).text(txt));
                        }
                        Phase::Done => {
                            ui.label(format!("✓ {arrow} {} 完成", t.name));
                        }
                        Phase::Failed => {
                            ui.colored_label(
                                egui::Color32::LIGHT_RED,
                                format!("✗ {arrow} {} 失败", t.name),
                            );
                        }
                        Phase::Cancelled => {
                            ui.weak(format!("⊘ {arrow} {} 已取消", t.name));
                        }
                    }
                }
                ui.separator();
            }

            // 最近传输历史（T-809）：可折叠，默认收起，新的在前。
            let hist = transfers::center().history();
            if !hist.is_empty() {
                egui::CollapsingHeader::new(format!("最近传输（{}）", hist.len()))
                    .default_open(false)
                    .show(ui, |ui| {
                        for h in &hist {
                            let arrow = match h.dir {
                                Dir::Send => "↑",
                                Dir::Recv => "↓",
                            };
                            let mark = match h.outcome {
                                Phase::Done => "✓",
                                Phase::Failed => "✗",
                                _ => "⊘",
                            };
                            ui.small(format!(
                                "{mark} {arrow} {}　{} · {}",
                                h.name,
                                human_size(h.bytes),
                                human_secs(h.secs)
                            ));
                        }
                    });
                ui.separator();
            }

            // 剪贴板历史（T-808）：可折叠、默认收起；「回填」写回系统剪贴板。
            let clips = cliphist::snapshot();
            if !clips.is_empty() {
                egui::CollapsingHeader::new(format!("剪贴板历史（{}）", clips.len()))
                    .default_open(false)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.small("⇠ 来自对端　⇢ 本机发出　仅存内存，退出即清");
                            if ui.small_button("清空").clicked() {
                                cliphist::clear();
                                self.clip_result = None;
                            }
                        });
                        for e in &clips {
                            ui.horizontal(|ui| {
                                match &e.refill {
                                    Some(content) => {
                                        if ui.small_button("回填").clicked() {
                                            self.clip_result =
                                                Some(match daemon::write_clipboard(content) {
                                                    Ok(()) => "✓ 已回填到剪贴板".to_string(),
                                                    Err(err) => format!("✗ 回填失败：{err}"),
                                                });
                                        }
                                    }
                                    None => {
                                        ui.weak("过大");
                                    }
                                }
                                let src = if e.from_peer { "⇠" } else { "⇢" };
                                ui.small(format!("{src} {}", e.preview));
                            });
                        }
                        if let Some(r) = &self.clip_result {
                            ui.small(r.clone());
                        }
                    });
                ui.separator();
            }

            // 设置分组卡片（T-606）：每端只显示在本端生效的选项。
            // 各设置的消费端见 engine.rs——方位/滚轮反转在控制端（发起穿越方，
            // 用户布局里=Windows）生效，修饰键交换在被控端（注入方=mac）生效。
            let card = egui::Frame::new()
                .fill(egui::Color32::from_rgb(40, 40, 40))
                .inner_margin(10)
                .corner_radius(8);

            if let Ok(mut cfg) = self.config.lock() {
                let mut dirty = false;

                card.show(ui, |ui| {
                    ui.strong("连接");
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label("对端 IP");
                        if ui.text_edit_singleline(&mut self.peer_ip).changed() {
                            let v = self.peer_ip.trim();
                            cfg.connect = if v.is_empty() {
                                None
                            } else {
                                Some(v.to_string())
                            };
                            dirty = true;
                        }
                    });
                    ui.small("留空 = 同网段自动发现（mDNS）；填 IP:端口 手动连接");
                    if ui
                        .checkbox(
                            &mut cfg.auto_start,
                            "打开软件时自动连接（启用过一次后默认开启）",
                        )
                        .changed()
                    {
                        dirty = true;
                    }
                    if ui.checkbox(&mut self.autostart_on, "开机自启").changed() {
                        if let Some(mut a) = platform_autostart() {
                            if a.set_enabled(self.autostart_on).is_err() {
                                self.autostart_on = !self.autostart_on;
                            }
                        } else {
                            self.autostart_on = false;
                        }
                    }
                });
                ui.add_space(6.0);

                if cfg!(target_os = "macos") {
                    card.show(ui, |ui| {
                        ui.strong("被 Windows 控制");
                        ui.small("对端键鼠操作本机时生效");
                        ui.add_space(4.0);
                        if ui
                            .checkbox(&mut cfg.swap_modifiers, "交换 Ctrl ↔ Cmd")
                            .changed()
                        {
                            dirty = true;
                        }
                        ui.small("Windows 键盘的 Ctrl+C，在本机注入为 ⌘C");
                    });
                } else {
                    card.show(ui, |ui| {
                        ui.strong("控制 Mac");
                        ui.small("本机光标穿越到 Mac 时生效");
                        ui.add_space(4.0);
                        ui.label("Mac 在本机的哪一边");
                        ui.add_space(2.0);
                        ui.vertical_centered(|ui| {
                            egui::Grid::new("side_grid").spacing([4.0, 4.0]).show(
                                ui,
                                |ui| {
                                    blank_cell(ui);
                                    side_cell(ui, &mut cfg.side, &mut dirty, "top", "上");
                                    blank_cell(ui);
                                    ui.end_row();
                                    side_cell(ui, &mut cfg.side, &mut dirty, "left", "左");
                                    ui.add_sized(
                                        SIDE_CELL,
                                        egui::Label::new(egui::RichText::new("本机").weak()),
                                    );
                                    side_cell(ui, &mut cfg.side, &mut dirty, "right", "右");
                                    ui.end_row();
                                    blank_cell(ui);
                                    side_cell(ui, &mut cfg.side, &mut dirty, "bottom", "下");
                                    blank_cell(ui);
                                    ui.end_row();
                                },
                            );
                            let dir = match cfg.side.as_str() {
                                "left" => "左",
                                "top" => "上",
                                "bottom" => "下",
                                _ => "右",
                            };
                            ui.small(format!("光标从{dir}边缘推出即控制 Mac"));
                        });
                        ui.add_space(4.0);
                        cfg.scroll_speed = config::normalize_scroll_speed(cfg.scroll_speed);
                        ui.label("纵向滚轮速度");
                        if ui
                            .add(
                                egui::Slider::new(&mut cfg.scroll_speed, 1.0..=3.0)
                                    .step_by(0.1)
                                    .fixed_decimals(1)
                                    .suffix("×"),
                            )
                            .changed()
                        {
                            cfg.scroll_speed = config::normalize_scroll_speed(cfg.scroll_speed);
                            dirty = true;
                        }
                        ui.small("控制 Mac 时生效，1.0× 为原速，仅调整上下滚动");
                        ui.add_space(4.0);
                        if ui
                            .checkbox(&mut cfg.invert_scroll, "反转滚轮方向")
                            .changed()
                        {
                            dirty = true;
                        }
                        ui.small("操作 Mac 时，滚轮上下取反");
                    });
                }
                if dirty {
                    let _ = config::save(&cfg);
                }
            }

            // 诊断导出（T-810）：一键打包日志+配置+版本到桌面，回报问题时直接拖走。
            ui.separator();
            ui.horizontal(|ui| {
                if ui.button("导出诊断包").clicked() {
                    self.diag_result = Some(match diag::export(self.args.log_dir.as_deref()) {
                        Ok(p) => format!("✓ 已导出：{}", p.display()),
                        Err(e) => format!("✗ 导出失败：{e}"),
                    });
                }
                ui.small("日志(近7天)+配置+版本 → 桌面");
            });
            if let Some(r) = &self.diag_result {
                ui.small(r.clone());
            }

            ui.separator();
            if cfg!(target_os = "macos") {
                ui.small("方位、滚轮速度与方向请在 Windows 端设置");
            } else {
                ui.small("Ctrl/Cmd 交换请在 Mac 端设置");
            }
            ui.small("设置即时生效，无需暂停互控。关闭本窗口会缩到托盘继续运行。");
        });

        // 窗口可见时定期刷新（更新 RTT 状态行）
        ctx.request_repaint_after(Duration::from_millis(500));
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.egui_glow.is_some() {
            return;
        }
        // SAFETY: 在 resumed（事件循环活跃）中创建 GL 上下文
        let gl_window = unsafe { GlutinWindowContext::new(event_loop) };
        // SAFETY: 用 glutin 的 proc 加载器构造 glow 上下文
        let gl = unsafe {
            egui_glow::glow::Context::from_loader_function(|s| {
                std::ffi::CString::new(s)
                    .map(|c| gl_window.get_proc_address(&c))
                    .unwrap_or(std::ptr::null())
            })
        };
        let gl = Arc::new(gl);
        let egui_glow = egui_glow::EguiGlow::new(event_loop, gl.clone(), None, None, true);
        install_cjk_font(&egui_glow.egui_ctx);

        // egui 请求重绘 → 经 proxy 转成 UserEvent::Repaint
        let proxy = self.proxy.clone();
        egui_glow
            .egui_ctx
            .set_request_repaint_callback(move |info| {
                let _ = proxy.send_event(UserEvent::Repaint(info.delay));
            });

        // 托盘菜单（初始「启用互控」；切换时整体重建，见 set_toggle_text）
        let mut tb = TrayIconBuilder::new()
            .with_menu(Box::new(build_tray_menu(false)))
            .with_tooltip("MouseShare");
        if let Some(ic) = load_tray_icon() {
            tb = tb.with_icon(ic);
        }
        let tray = match tb.build() {
            Ok(t) => {
                // mac 菜单栏：模板图标，系统按深/浅色模式自动反色（只用 alpha 形状）
                #[cfg(target_os = "macos")]
                t.set_icon_as_template(true);
                Some(t)
            }
            Err(e) => {
                tracing::error!(target: "mouseshare::ui", error = %e, "托盘创建失败");
                None
            }
        };

        if self.first_run {
            gl_window.window().set_visible(true);
        }

        self.gl_window = Some(gl_window);
        self.gl = Some(gl);
        self.egui_glow = Some(egui_glow);
        self.tray = tray;
        // args 已用于首启 config，之后不再需要
        let _ = &self.args;

        // 任务2：非首启且用户曾启用过 → 打开软件即自动开始互控，免手动点「启用」
        if !self.first_run {
            let auto = self.config.lock().map(|c| c.auto_start).unwrap_or(false);
            if auto {
                tracing::info!(target: "mouseshare::ui", "auto_start 已开，自动启动互控");
                self.handle.start();
                self.set_toggle_text(true);
            }
        }

        // 首启：主动渲染一帧，确保窗口立即有内容（不等系统重绘事件）
        if self.first_run {
            self.redraw();
        }
    }

    fn window_event(&mut self, _event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            // 关窗 = 隐藏到托盘（不退出）
            WindowEvent::CloseRequested => {
                if let Some(w) = &self.gl_window {
                    tracing::info!(target: "mouseshare::ui", "设置窗口隐藏（关闭按钮）");
                    w.window().set_visible(false);
                }
            }
            WindowEvent::RedrawRequested => {
                self.redraw();
            }
            WindowEvent::Resized(size) => {
                if let Some(w) = &self.gl_window {
                    w.resize(size);
                }
            }
            other => {
                let resp = match (self.egui_glow.as_mut(), self.gl_window.as_ref()) {
                    (Some(eg), Some(w)) => Some(eg.on_window_event(w.window(), &other)),
                    _ => None,
                };
                if let (Some(resp), Some(w)) = (resp, &self.gl_window) {
                    if resp.repaint {
                        w.window().request_redraw();
                    }
                }
            }
        }
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Menu(id) => self.on_menu(&id),
            UserEvent::Repaint(delay) => {
                self.repaint_after = delay;
                if delay.is_zero()
                    && let Some(w) = &self.gl_window
                {
                    w.window().request_redraw();
                }
            }
            UserEvent::ShowWindow => {
                // 直接显示（不依赖刷新循环，隐藏态也能弹出大文件确认）；mac 不抢键盘焦点
                self.show_window();
                self.redraw();
            }
        }
    }

    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        if let StartCause::ResumeTimeReached { .. } = cause
            && let Some(w) = &self.gl_window
        {
            w.window().request_redraw();
        }
        // 控制流：有限延迟则定时唤醒重绘，否则等待（托盘事件会经 proxy 唤醒）
        let cf = if self.repaint_after.is_zero() {
            ControlFlow::Poll
        } else if let Some(t) = std::time::Instant::now().checked_add(self.repaint_after) {
            ControlFlow::WaitUntil(t)
        } else {
            ControlFlow::Wait
        };
        event_loop.set_control_flow(cf);
    }
}

/// 字节数 → 人类可读（B/KB/MB/GB）。
fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b < KB {
        format!("{bytes} B")
    } else if b < KB * KB {
        format!("{:.1} KB", b / KB)
    } else if b < KB * KB * KB {
        format!("{:.1} MB", b / (KB * KB))
    } else {
        format!("{:.2} GB", b / (KB * KB * KB))
    }
}

/// 秒 → 可读时长（T-809）："45秒"、"3分12秒"、"1时08分"。
fn human_secs(s: u64) -> String {
    if s < 60 {
        format!("{s}秒")
    } else if s < 3600 {
        format!("{}分{:02}秒", s / 60, s % 60)
    } else {
        format!("{}时{:02}分", s / 3600, (s % 3600) / 60)
    }
}

/// 本地自测控制通道（仅 debug）：读 `MS_UI_CONTROL` 指向的文件，把内容当菜单动作触发。
#[cfg(debug_assertions)]
fn spawn_control_listener(proxy: EventLoopProxy<UserEvent>) {
    let Some(path) = std::env::var_os("MS_UI_CONTROL") else {
        return;
    };
    let path = PathBuf::from(path);
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_millis(100));
            if let Ok(s) = std::fs::read_to_string(&path) {
                let cmd = s.trim().to_string();
                if !cmd.is_empty() {
                    let _ = std::fs::write(&path, "");
                    if proxy.send_event(UserEvent::Menu(cmd)).is_err() {
                        return;
                    }
                }
            }
        }
    });
}

/// 运行 UI（托盘 + 设置窗口）。返回是否正常结束。
pub fn run(args: Args) -> bool {
    let first_run = config::config_path().map(|p| !p.exists()).unwrap_or(true);
    let event_loop = match EventLoop::<UserEvent>::with_user_event().build() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("创建事件循环失败: {e}");
            return false;
        }
    };
    let proxy = event_loop.create_proxy();

    // 托盘菜单事件 → proxy（不在回调里碰 MenuItem，muda MenuItem 非 Send+Sync）
    let p_menu = proxy.clone();
    MenuEvent::set_event_handler(Some(move |ev: MenuEvent| {
        let _ = p_menu.send_event(UserEvent::Menu(ev.id.as_ref().to_string()));
    }));

    #[cfg(debug_assertions)]
    spawn_control_listener(proxy.clone());
    spawn_transfer_watcher(proxy.clone());

    let mut app = App::new(args, first_run, proxy);
    event_loop.run_app(&mut app).is_ok()
}

/// 后台轮询传输中心：有大文件待确认时请求弹出设置窗口（隐藏态也能弹），
/// 并在有进行中传输时驱动窗口重绘以刷新进度。
fn spawn_transfer_watcher(proxy: EventLoopProxy<UserEvent>) {
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_millis(200));
            let center = crate::transfers::center();
            if center.take_show_request() && proxy.send_event(UserEvent::ShowWindow).is_err() {
                return;
            }
            if center.has_active()
                && proxy
                    .send_event(UserEvent::Repaint(Duration::ZERO))
                    .is_err()
            {
                return;
            }
        }
    });
}
