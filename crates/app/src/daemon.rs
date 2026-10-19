//! 守护进程主循环（DEVELOPMENT.md §5.1）：发现/连接 → 配对 → 捕获→engine→网络→注入。
//!
//! 身份与信任列表持久化到配置目录文件（cert.der/key.der/trust.json，私钥设 0600）；
//! 已信任对端免核对配对码（TOFU + pinning）；已配对后陌生指纹拒连（验收标准 6，
//! T-703，重新配对需删 trust.json）。屏幕尺寸经 CLI/配置。

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use ms_core::control::Action;
use ms_core::engine::{EdgeConfig, Engine, EngineOut, Inject};
use ms_core::input_map::ModifierMode;
use ms_net::discovery::Discovery;
use ms_net::quic::{Conn, MsgSendStream};
use ms_net::{DisconnectReason, Endpoint, Identity, TrustStore};
use ms_platform::{
    Button, ClipboardAccess, HidKey, InputCapture, InputEvent, InputInject, Platform, Rect,
};
use ms_proto::{Edge, Layout, Msg, Ping, Pong};

use crate::cli::Args;
use crate::clip;
use crate::config::{Config, DEFAULT_PEER_PORT, normalize_peer_address};
use crate::status::ConnStatus;
use crate::transfers;
use crate::xfer;

/// 引擎相关的「可实时调整」设置（设置界面改动后无需重启互控即生效）。
#[derive(Clone, Copy, PartialEq, Debug)]
struct LiveSettings {
    side: Edge,
    no_takeover: bool,
    invert_scroll: bool,
    scroll_speed: f64,
    swap_modifiers: bool,
    edge_push: f64,
    return_push: f64,
}

impl LiveSettings {
    /// 从共享配置取一份快照（方位非法时回退 Right）。
    fn from_config(cfg: &Arc<Mutex<Config>>) -> Self {
        let c = cfg.lock().unwrap_or_else(|e| e.into_inner());
        Self {
            side: parse_side(&c.side).unwrap_or(Edge::Right),
            // mac 端固定从属（T-608）：UI 已按「Windows=控制端、mac=被控端」裁剪（T-606），
            // 引擎与之对齐。否则 mac 侧残留的 side 配置会把对应屏幕边变成蓄力穿越边，
            // 挡住自动隐藏的程序坞，推够 edge_push 还会误穿越去控制 Windows。
            no_takeover: cfg!(target_os = "macos") || c.no_takeover,
            invert_scroll: c.invert_scroll,
            scroll_speed: crate::config::normalize_scroll_speed(c.scroll_speed),
            swap_modifiers: c.swap_modifiers,
            edge_push: c.edge_push,
            return_push: c.return_push,
        }
    }

    /// 应用到引擎（方位/从属/滚轮/修饰键映射）。
    fn apply(&self, engine: &mut Engine) {
        engine.set_layout(Layout {
            peer_position: self.side,
        });
        engine.set_can_initiate(!self.no_takeover);
        engine.set_invert_scroll(self.invert_scroll);
        engine.set_scroll_speed(self.scroll_speed);
        engine.set_edge_push(self.edge_push);
        engine.set_return_push(self.return_push);
        engine.set_modifier_mode(if self.swap_modifiers {
            ModifierMode::SwapCtrlGui
        } else {
            ModifierMode::Position
        });
    }
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

/// 输入授权是否就绪（供设置界面引导页查询，FR-803）。
pub fn input_permission_granted() -> bool {
    make_platform().has_input_permission()
}

/// 引导用户开启输入授权（打开系统设置页等）。
pub fn open_input_permission_settings() {
    make_platform().open_input_permission_settings();
}

/// 把一条内容写回系统剪贴板（供设置窗剪贴板历史「回填」使用，T-808）。
pub fn write_clipboard(content: &ms_platform::ClipboardContent) -> Result<(), String> {
    let mut c = make_platform().clipboard().map_err(|e| e.to_string())?;
    c.write(content).map_err(|e| e.to_string())
}

fn parse_side(s: &str) -> Result<Edge, String> {
    match s {
        "right" => Ok(Edge::Right),
        "left" => Ok(Edge::Left),
        "top" => Ok(Edge::Top),
        "bottom" => Ok(Edge::Bottom),
        other => Err(format!("无效方位 {other}（应为 right/left/top/bottom）")),
    }
}

fn code_to_button(code: u8) -> Button {
    match code {
        1 => Button::Right,
        2 => Button::Middle,
        3 => Button::X1,
        4 => Button::X2,
        _ => Button::Left,
    }
}

/// 同步入口：建运行时跑异步主循环。返回是否正常结束。
pub fn run(args: &Args) -> bool {
    // CLI 直跑：用 CLI 参数构一份共享配置（无设置界面，运行期不变）。
    let config = Arc::new(Mutex::new(Config::from_args(args)));
    run_until(
        args,
        config,
        Arc::new(AtomicBool::new(true)),
        Arc::new(ConnStatus::new()),
    )
}

/// 可外部停止的入口：`running` 置 false 时主循环尽快退出（托盘"暂停"用）。
/// `config` 为设置界面共享的实时配置，运行期可改、引擎设置即时生效（无需重启互控）。
/// `status` 由托盘共享，供连接质量显示（FR-606）。
pub fn run_until(
    args: &Args,
    config: Arc<Mutex<Config>>,
    running: Arc<AtomicBool>,
    status: Arc<ConnStatus>,
) -> bool {
    status.set_connected(false);
    status.clear_error();
    // UI 与 CLI 共用此入口：先验证，再创建身份/监听端点，非法地址不能进入重试循环。
    let mut args = args.clone();
    args.connect = match normalize_peer_address(args.connect.as_deref().unwrap_or_default()) {
        Ok(connect) => connect,
        Err(message) => {
            status.set_error(message);
            tracing::warn!(target: "mouseshare::app", "对端地址格式不正确，互控未启动");
            return false;
        }
    };
    args.serve = args.connect.is_none();
    let rt = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => {
            status.set_error("无法启动连接服务，请退出软件后重新打开。");
            tracing::error!(target: "mouseshare::app", error = %e, "创建运行时失败");
            return false;
        }
    };
    let result = rt.block_on(run_async(&args, &config, running, Arc::clone(&status)));
    // 先停止运行时中的后台传输任务，随后统一写取消终态，避免任务被 drop 后留下永久进度。
    drop(rt);
    transfers::center().cancel_active();
    finish_run(result, &status)
}

