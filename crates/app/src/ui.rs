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
use std::path::{Path, PathBuf};
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
use crate::status::{ConnStatus, ConnectionPhase};
use crate::transfers::{self, Dir, Phase};

mod views;

/// 独立 UI 预览不读取用户设置、不发起连接，也不申请输入权限。
fn preview_platform(value: Option<&str>) -> Option<bool> {
    match value {
        Some("windows") => Some(false),
        Some("mac") => Some(true),
        Some("1") => Some(cfg!(target_os = "macos")),
        _ => None,
    }
}

pub(crate) fn ui_preview_enabled() -> bool {
    preview_platform(std::env::var("FESILENT_UI_PREVIEW").ok().as_deref()).is_some()
}

/// 用系统文件管理器打开已存在的本地目录，不把路径交给 shell 解释。
fn open_path(path: &Path) -> Result<(), String> {
    if ui_preview_enabled() {
        return Err("预览模式：不会打开实际文件目录。".into());
    }
    if !path.exists() {
        return Err("接收目录尚未创建，收到第一个文件后即可打开。".into());
    }
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(target_os = "windows")]
    let mut command = std::process::Command::new("explorer.exe");
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut command = std::process::Command::new("xdg-open");
    command
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|_| "无法打开文件夹，请在系统文件管理器中查看下载目录。".into())
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
            .with_inner_size(winit::dpi::LogicalSize::new(1040.0, 760.0))
            .with_min_inner_size(winit::dpi::LogicalSize::new(680.0, 560.0))
            .with_title("Fesilent Mouse")
            .with_window_icon(load_window_icon())
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
    ui_preview: bool,
    operation_result: Arc<Mutex<Option<Result<String, String>>>>,
}

impl DaemonHandle {
    fn new(config: Arc<Mutex<Config>>, log_dir: Option<PathBuf>) -> Self {
        let status = Arc::new(ConnStatus::new());
        status.require_pairing_confirmation(true);
        Self {
            config,
            log_dir,
            running: Arc::new(AtomicBool::new(false)),
            status,
            thread: None,
            ui_preview: ui_preview_enabled(),
            operation_result: Arc::new(Mutex::new(None)),
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
        if self.ui_preview {
            self.running.store(true, Ordering::Release);
            self.status.set_phase(
                if preview_platform(std::env::var("FESILENT_UI_PREVIEW").ok().as_deref())
                    == Some(true)
                {
                    ConnectionPhase::Listening
                } else {
                    ConnectionPhase::Discovering
                },
            );
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
        if !cfg.devices.records.is_empty()
            && cfg.devices.active_target.is_none()
            && !self.status.adding_device()
        {
            self.status
                .set_error("请在「我的设备」中设置方位并选择连接目标，或点击添加新设备。");
            return;
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
        self.status.set_phase(ConnectionPhase::Idle);
    }

    /// 等旧会话退出后再解除配对，避免旧线程把已清理的信任重新写入。
    fn forget_devices(&mut self) {
        if self.ui_preview {
            if let Ok(mut result) = self.operation_result.lock() {
                *result = Some(Ok("预览模式：未更改实际设备配对。".into()));
            }
            return;
        }
        self.stop();
        let previous = self.thread.take();
        let result = Arc::clone(&self.operation_result);
        self.thread = Some(std::thread::spawn(move || {
            if let Some(previous) = previous {
                let _ = previous.join();
            }
            let outcome = daemon::forget_paired_devices().map(|()| {
                "本机已忘记设备。请在另一台电脑上也先忘记设备，再重新连接并核对配对码。".into()
            });
            if let Ok(mut shared) = result.lock() {
                *shared = Some(outcome);
            }
        }));
    }

    fn forget_device(&mut self, fingerprint: [u8; 32]) {
        self.stop();
        if self.ui_preview {
            return;
        }
        let previous = self.thread.take();
        let result = self.operation_result.clone();
        self.thread = Some(std::thread::spawn(move || {
            if let Some(previous) = previous {
                let _ = previous.join();
            }
            let outcome = daemon::forget_device(&fingerprint)
                .map(|()| "本机已撤销这台设备的信任；另一端需分别操作。".into());
            *result.lock().unwrap_or_else(|e| e.into_inner()) = Some(outcome);
        }));
    }

    fn take_operation_result(&self) -> Option<Result<String, String>> {
        self.operation_result.lock().ok()?.take()
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
                // 中文字体作为回退，避免旧系统黑体的拉丁字形覆盖原生界面字体。
                f.push("cjk".to_owned());
            }
            if let Some(f) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
                f.push("cjk".to_owned());
            }
            let system_font = if cfg!(target_os = "macos") {
                "/System/Library/Fonts/SFNS.ttf"
            } else {
                "C:\\Windows\\Fonts\\segoeui.ttf"
            };
            if let Ok(bytes) = std::fs::read(system_font) {
                fonts.font_data.insert(
                    "system-ui".to_owned(),
                    Arc::new(egui::FontData::from_owned(bytes)),
                );
                if let Some(f) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
                    f.insert(0, "system-ui".to_owned());
                }
            }
            ctx.set_fonts(fonts);
            tracing::info!(target: "mouseshare::ui", font = path, "已加载中文字体");
            return;
        }
    }
    tracing::warn!(target: "mouseshare::ui", "未找到系统中文字体，界面中文可能为方块");
}

