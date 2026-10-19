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

mod views;

/// 方位九宫格的单元尺寸（T-606）。
const SIDE_CELL: [f32; 2] = [64.0, 36.0];

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
            .with_inner_size(winit::dpi::LogicalSize::new(920.0, 740.0))
            .with_min_inner_size(winit::dpi::LogicalSize::new(680.0, 560.0))
            .with_title("Fesilent Mouse")
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
        let Ok(mut cfg) = self.config.lock().map(|c| c.clone()) else {
            return;
        };
        match config::normalize_peer_address(cfg.connect.as_deref().unwrap_or_default()) {
            Ok(address) => cfg.connect = address,
            Err(error) => {
                self.status.set_error(error);
                return;
            }
        }
        self.status.clear_error();
        let args = cfg.to_args(self.log_dir.clone());
        let running = Arc::new(AtomicBool::new(true));
        self.running = Arc::clone(&running);
        let status = Arc::clone(&self.status);
        // 把共享配置交给 daemon：设置界面改动后引擎设置即时生效（无需重启互控）。
        let config = Arc::clone(&self.config);
        let previous = self.thread.take();
        self.thread = Some(std::thread::spawn(move || {
            // 重新应用地址时等待旧监听退出，避免新旧线程同时绑定同一端口。
            // join 发生在工作线程上，原生窗口仍可继续交互。
            if let Some(previous) = previous {
                let _ = previous.join();
            }
            if !running.load(Ordering::Acquire) {
                return;
            }
            tracing::info!(target: "mouseshare::ui", "互控已启用（后台线程）");
            daemon::run_until(&args, config, Arc::clone(&running), status);
            running.store(false, Ordering::Release);
            tracing::info!(target: "mouseshare::ui", "互控已停止");
        }));
    }

    fn stop(&mut self) {
        self.running.store(false, Ordering::Release);
        self.status.set_connected(false);
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
    let quit = MenuItem::with_id("quit", "退出 Fesilent Mouse", true, None);
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
    section: views::Section,
    manual_connection: bool,
    connection_notice: Option<String>,
    form_error: Option<String>,
    local_ips: Vec<String>,
}

impl App {
    fn new(args: Args, first_run: bool, proxy: EventLoopProxy<UserEvent>) -> Self {
        let mut cfg = config::load();
        if first_run {
            cfg = Config::from_args(&args);
            let _ = config::save(&cfg);
        }
        let peer_ip = cfg.connect.clone().unwrap_or_default();
        let manual_connection = !peer_ip.is_empty();
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
            section: views::Section::Connection,
            manual_connection,
            connection_notice: None,
            form_error: None,
            local_ips: views::local_ipv4_addresses(),
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
                    self.handle.is_running()
                };
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
                gl.clear_color(0.953, 0.961, 0.973, 1.0);
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
        views::install_theme(&egui_glow.egui_ctx);

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
            .with_tooltip("Fesilent Mouse");
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

        let show_window =
            self.first_run || std::env::var("FESILENT_SHOW_WINDOW").is_ok_and(|value| value == "1");
        if show_window {
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
                self.set_toggle_text(self.handle.is_running());
            }
        }

        // 首启：主动渲染一帧，确保窗口立即有内容（不等系统重绘事件）
        if show_window {
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