/// 所有异步退出路径共用的状态收尾，包括重连退避期间的提前暂停。
fn finish_run(result: Result<(), String>, status: &ConnStatus) -> bool {
    status.set_connected(false);
    match result {
        Ok(()) => {
            status.clear_error();
            true
        }
        Err(e) => {
            if status.last_error().is_none() {
                status.set_error(
                    "连接服务启动失败，请退出软件后重新打开，并确认 Mac 已授予辅助功能权限。",
                );
            }
            tracing::error!(target: "mouseshare::app", error = %e, "守护进程退出");
            false
        }
    }
}

/// 应用配置目录（mac `~/Library/Application Support/FesilentMouse` / win `%APPDATA%\MouseShare`）。
fn data_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join("Library/Application Support/FesilentMouse"))
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA").map(|a| PathBuf::from(a).join("FesilentMouse"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        None
    }
}

/// 写文件，并在 unix 上把权限设为 0600（仅当前用户可读，保护私钥）。
fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, data)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// 从配置目录读本机身份（DER 文件）；无则生成并保存（配对持久化，T-203）。
/// 不用钥匙串（避免密码弹窗），私钥文件设 0600 权限。无配置目录时用临时身份。
fn load_or_create_identity() -> Result<Identity, String> {
    let dir = data_dir();
    if let Some(d) = &dir {
        if let (Ok(c), Ok(k)) = (
            std::fs::read(d.join("cert.der")),
            std::fs::read(d.join("key.der")),
        ) && !c.is_empty()
            && !k.is_empty()
        {
            tracing::info!(target: "mouseshare::app", "复用已保存的本机身份");
            return Ok(Identity::from_der(c, k));
        }
    }
    let id = Identity::generate().map_err(|e| e.to_string())?;
    if let Some(d) = &dir {
        let _ = std::fs::create_dir_all(d);
        let _ = write_private(&d.join("cert.der"), id.cert_der());
        let _ = write_private(&d.join("key.der"), id.key_der());
        tracing::info!(target: "mouseshare::app", "已生成并保存本机身份");
    } else {
        tracing::warn!(target: "mouseshare::app", "无法定位配置目录，使用临时身份（重连需重新核对配对码）");
    }
    Ok(id)
}