/// 主窗口与托盘共用 RGBA 解码，展开调色板和透明色，兼容不同 PNG 导出格式。
fn load_icon_rgba(bytes: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let mut decoder = png::Decoder::new(bytes);
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf[..info.buffer_size()].to_vec(),
        png::ColorType::Rgb => buf[..info.buffer_size()]
            .chunks(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        png::ColorType::Grayscale => buf[..info.buffer_size()]
            .iter()
            .flat_map(|value| [*value, *value, *value, 255])
            .collect(),
        png::ColorType::GrayscaleAlpha => buf[..info.buffer_size()]
            .chunks(2)
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        png::ColorType::Indexed => return None,
    };
    Some((rgba, info.width, info.height))
}

/// Windows 标题栏与运行中的任务栏使用主图标；macOS Dock 由应用包内 ICNS 提供。
fn load_window_icon() -> Option<winit::window::Icon> {
    let (rgba, width, height) = load_icon_rgba(include_bytes!("../icons/icon.png"))?;
    winit::window::Icon::from_rgba(rgba, width, height).ok()
}

fn load_tray_icon() -> Option<Icon> {
    let (rgba, width, height) = load_icon_rgba(include_bytes!("../icons/tray.png"))?;
    Icon::from_rgba(rgba, width, height).ok()
}

/// 构建一份托盘菜单（toggle 文字随 running 变）。
///
/// 切换状态时整体重建并 `set_menu` 替换，而非 `MenuItem::set_text`——后者对已挂载到
/// 系统托盘的菜单项更新在部分平台/版本不可靠（真机回报：点暂停后文字不变）。
fn build_tray_menu(running: bool) -> Menu {
    let toggle_label = if running {
        "暂停连接"
    } else {
        "连接设备"
    };
    let toggle = MenuItem::with_id("toggle", toggle_label, true, None);
    let settings = MenuItem::with_id("settings", "打开工作空间…", true, None);
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
    /// 用户再次启动或点击 Dock/Finder，恢复并激活已有设置窗口。
    ActivateWindow,
    /// 后台网卡检测完成；主线程更新可复制的本机连接地址。
    LocalAddressesChanged(Vec<String>),
    /// 首次配对需核对短认证串，窗口隐藏时也必须能发现请求。
    PairingRequested,
    QuitReady,
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
    settings_notice: Option<(String, std::time::Instant)>,
    confirm_forget_devices: bool,
    ui_preview: bool,
    pending_forget_device: Option<String>,
    forget_all_pending: bool,
    quitting: bool,
    activation_pending: bool,
    instance_accepting: Option<Arc<AtomicBool>>,
}