async fn run_async(
    args: &Args,
    config: &Arc<Mutex<Config>>,
    running: Arc<AtomicBool>,
    status: Arc<ConnStatus>,
) -> Result<(), String> {
    let platform = make_platform();
    if !platform.has_input_permission() {
        let message = "请在 Mac 的「系统设置 → 隐私与安全性 → 辅助功能」中勾选本软件，再启用互控。";
        status.set_error(message);
        return Err(message.into());
    }
    let port = args.port.unwrap_or(DEFAULT_PEER_PORT);
    let id = load_or_create_identity().map_err(|e| {
        status.set_error("无法读取或保存配对身份，请确认当前用户可以写入软件的配置目录。");
        e
    })?;
    let bind = SocketAddr::from((
        Ipv4Addr::UNSPECIFIED,
        if args.connect.is_some() { 0 } else { port },
    ));
    let ep = Endpoint::new(bind, &id).map_err(|e| {
        status.set_error("无法打开连接端口，请确认没有重复运行的软件实例，再暂停并重新启用互控。");
        e.to_string()
    })?;
    let trust_path = data_dir().map(|d| d.join("trust.json"));

    // 自动重连循环：会话因断连结束、但仍处「启用」态（running=true）时回到发现/重连，
    // 只有用户暂停（running=false）才真正退出。这样一端暂停不会永久拖垮对端，且 Wi-Fi
    // 抖动后能自愈（endpoint/identity 在循环外复用，避免端口重绑）。
    let mut attempt: u32 = 0;
    // 已拒绝过的陌生指纹（本次运行内只 warn 一次，重连循环反复撞上时降为 debug 防刷屏）
    let mut rejected_logged: std::collections::HashSet<[u8; 32]> = std::collections::HashSet::new();
    while running.load(Ordering::Acquire) {
        if attempt > 0 {
            // 断连后短暂退避再重连（不忙等）；退避期间也及时响应暂停
            for _ in 0..8 {
                if !running.load(Ordering::Acquire) {
                    return Ok(());
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
        attempt += 1;

        let conn = match establish_conn(&ep, args, &id, port, &running).await {
            Ok(Some(c)) => c,
            Ok(None) => break, // 发现等待中收到暂停
            Err(e) => {
                if running.load(Ordering::Acquire) {
                    status.set_error("尚未连接。请确认两台电脑在同一局域网，Mac 端地址留空并启用互控；Windows 端填写 Mac 的 IP，且防火墙允许 UDP 53728。");
                }
                tracing::warn!(target: "mouseshare::app", error = %e, "建立连接失败，将重试");
                continue;
            }
        };
        tracing::info!(target: "mouseshare::app", peer = %conn.remote_addr(), "已连接对端");
        if !check_trust(&conn, &id, trust_path.as_deref(), &mut rejected_logged) {
            status.set_error("配对验证未通过。请确认连接的是已配对的电脑，必要时重新配对。");
            conn.close();
            continue;
        }

        // Windows：DSCP 不能经 setsockopt（系统忽略），连接建立后把底层 UDP socket 挂到
        // qWAVE QoS flow（EF 优先级，NFR-08/T-708），guard 持有到本次会话结束、重连时对
        // 新连接重挂。macOS/Linux 上 DSCP 已在建 socket 时由 set_tos 生效，无需此步。
        #[cfg(target_os = "windows")]
        let _qos_flow =
            match ms_platform_windows::qos::add_socket_to_flow(ep.raw_socket(), conn.remote_addr())
            {
                Ok(flow) => {
                    tracing::info!(
                        target: "mouseshare::win",
                        flow_id = flow.flow_id(),
                        "已为 QUIC socket 挂上 qWAVE QoS flow(EF/DSCP46)"
                    );
                    Some(flow)
                }
                Err(e) => {
                    tracing::warn!(
                        target: "mouseshare::win",
                        error = %e,
                        "qWAVE QoS 标记失败，连接继续但无 DSCP 优先级"
                    );
                    None
                }
            };

        match run_session(&conn, &platform, args, config, &running, &status).await {
            Err(e) => {
                if running.load(Ordering::Acquire) {
                    status.set_error("互控会话启动失败。请确认 Mac 已授予辅助功能权限、两端均已启用互控，再暂停并重新连接。");
                }
                tracing::warn!(target: "mouseshare::app", error = %e, "会话异常");
            }
            Ok(()) if running.load(Ordering::Acquire) => {
                status.set_error("连接已中断，正在重连。请确认 Mac 仍在运行、已启用互控，并且两台电脑仍在同一局域网。");
            }
            Ok(()) => {}
        }
        // 网络会话结束后旧传输不应跟随下一次连接继续等待确认。
        transfers::center().cancel_active();
        status.set_connected(false);
        if running.load(Ordering::Acquire) {
            tracing::warn!(target: "mouseshare::app", "连接断开，自动重连中");
        }
    }
    tracing::info!(target: "mouseshare::app", "已退出互控");
    Ok(())
}

/// 建立一条连接：`--connect` 直连，否则 mDNS 自动发现。发现等待期间收到暂停返回 `Ok(None)`。
async fn establish_conn(
    ep: &Endpoint,
    args: &Args,
    id: &Identity,
    port: u16,
    running: &Arc<AtomicBool>,
) -> Result<Option<Conn>, String> {
    if let Some(addr) = &args.connect {
        let sa: SocketAddr = addr
            .parse()
            .map_err(|_| "对端地址格式不正确，请填写 IPv4 或 IPv4:端口。".to_string())?;
        tracing::info!(target: "mouseshare::app", addr = %sa, "手动直连对端");
        return Ok(Some(ep.connect(sa).await.map_err(|e| e.to_string())?));
    }
    // mDNS 自动发现：广播本机 + 浏览对端；device_id 较小的一方主动连接、较大的监听，
    // 避免双向连接。同时仍 accept（手动 --connect 的对端也能连进来）。device_id 用身份
    // 指纹前 16 字节 hex（≤63 字节符合 DNS label；稳定唯一）。
    let own_id: String = id.fingerprint()[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let mut disco = Discovery::new().map_err(|e| e.to_string())?;
    disco
        .advertise(&own_id, "MouseShare", port)
        .map_err(|e| e.to_string())?;
    let mut rx = disco.browse(own_id.clone()).map_err(|e| e.to_string())?;
    tracing::info!(target: "mouseshare::app", port, "监听中，mDNS 自动发现（同网段另一台开着会自动连）");
    let conn = loop {
        tokio::select! {
            accepted = ep.accept() => {
                break accepted.ok_or("endpoint 已关闭")?.map_err(|e| e.to_string())?;
            }
            peer = rx.recv() => {
                if let Some(peer) = peer
                    && own_id.as_str() < peer.device_id.as_str()
                    && let Some(ip) = peer.addrs.first()
                {
                    let sa = SocketAddr::new(*ip as IpAddr, peer.port);
                    tracing::info!(target: "mouseshare::app", name = %peer.name, addr = %sa, "发现对端，连接中");
                    break ep.connect(sa).await.map_err(|e| e.to_string())?;
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(300)) => {
                if !running.load(Ordering::Acquire) {
                    disco.shutdown();
                    tracing::info!(target: "mouseshare::app", "已暂停（发现等待时退出）");
                    return Ok(None);
                }
            }
        }
    };
    disco.shutdown();
    Ok(Some(conn))
}

/// 信任判定（纯函数，可单测）：验收标准 6「未配对设备连接被拒绝」。
/// - 取不到对端指纹（mTLS 异常）→ 拒绝：无法固定身份就不能建会话；
/// - 对端指纹 = 本机指纹 → 放行：TLS 验签已证明对方持有本机私钥，即本机双实例
///   回环调试（--loopback / T-702 浸泡，两实例共享身份文件），无冒充可能；
/// - 信任列表为空（全新安装）→ 首次配对 TOFU：显示配对码（H-4 人工核对两端一致）并信任；
/// - 列表非空且指纹在列 → 放行；
/// - 列表非空且指纹不在列 → 拒绝（SPEC §5 AuthRejected）：已配对后陌生设备不可再
///   TOFU 混入。重新配对需删除配置目录的 trust.json（UI 解除配对归 FR-105，未实现）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrustDecision {
    /// 已在信任列表（或为本机自身身份），放行。
    Trusted,
    /// 全新安装首次配对：TOFU 信任并持久化。
    FirstPair,
    /// 拒绝建会话（陌生指纹 / 无指纹）。
    Rejected,
}

fn decide_trust(
    peer_fp: Option<&[u8; 32]>,
    own_fp: &[u8; 32],
    trust: &TrustStore,
) -> TrustDecision {
    match peer_fp {
        None => TrustDecision::Rejected,
        Some(fp) if fp == own_fp => TrustDecision::Trusted,
        Some(fp) if trust.is_trusted(fp) => TrustDecision::Trusted,
        Some(_) if trust.is_empty() => TrustDecision::FirstPair,
        Some(_) => TrustDecision::Rejected,
    }
}

/// 信任列表（配置目录文件）：已信任对端免核对；全新安装首次配对打印配对码并记住
/// （TOFU + pinning）；已配对后遇到陌生指纹**拒连**（返回 false，调用方须关连接）。
fn check_trust(
    conn: &Conn,
    id: &Identity,
    trust_path: Option<&Path>,
    rejected_logged: &mut std::collections::HashSet<[u8; 32]>,
) -> bool {
    let mut trust = trust_path
        .and_then(|p| std::fs::read(p).ok())
        .map(|b| TrustStore::from_bytes(&b))
        .unwrap_or_default();
    let peer_fp = conn.peer_fingerprint();
    match decide_trust(peer_fp.as_ref(), &id.fingerprint(), &trust) {
        TrustDecision::Trusted => {
            tracing::info!(target: "mouseshare::app", "已信任的对端，自动连接（免核对配对码）");
            true
        }
        TrustDecision::FirstPair => {
            // 首次配对（信任列表为空时才会走到这里，peer_fp 必有值）
            if let Some(sas) = conn.compute_sas(&id.fingerprint()) {
                tracing::info!(target: "mouseshare::app", %sas, "新对端配对码（请核对两端一致；之后将自动信任，免再核对）");
            }
            if let Some(fp) = peer_fp {
                trust.trust(&fp);
            }
            if let Some(p) = trust_path {
                if let Some(d) = p.parent() {
                    let _ = std::fs::create_dir_all(d);
                }
                let _ = std::fs::write(p, trust.to_bytes());
            }
            true
        }
        TrustDecision::Rejected => {
            let fp_hex = peer_fp.map(|fp| ms_net::fingerprint_hex(&fp));
            let first_time = match peer_fp {
                Some(fp) => rejected_logged.insert(fp),
                None => true,
            };
            if first_time {
                tracing::warn!(
                    target: "mouseshare::app",
                    reason = ?DisconnectReason::AuthRejected,
                    peer = %conn.remote_addr(),
                    fp = fp_hex.as_deref().unwrap_or("<无指纹>"),
                    "拒绝未配对设备（验收标准 6）：本机已有信任对端，陌生设备不可混入；重新配对请删除配置目录的 trust.json"
                );
            } else {
                tracing::debug!(
                    target: "mouseshare::app",
                    reason = ?DisconnectReason::AuthRejected,
                    peer = %conn.remote_addr(),
                    "再次拒绝同一未配对设备"
                );
            }
            false
        }
    }
}

/// 检测本机虚拟屏幕尺寸。开机/唤醒后显示器可能短暂返回 0×0 / 过渡值（CGMainDisplayID
/// 未就绪等），若直接用会导致跨屏入点错位（如对端光标进来落到角落）。故校验尺寸非法
/// （w/h ≤ 0）时短暂重试（最多约 2s），都拿不到再回退默认。修复 T-509。
async fn detect_own_screen<P: Platform>(platform: &P, fallback: Rect) -> Rect {
    for attempt in 0..10u32 {
        if let Ok(rect) = platform
            .screen_info()
            .and_then(|mut s| s.virtual_screen())
            .map(|vs| vs.rect)
            && rect.w > 0.0
            && rect.h > 0.0
        {
            tracing::info!(
                target: "mouseshare::app",
                w = rect.w, h = rect.h, attempt,
                "本机屏幕尺寸已检测"
            );
            return rect;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    tracing::warn!(
        target: "mouseshare::app",
        w = fallback.w, h = fallback.h,
        "屏幕尺寸多次检测无效，回退默认"
    );
    fallback
}

/// 键盘链路分段计数（T-613 取证）：捕获→转发→接收→注入逐段计数，
/// 用于从两端日志区分「钩子被摘除（cap 不涨）/ 事件未转发（cap 涨 fwd 不涨）/
/// 注入被吃（对端 recv 涨 inj 不涨，或 inj 涨但应用无反应）」三种断链情形。
/// 仅心跳时打日志，计数本身是普通字段自增，不进热路径。
#[derive(Default, Clone, Copy, PartialEq)]
struct KeyLinkStats {
    /// 本机捕获通道收到的 Key 事件数（0 ≈ 钩子/tap 已死）。
    cap: u64,
    /// 发往对端的 KeyEvent 消息数。
    fwd: u64,
    /// 收到对端的 KeyEvent 消息数。
    recv: u64,
    /// 实际执行注入的 Key 命令数。
    inj: u64,
}

/// 跑一次会话：捕获→engine→网络→注入，直到断连或用户暂停（running=false）返回。
async fn run_session<P: Platform>(
    conn: &Conn,
    platform: &P,
    args: &Args,
    config: &Arc<Mutex<Config>>,
    running: &Arc<AtomicBool>,
    status: &Arc<ConnStatus>,
) -> Result<(), String> {
    // 平台层
    let mut capture = platform.input_capture().map_err(|e| e.to_string())?;
    let mut inject = platform.input_inject().map_err(|e| e.to_string())?;

    // 本机屏幕：自动检测（开机过渡值会重试校验，T-509），失败回退 CLI --screen
    let own = detect_own_screen(
        platform,
        Rect {
            x: 0.0,
            y: 0.0,
            w: args.screen.0,
            h: args.screen.1,
        },
    )
    .await;
    tracing::info!(target: "mouseshare::app", w = own.w, h = own.h, "本机屏幕");

    // 引擎消息流：各开一条单向流发、各收一条。
    // ⚠ QUIC 的 open_uni 在「首次写数据」前对端 accept_uni 不会返回；若双方都
    // 先 open 再 accept 而都未写，会互等死锁直到空闲超时。故必须：open → 立即
    // send（触发对端 accept）→ 再 accept 自己这侧的对端流。
    let mut send_stream = conn.open_msg_stream().await.map_err(|e| e.to_string())?;

    // 交换屏幕尺寸（FR-203：用对端真实尺寸按比例映射）。先发以解开 accept 互等。
    send_stream
        .send(&Msg::from(ms_proto::ScreenInfo {
            virtual_rect: ms_proto::Rect {
                x: own.x,
                y: own.y,
                w: own.w,
                h: own.h,
            },
            scale: 1.0,
        }))
        .await
        .map_err(|e| e.to_string())?;

    let peer_fallback = Rect {
        x: 0.0,
        y: 0.0,
        w: args.peer_screen.0,
        h: args.peer_screen.1,
    };
    let mut recv_stream = conn.accept_msg_stream().await.map_err(|e| e.to_string())?;
    let peer = match recv_stream.recv().await.map_err(|e| e.to_string())? {
        Some(Msg::ScreenInfo(si)) if si.virtual_rect.w > 0.0 && si.virtual_rect.h > 0.0 => {
            tracing::info!(
                target: "mouseshare::app",
                w = si.virtual_rect.w, h = si.virtual_rect.h,
                "对端屏幕尺寸"
            );
            Rect {
                x: si.virtual_rect.x,
                y: si.virtual_rect.y,
                w: si.virtual_rect.w,
                h: si.virtual_rect.h,
            }
        }
        // 对端尺寸缺失/非法（开机过渡）：回退默认，避免跨屏映射错位
        other => {
            tracing::warn!(
                target: "mouseshare::app",
                ?other,
                w = peer_fallback.w, h = peer_fallback.h,
                "对端屏幕尺寸无效，回退默认"
            );
            peer_fallback
        }
    };
    tracing::info!(target: "mouseshare::app", w = peer.w, h = peer.h, "对端屏幕");
    // 引擎设置从共享配置取初值并应用（方位/从属/滚轮/修饰键），运行期可热更新（见主循环）。
    let mut live = LiveSettings::from_config(config);
    let mut engine = Engine::new(
        own,
        peer,
        Layout {
            peer_position: live.side,
        },
        EdgeConfig::default(),
    );
    live.apply(&mut engine);
    tracing::info!(
        target: "mouseshare::app",
        side = ?live.side,
        no_takeover = live.no_takeover,
        invert_scroll = live.invert_scroll,
        scroll_speed = live.scroll_speed,
        swap_modifiers = live.swap_modifiers,
        edge_push = live.edge_push,
        return_push = live.return_push,
        "引擎设置已应用（设置界面改动即时生效）"
    );

    // 文件传输（T-504，FR-501~505）：默认开启。复制文件后切换控制权即发往对端接收目录。
    let file_transfer = !args.no_clipboard;
    if file_transfer {
        xfer::spawn_stale_cleanup();
        tracing::info!(target: "mouseshare::app", "文件传输已启用：复制文件后切换控制权即发往对端「下载/MouseShare」");
    }

    // 剪贴板同步（T-405，FR-401~405）：控制权切换时经独立流同步，与键鼠流隔离。
    // 接收任务必须在控制流已 accept（上面 recv_stream）之后启动，避免抢走控制流。
    let clip_state = clip::shared_state(!args.no_clipboard);
    let mut clip_read: Option<Box<dyn ClipboardAccess>> = None;
    if args.no_clipboard {
        tracing::info!(target: "mouseshare::app", "剪贴板同步已关闭（--no-clipboard）");
    } else {
        match platform.clipboard() {
            Ok(c) => clip_read = Some(c),
            Err(e) => {
                tracing::warn!(target: "mouseshare::app", error = %e, "剪贴板读句柄不可用，已禁用同步")
            }
        }
        match clip::spawn_receiver(conn.clone(), platform, clip_state.clone(), running.clone()) {
            Ok(_handle) if clip_read.is_some() => {
                tracing::info!(target: "mouseshare::app", "剪贴板同步已启用（控制权切换时同步；--no-clipboard 可关闭）");
            }
            Ok(_handle) => {} // 读句柄不可用上面已提示
            Err(e) => {
                tracing::warn!(target: "mouseshare::app", error = %e, "剪贴板接收任务启动失败")
            }
        }
    }

    // 捕获 → tokio 通道。丢弃计数（T-613 取证）：通道满时 try_send 静默丢事件，
    // 原子自增无堆分配无锁，符合热路径红线（DEVELOPMENT §6.2）。
    let cap_dropped = Arc::new(AtomicU64::new(0));
    let (tx, mut rx) = tokio::sync::mpsc::channel::<InputEvent>(2048);
    capture
        .start(
            Box::new({
                let dropped = Arc::clone(&cap_dropped);
                move |ev| {
                    if tx.try_send(ev).is_err() {
                        dropped.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }),
            false,
        )
        .map_err(|e| format!("启动捕获失败: {e}（macOS 需辅助功能授权）"))?;

    tracing::info!(target: "mouseshare::app", side = %args.side, "MouseShare 运行中：鼠标移到该侧边缘即可穿越到对端");
    // 连接质量：标记已连，初始记一次 RTT（供托盘显示，FR-606）
    status.set_connected(true);
    status.set_rtt_ms(conn.rtt_ms());

    let mut hb = tokio::time::interval(std::time::Duration::from_secs(1));
    let mut seq = 0u32;
    let mut key_stats = KeyLinkStats::default();
    let mut key_stats_logged = (KeyLinkStats::default(), 0u64);

    while running.load(Ordering::Acquire) {
        tokio::select! {
            _ = conn.wait_closed() => {
                tracing::warn!(target: "mouseshare::app", "连接断开");
                status.inc_disconnect();
                break;
            }
            _ = hb.tick() => {
                seq = seq.wrapping_add(1);
                let _ = send_stream.send(&Msg::from(Ping { seq, t0: 0 })).await;
                status.set_rtt_ms(conn.rtt_ms());
                // 设置即时生效：每秒重读共享配置，引擎相关项有变化则热应用（无需重启互控）。
                let cur = LiveSettings::from_config(config);
                if cur != live {
                    cur.apply(&mut engine);
                    live = cur;
                    tracing::info!(
                        target: "mouseshare::app",
                        side = ?live.side, no_takeover = live.no_takeover,
                        invert_scroll = live.invert_scroll, swap_modifiers = live.swap_modifiers,
                        scroll_speed = live.scroll_speed,
                        edge_push = live.edge_push, return_push = live.return_push,
                        "设置已即时更新"
                    );
                }
                // T-613 取证：键盘链路计数有变化才打一行（最多 1 行/秒），
                // 两端日志按时间对齐即可定位断链发生在哪一段。
                let drops = cap_dropped.load(Ordering::Relaxed);
                if (key_stats, drops) != key_stats_logged {
                    tracing::info!(
                        target: "mouseshare::app",
                        state = ?engine.state(),
                        cap = key_stats.cap, fwd = key_stats.fwd,
                        recv = key_stats.recv, inj = key_stats.inj,
                        dropped = drops,
                        "键盘链路计数（T-613 取证）"
                    );
                    key_stats_logged = (key_stats, drops);
                }
            }
            Some(ev) = rx.recv() => {
                if matches!(ev, InputEvent::Key { .. }) {
                    key_stats.cap += 1;
                }
                let prev = engine.state();
                let out = engine.feed_local(ev);
                let now = engine.state();
                apply(out, &mut capture, &mut inject, &mut send_stream, &mut key_stats).await;
                clip::on_transition(prev, now, conn, clip_read.as_mut(), &clip_state, file_transfer);
            }
            r = recv_stream.recv() => {
                match r {
                    Ok(Some(Msg::Ping(p))) => {
                        let _ = send_stream.send(&Msg::from(Pong { seq: p.seq, t0: p.t0 })).await;
                    }
                    Ok(Some(Msg::Pong(_))) => {}
                    Ok(Some(msg)) => {
                        if matches!(msg, Msg::KeyEvent(_)) {
                            key_stats.recv += 1;
                        }
                        let prev = engine.state();
                        let out = engine.feed_peer(msg);
                        let now = engine.state();
                        apply(out, &mut capture, &mut inject, &mut send_stream, &mut key_stats).await;
                        clip::on_transition(prev, now, conn, clip_read.as_mut(), &clip_state, file_transfer);
                    }
                    _ => {
                        tracing::warn!(target: "mouseshare::app", "接收流结束");
                        status.inc_disconnect();
                        break;
                    }
                }
            }
        }
    }

    // 安全回退：交还控制、释放按键
    status.set_connected(false);
    let out = engine.on_disconnect();
    apply(
        out,
        &mut capture,
        &mut inject,
        &mut send_stream,
        &mut key_stats,
    )
    .await;
    let _ = inject.release_all();
    let _ = capture.stop();
    tracing::info!(target: "mouseshare::app", "会话结束");
    Ok(())
}

/// 执行引擎输出：发消息、执行平台动作、注入。
async fn apply(
    out: EngineOut,
    capture: &mut Box<dyn InputCapture>,
    inject: &mut Box<dyn InputInject>,
    send_stream: &mut MsgSendStream,
    key_stats: &mut KeyLinkStats,
) {
    for msg in out.to_peer {
        if matches!(msg, Msg::KeyEvent(_)) {
            key_stats.fwd += 1;
        }
        let _ = send_stream.send(&msg).await;
    }
    for action in out.actions {
        match action {
            Action::SetGrab(on) => {
                let _ = capture.set_grab(on);
            }
            Action::SetCursorVisible(visible) => {
                let _ = capture.confine_cursor(!visible);
            }
            // warp/reset 在 v1 由 grab+confine 近似处理，暂不单独 warp
            _ => {}
        }
    }
    for cmd in out.inject {
        match cmd {
            Inject::MoveTo { x, y } => {
                let _ = inject.mouse_move_to(x, y);
            }
            Inject::Key { hid, down } => {
                key_stats.inj += 1;
                let _ = inject.key(HidKey(hid), down);
            }
            Inject::Button { btn, down } => {
                let _ = inject.mouse_button(code_to_button(btn), down);
            }
            Inject::Scroll { dx, dy, precise } => {
                let _ = inject.scroll(dx as f64, dy as f64, precise);
            }
            Inject::ReleaseAll => {
                let _ = inject.release_all();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn 正常暂停统一清理错误而失败保留可见提示() {
        let status = ConnStatus::new();
        status.set_error("连接失败，正在重试");
        assert!(finish_run(Ok(()), &status));
        assert_eq!(status.last_error(), None);
        assert!(!status.connected());

        status.set_error("请在 Mac 端授予辅助功能权限");
        assert!(!finish_run(Err("internal detail".into()), &status));
        assert_eq!(
            status.last_error().as_deref(),
            Some("请在 Mac 端授予辅助功能权限")
        );

        status.clear_error();
        assert!(!finish_run(Err("internal detail".into()), &status));
        let error = status.last_error().unwrap();
        assert!(error.contains("连接服务启动失败"));
        assert!(!error.contains("internal detail"));
    }

    #[test]
    fn 非法连接地址在启动前退出并保留可见错误() {
        let args = Args {
            connect: Some("private pasted text".into()),
            ..Args::default()
        };
        let status = Arc::new(ConnStatus::new());
        let config = Arc::new(Mutex::new(Config::from_args(&args)));
        assert!(!run_until(
            &args,
            config,
            Arc::new(AtomicBool::new(true)),
            Arc::clone(&status),
        ));
        assert!(!status.connected());
        let error = status.last_error().unwrap();
        assert!(error.contains("有效的局域网 IPv4"));
        assert!(!error.contains("private pasted text"));
    }

    fn cfg(side: &str, no_takeover: bool, invert: bool, swap: bool) -> Arc<Mutex<Config>> {
        Arc::new(Mutex::new(Config {
            side: side.into(),
            no_takeover,
            invert_scroll: invert,
            swap_modifiers: swap,
            ..Config::default()
        }))
    }

    #[test]
    fn live_settings_读取与变更检测() {
        let c = cfg("left", true, false, true);
        let s = LiveSettings::from_config(&c);
        assert_eq!(s.side, Edge::Left);
        assert!(s.no_takeover && !s.invert_scroll && s.swap_modifiers);
        // 无变化 → 相等（不触发热应用）
        assert_eq!(s, LiveSettings::from_config(&c));
        assert_eq!(s.edge_push, 100.0, "edge_push 默认 100");
        assert_eq!(s.return_push, 60.0, "return_push 默认 60");
        assert_eq!(s.scroll_speed, 1.0, "scroll_speed 默认 1.0");
        // 改一项 → 不等（触发热应用）
        c.lock().unwrap().invert_scroll = true;
        let s2 = LiveSettings::from_config(&c);
        assert_ne!(s, s2);
        assert!(s2.invert_scroll);
        // 改边缘推挤 → 触发热应用
        c.lock().unwrap().edge_push = 80.0;
        let s3 = LiveSettings::from_config(&c);
        assert_ne!(s2, s3);
        assert_eq!(s3.edge_push, 80.0);
        // 倍率按 0.1 档位规范化，并参与变化检测。
        c.lock().unwrap().scroll_speed = 1.46;
        let s4 = LiveSettings::from_config(&c);
        assert_eq!(s4.scroll_speed, 1.5);
        assert_ne!(s3, s4);
        // 非法方位回退 Right
        assert_eq!(
            LiveSettings::from_config(&cfg("nonsense", false, false, false)).side,
            Edge::Right
        );
    }

    #[test]
    fn mac_端固定从属_不随配置发起穿越() {
        // T-608 复现：UI 已按「Windows=控制端、mac=被控端」裁剪（T-606），但引擎若仍按
        // 配置允许 mac 发起穿越，残留的 side（用户实测 "bottom"）会把 mac 屏幕对应边
        // 变成 100px 蓄力穿越边——挡住自动隐藏的程序坞，推够还会误穿越去控制 Windows。
        let s = LiveSettings::from_config(&cfg("bottom", false, false, true));
        if cfg!(target_os = "macos") {
            assert!(s.no_takeover, "mac 端必须固定从属（no_takeover 恒 true）");
        } else {
            assert!(!s.no_takeover, "非 mac 端仍按配置取值");
        }
    }

    #[test]
    fn 信任判定_未配对设备被拒绝() {
        // 验收标准 6：已配对后陌生设备连接必须被拒绝（T-703）
        let own = [9u8; 32];
        let paired = [1u8; 32];
        let stranger = [2u8; 32];
        let mut trust = TrustStore::new();

        // 全新安装（信任列表空）：首次配对走 TOFU（H-4 人工核对配对码）
        assert_eq!(
            decide_trust(Some(&paired), &own, &trust),
            TrustDecision::FirstPair,
            "空信任列表应允许首次配对"
        );
        trust.trust(&paired);

        // 已配对对端：放行
        assert_eq!(
            decide_trust(Some(&paired), &own, &trust),
            TrustDecision::Trusted
        );
        // 陌生指纹：拒绝（不可再 TOFU 混入）
        assert_eq!(
            decide_trust(Some(&stranger), &own, &trust),
            TrustDecision::Rejected,
            "已配对后陌生设备必须被拒绝"
        );
        // 取不到指纹（mTLS 异常）：拒绝
        assert_eq!(decide_trust(None, &own, &trust), TrustDecision::Rejected);
        assert_eq!(
            decide_trust(None, &own, &TrustStore::new()),
            TrustDecision::Rejected
        );
        // 本机自身指纹（回环双实例共享身份文件）：恒放行——TLS 验签已证明持有本机私钥
        assert_eq!(
            decide_trust(Some(&own), &own, &trust),
            TrustDecision::Trusted,
            "回环自连不应被拒"
        );
    }
}