impl App {
    fn new(args: Args, first_run: bool, proxy: EventLoopProxy<UserEvent>) -> Self {
        let ui_preview = ui_preview_enabled();
        let mut cfg = if ui_preview {
            Config::default()
        } else {
            config::load()
        };
        if first_run && !ui_preview {
            cfg = Config::from_args(&args);
            let _ = config::save(&cfg);
        }
        // 显式命令行端口覆盖本次GUI会话，地址卡片与真正监听端口使用同一配置。
        if let Some(port) = args.port.filter(|port| *port != 0) {
            cfg.port = Some(port);
        }
        if ui_preview && std::env::var("FESILENT_UI_PREVIEW_DEVICES").ok().as_deref() == Some("1") {
            for (fingerprint, name, side) in [
                ([1; 32], "书房电脑 · 预览", ms_proto::Edge::Right),
                ([2; 32], "笔记本 · 预览", ms_proto::Edge::Left),
            ] {
                let _ = cfg
                    .devices
                    .upsert_authenticated(fingerprint, name, None, side);
            }
            let _ = cfg
                .devices
                .select(&ms_core::topology::DeviceId::from_fingerprint([1; 32]).to_string());
        }
        let peer_ip = cfg.connect.clone().unwrap_or_default();
        let manual_connection = !peer_ip.is_empty();
        let config = Arc::new(Mutex::new(cfg));
        let handle = DaemonHandle::new(Arc::clone(&config), args.log_dir.clone());
        if ui_preview {
            match std::env::var("FESILENT_UI_PREVIEW_STATE").ok().as_deref() {
                Some("connected") => {
                    handle.running.store(true, Ordering::Release);
                    handle.status.set_connected(true);
                    handle.status.set_rtt_ms(3);
                }
                Some("reconnecting") => {
                    handle.running.store(true, Ordering::Release);
                    handle.status.set_phase(ConnectionPhase::Reconnecting);
                }
                _ => {}
            }
        }
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
            autostart_on: !ui_preview && autostart_enabled(),
            toggle_label: "启用互控",
            hide_requested: false,
            diag_result: None,
            clip_result: None,
            section: views::Section::Connection,
            manual_connection,
            connection_notice: None,
            form_error: None,
            local_ips: Vec::new(),
            settings_notice: None,
            confirm_forget_devices: false,
            pending_forget_device: None,
            forget_all_pending: false,
            quitting: false,
            activation_pending: false,
            instance_accepting: None,
            ui_preview,
        }
    }

    fn is_mac_ui(&self) -> bool {
        if self.ui_preview {
            preview_platform(std::env::var("FESILENT_UI_PREVIEW").ok().as_deref())
                .unwrap_or(cfg!(target_os = "macos"))
        } else {
            cfg!(target_os = "macos")
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

    /// 主动重开与自动通知分开处理：仅用户动作恢复最小化并请求键盘焦点。
    fn activate_window(&mut self) {
        if self.quitting {
            return;
        }
        let Some(window) = &self.gl_window else {
            self.activation_pending = true;
            return;
        };
        self.activation_pending = false;
        window.window().set_minimized(false);
        window.window().set_visible(true);
        window.window().focus_window();
        window.window().request_redraw();
        tracing::info!(target: "mouseshare::ui", "再次打开：已恢复现有设置窗口");
    }

    /// 处理一次菜单动作（托盘或自测触发，均在主线程）。
    fn on_menu(&mut self, id: &str) {
        match id {
            "toggle" => {
                if self.quitting || self.pending_forget_device.is_some() || self.forget_all_pending
                {
                    return;
                }
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
                self.activate_window();
                // 主动渲染一帧：不等系统 RedrawRequested，规避"被遮挡窗口不重绘"导致的空白
                self.redraw();
            }
            "quit" => {
                if self.quitting || self.pending_forget_device.is_some() || self.forget_all_pending
                {
                    return;
                }
                self.quitting = true;
                if let Some(accepting) = &self.instance_accepting {
                    accepting.store(false, Ordering::Release);
                }
                self.handle.stop();
                let previous = self.handle.thread.take();
                let proxy = self.proxy.clone();
                std::thread::spawn(move || {
                    if let Some(thread) = previous {
                        let _ = thread.join();
                    }
                    let _ = proxy.send_event(UserEvent::QuitReady);
                });
            }
            _ => {}
        }
    }

    fn set_toggle_text(&mut self, running: bool) {
        let label = if running {
            "暂停连接"
        } else {
            "连接设备"
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
        if let Some(outcome) = self.handle.take_operation_result() {
            match outcome {
                Ok(message) => {
                    if let Some(fingerprint) = self.pending_forget_device.take() {
                        let mut cfg = self
                            .config
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .clone();
                        let selected =
                            cfg.devices.active_target.as_deref() == Some(fingerprint.as_str());
                        cfg.devices.remove(&fingerprint);
                        if selected {
                            cfg.connect = None;
                            self.manual_connection = false;
                            self.peer_ip.clear();
                        }
                        self.store_config(&cfg);
                    }
                    if self.forget_all_pending {
                        let mut cfg = self
                            .config
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .clone();
                        cfg.devices = crate::devices::DeviceCatalog::default();
                        cfg.connect = None;
                        self.store_config(&cfg);
                        self.manual_connection = false;
                        self.peer_ip.clear();
                        self.forget_all_pending = false;
                    }
                    self.settings_notice = Some((message, std::time::Instant::now()));
                }
                Err(error) => {
                    self.pending_forget_device = None;
                    self.forget_all_pending = false;
                    self.form_error = Some(error);
                }
            }
        }
        let (Some(mut egui_glow), Some(gl_window)) = (self.egui_glow.take(), self.gl_window.take())
        else {
            return;
        };
        egui_glow.run(gl_window.window(), |ctx| self.draw(ctx));
        if let Some(gl) = &self.gl {
            use egui_glow::glow::HasContext as _;
            // SAFETY: gl 由 resumed 初始化，当前上下文有效
            unsafe {
                gl.clear_color(0.973, 0.969, 0.976, 1.0);
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

        let show_window = self.ui_preview
            || self.first_run
            || std::env::var("FESILENT_SHOW_WINDOW").is_ok_and(|value| value == "1");
        if show_window {
            gl_window.window().set_visible(true);
        }

        self.gl_window = Some(gl_window);
        self.gl = Some(gl);
        self.egui_glow = Some(egui_glow);
        self.tray = tray;
        if self.activation_pending {
            self.activate_window();
        }
        // args 已用于首启 config，之后不再需要
        let _ = &self.args;

        // 任务2：非首启且用户曾启用过 → 打开软件即自动开始互控，免手动点「启用」
        if !self.first_run && !self.ui_preview {
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
                if let (Some(resp), Some(w)) = (resp, &self.gl_window)
                    && resp.repaint
                {
                    w.window().request_redraw();
                }
            }
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::QuitReady => event_loop.exit(),
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
            UserEvent::ActivateWindow => {
                self.activate_window();
                self.redraw();
            }
            UserEvent::LocalAddressesChanged(addresses) => {
                if self.local_ips != addresses {
                    self.local_ips = addresses;
                    self.redraw();
                }
            }
            UserEvent::PairingRequested => {
                if self.handle.status.pending_pairing().is_some() {
                    self.section = views::Section::Connection;
                    self.show_window();
                    self.redraw();
                }
            }
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(accepting) = &self.instance_accepting {
            accepting.store(false, Ordering::Release);
        }
        self.handle.stop();
        if let Some(thread) = self.handle.thread.take() {
            let _ = thread.join();
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
pub fn run(
    args: Args,
    activation: Option<std::sync::mpsc::Receiver<()>>,
    instance_accepting: Option<Arc<AtomicBool>>,
) -> bool {
    let first_run =
        ui_preview_enabled() || config::config_path().map(|p| !p.exists()).unwrap_or(true);
    let event_loop = match EventLoop::<UserEvent>::with_user_event().build() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("创建事件循环失败: {e}");
            return false;
        }
    };
    let proxy = event_loop.create_proxy();

    #[cfg(target_os = "macos")]
    if !ui_preview_enabled() {
        let reopen_proxy = proxy.clone();
        if let Err(error) = ms_platform_macos::gui_reopen::install_reopen_handler(move || {
            let _ = reopen_proxy.send_event(UserEvent::ActivateWindow);
        }) {
            tracing::warn!(target: "mouseshare::ui", %error, "未能安装Dock重开处理");
        }
    }

    if let Some(activation) = activation {
        let activation_proxy = proxy.clone();
        std::thread::spawn(move || {
            while activation.recv().is_ok() {
                if activation_proxy
                    .send_event(UserEvent::ActivateWindow)
                    .is_err()
                {
                    break;
                }
            }
        });
    }

    // 网卡查询在后台进行；通过事件唤醒静止或隐藏的窗口，不在GUI线程运行系统命令。
    let local_addresses = views::spawn_local_ip_monitor();
    let address_proxy = proxy.clone();
    std::thread::spawn(move || {
        for addresses in local_addresses {
            if address_proxy
                .send_event(UserEvent::LocalAddressesChanged(addresses))
                .is_err()
            {
                break;
            }
        }
    });

    // 托盘菜单事件 → proxy（不在回调里碰 MenuItem，muda MenuItem 非 Send+Sync）
    let p_menu = proxy.clone();
    MenuEvent::set_event_handler(Some(move |ev: MenuEvent| {
        let _ = p_menu.send_event(UserEvent::Menu(ev.id.as_ref().to_string()));
    }));

    #[cfg(debug_assertions)]
    if !ui_preview_enabled() {
        spawn_control_listener(proxy.clone());
    }
    let mut app = App::new(args, first_run, proxy.clone());
    app.instance_accepting = instance_accepting;
    if !app.ui_preview {
        spawn_transfer_watcher(proxy, Arc::clone(&app.handle.status));
    }
    event_loop.run_app(&mut app).is_ok()
}

/// 后台轮询传输中心：有大文件待确认时请求弹出设置窗口（隐藏态也能弹），
/// 并在有进行中传输时驱动窗口重绘以刷新进度。
fn spawn_transfer_watcher(proxy: EventLoopProxy<UserEvent>, status: Arc<ConnStatus>) {
    std::thread::spawn(move || {
        let mut pairing_shown = None;
        loop {
            std::thread::sleep(Duration::from_millis(200));
            let center = crate::transfers::center();
            let pairing_pending = status.pending_pairing();
            if pairing_pending.is_some()
                && pairing_pending != pairing_shown
                && proxy.send_event(UserEvent::PairingRequested).is_err()
            {
                return;
            }
            pairing_shown = pairing_pending;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 预览必须显式选择且启停和解除配对不创建真实后台线程() {
        assert_eq!(preview_platform(Some("windows")), Some(false));
        assert_eq!(preview_platform(Some("mac")), Some(true));
        assert_eq!(preview_platform(Some("false")), None);
        assert_eq!(preview_platform(None), None);

        let mut handle = DaemonHandle::new(Arc::new(Mutex::new(Config::default())), None);
        handle.ui_preview = true;
        handle.start();
        assert!(handle.is_running());
        assert!(!handle.status.connected());
        assert!(
            handle.thread.is_none(),
            "隔离预览不能创建会执行捕获和联网的守护线程"
        );
        handle.stop();
        assert!(!handle.is_running());
        assert_eq!(handle.status.phase(), ConnectionPhase::Idle);
        handle.forget_devices();
        assert!(handle.thread.is_none(), "预览解除配对不能启动文件清理线程");
        assert!(
            handle
                .take_operation_result()
                .is_some_and(|result| result.is_ok())
        );
    }
}
