//! 守护进程主循环（DEVELOPMENT.md §5.1）：发现/连接 → 配对 → 捕获→engine→网络→注入。
//!
//! 身份与信任列表持久化到配置目录文件（cert.der/key.der/trust.json，私钥设 0600）；
//! 已信任对端免核对配对码；图形界面首次配对需人工确认，headless 保留 TOFU。
//! 已配对后陌生指纹拒连（验收标准 6，T-703）；可从界面解除配对。屏幕尺寸经 CLI/配置。

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use ms_core::engine::{EdgeConfig, Engine, EngineOut};
use ms_core::input_map::{ModifierMode, Shortcut};
use ms_net::discovery::Discovery;
use ms_net::quic::{Conn, MsgSendStream};
use ms_net::{Backoff, DisconnectReason, Endpoint, HeartbeatTracker, Identity, TrustStore};
use ms_platform::{ClipboardAccess, InputCapture, InputEvent, InputInject, Platform, Rect};
use ms_proto::{Edge, Layout, Msg, Ping, Pong};

use crate::cli::Args;
use crate::clip;
use crate::config::{Config, DEFAULT_PEER_PORT, normalize_peer_address};
use crate::session_safety;
use crate::status::{ConnStatus, ConnectionPhase, PairingRequest};
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
    shortcuts: Option<(Shortcut, Shortcut)>,
    display_offset: f64,
}

impl LiveSettings {
    /// 从共享配置取一份快照（方位非法时回退 Right）。
    fn from_config(cfg: &Arc<Mutex<Config>>) -> Self {
        let c = cfg.lock().unwrap_or_else(|e| e.into_inner());
        Self {
            side: parse_side(&c.side).unwrap_or(Edge::Right),
            // 保持旧Mac默认从属；用户显式打开mac_control后才能发起控制。
            no_takeover: (cfg!(target_os = "macos") && !c.mac_control) || c.no_takeover,
            invert_scroll: c.invert_scroll,
            scroll_speed: crate::config::normalize_scroll_speed(c.scroll_speed),
            swap_modifiers: c.swap_modifiers,
            edge_push: c.edge_push,
            return_push: c.return_push,
            shortcuts: c
                .shortcuts_enabled
                .then_some((c.switch_shortcut.binding(), c.lock_shortcut.binding())),
            display_offset: c.display_offset,
        }
    }

    /// 应用到引擎（方位/从属/滚轮/修饰键映射）。
    fn apply(&self, engine: &mut Engine) {
        engine.set_shortcuts(self.shortcuts);
        engine.set_display_offset(self.display_offset);
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
            status.set_phase(ConnectionPhase::Idle);
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

/// 解除本应用已保存的配对，不重置证书身份。调用方须先停止并等待当前守护线程退出。
pub fn forget_paired_devices() -> Result<(), String> {
    let path = data_dir()
        .ok_or_else(|| "无法定位软件配置目录，请检查当前用户环境。".to_owned())?
        .join("trust.json");
    clear_trust_file(&path)
}

pub fn forget_device(fingerprint: &[u8; 32]) -> Result<(), String> {
    let path = data_dir().ok_or("无法确定配置目录")?.join("trust.json");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err("无法读取配对记录。".into()),
    };
    let mut trust: TrustStore =
        serde_json::from_slice(&bytes).map_err(|_| "配对记录损坏，请使用忘记全部设备恢复。")?;
    trust.revoke(fingerprint);
    write_private(&path, &trust.to_bytes()).map_err(|_| "无法保存配对记录。".into())
}

fn clear_trust_file(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err("无法解除配对，请确认当前用户可以写入软件的配置目录。".to_owned()),
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
        status.set_phase(ConnectionPhase::PermissionRequired);
        return Err(message.into());
    }
    let port = args.port.unwrap_or(DEFAULT_PEER_PORT);
    let id = load_or_create_identity().inspect_err(|_| {
        status.set_error("无法读取或保存配对身份，请确认当前用户可以写入软件的配置目录。");
    })?;
    config
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .devices
        .topology(id.fingerprint())?;
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
    let mut backoff = Backoff::new();
    let mut selected_target = config
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .devices
        .active_target
        .clone();
    // 已拒绝过的陌生指纹（本次运行内只 warn 一次，重连循环反复撞上时降为 debug 防刷屏）
    let mut rejected_logged: std::collections::HashSet<[u8; 32]> = std::collections::HashSet::new();
    while running.load(Ordering::Acquire) {
        if !platform.has_input_permission() {
            status.set_phase(ConnectionPhase::PermissionRequired);
            return Err("输入权限不足，请授权后重新连接。".into());
        }
        if attempt > 0 {
            let delay = backoff.next_delay();
            status.retry_after(delay);
            tokio::select! {
                _ = tokio::time::sleep(delay) => {},
                _ = wait_for_stop(&running) => return Ok(()),
            }
        }
        attempt += 1;

        let conn = match establish_conn(
            &ep,
            args,
            &id,
            &running,
            &status,
            attempt > 1,
            selected_target.as_deref(),
        )
        .await
        {
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
        if let Some(expected) = &selected_target
            && !conn.peer_fingerprint().is_some_and(|fp| {
                ms_core::topology::DeviceId::from_fingerprint(fp).to_string() == *expected
            })
        {
            conn.close();
            status.set_error("连接到的设备身份与所选设备不符，已拒绝；请检查设备地址或重新配对。");
            continue;
        }
        status.set_peer_address(conn.remote_addr().to_string());
        tracing::info!(target: "mouseshare::app", peer = %conn.remote_addr(), "已连接对端");
        let trusted = check_trust(
            &conn,
            &id,
            trust_path.as_deref(),
            &mut rejected_logged,
            &running,
            &status,
        )
        .await;
        let trusted = match trusted {
            Ok(trusted) => trusted,
            Err(error) => {
                conn.close();
                return Err(error);
            }
        };
        if !trusted {
            if !running.load(Ordering::Acquire) {
                conn.close();
                break;
            }
            status.set_error("配对验证未通过。请确认连接的是已配对的电脑，必要时重新配对。");
            conn.close();
            continue;
        }
        // TLS/人工配对期间用户可能已经暂停；不要在停止后启动平台捕获。
        if !running.load(Ordering::Acquire) {
            conn.close();
            break;
        }
        status.set_phase(ConnectionPhase::Connecting);
        if let Some(fp) = conn.peer_fingerprint() {
            status.set_peer_fingerprint(fp);
            // 本轮连接固定认证后的身份，断网不能静默换到另一台已配对电脑。
            if selected_target.is_none() {
                selected_target =
                    Some(ms_core::topology::DeviceId::from_fingerprint(fp).to_string());
            }
        }
        backoff.reset();

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

        let priority = conn
            .peer_fingerprint()
            .is_some_and(|fp| id.fingerprint() < fp);
        match run_session(&conn, &platform, args, config, &running, &status, priority).await {
            Err(e) => {
                if running.load(Ordering::Acquire) {
                    status.set_error(e.clone());
                }
                tracing::warn!(target: "mouseshare::app", reason = ?DisconnectReason::QuicError, error = %e, "会话异常，已执行输入回退");
            }
            Ok(()) if running.load(Ordering::Acquire) => {
                status.set_error("连接已中断，正在重连。请确认 Mac 仍在运行、已启用互控，并且两台电脑仍在同一局域网。");
            }
            Ok(()) => {}
        }
        conn.close(); // 捕获启动前的错误也必须终止旧接收连接。
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
    running: &Arc<AtomicBool>,
    status: &ConnStatus,
    reconnecting: bool,
    selected_target: Option<&str>,
) -> Result<Option<Conn>, String> {
    status.set_phase(if reconnecting {
        ConnectionPhase::Reconnecting
    } else if args.connect.is_some() {
        ConnectionPhase::Connecting
    } else if cfg!(target_os = "macos") {
        ConnectionPhase::Listening
    } else {
        ConnectionPhase::Discovering
    });
    let port = args.port.unwrap_or(DEFAULT_PEER_PORT);
    if let Some(addr) = &args.connect {
        let sa: SocketAddr = addr
            .parse()
            .map_err(|_| "对端地址格式不正确，请填写 IPv4 或 IPv4:端口。".to_string())?;
        tracing::info!(target: "mouseshare::app", addr = %sa, "手动直连对端");
        status.set_peer_address(sa.to_string());
        return tokio::select! {
            connection = ep.connect(sa) => Ok(Some(connection.map_err(|e| e.to_string())?)),
            _ = wait_for_stop(running) => Ok(None),
        };
    }
    // mDNS 自动发现：广播本机 + 浏览对端；device_id 较小的一方主动连接、较大的监听，
    // 避免双向连接。同时仍 accept（手动 --connect 的对端也能连进来）。device_id 用身份
    // 指纹前 16 字节 hex（≤63 字节符合 DNS label；稳定唯一）。
    let own_id: String = id.fingerprint()[..16]
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let known_trust: TrustStore = data_dir()
        .and_then(|p| std::fs::read(p.join("trust.json")).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let mut disco = Discovery::new().map_err(|e| e.to_string())?;
    disco
        .advertise(&own_id, "Fesilent Mouse", port)
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
                    && (!status.adding_device() || !known_trust.contains_discovery_id(&peer.device_id))
                    && selected_target.is_none_or(|target| target.starts_with(&peer.device_id))
                    && own_id.as_str() < peer.device_id.as_str()
                    && let Some(ip) = peer.addrs.first()
                {
                    let sa = SocketAddr::new(*ip as IpAddr, peer.port);
                    status.set_peer_address(sa.to_string());
                    status.set_phase(if reconnecting {
                        ConnectionPhase::Reconnecting
                    } else {
                        ConnectionPhase::Connecting
                    });
                    tracing::info!(target: "mouseshare::app", name = %peer.name, addr = %sa, "发现对端，连接中");
                    let connection = tokio::select! {
                        connection = ep.connect(sa) => connection,
                        _ = wait_for_stop(running) => {
                            disco.shutdown();
                            return Ok(None);
                        },
                    };
                    break connection.map_err(|e| e.to_string())?;
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

async fn wait_for_stop(running: &AtomicBool) {
    while running.load(Ordering::Acquire) {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

/// 仅用于输入捕获启动之前的握手等待。已启动捕获后的会话必须走安全回退，不能取消
/// 整个 future，否则会跳过解除 grab / release_all / 停捕获的固定顺序。
async fn before_capture_or_stop<T>(
    future: impl std::future::Future<Output = T>,
    running: &AtomicBool,
) -> Option<T> {
    if !running.load(Ordering::Acquire) {
        return None;
    }
    tokio::select! {
        value = future => Some(value),
        _ = wait_for_stop(running) => None,
    }
}

/// 另一端可能仍在90秒SAS确认窗口，给出120秒握手上限；期间随时可暂停。
async fn handshake_step<T>(
    future: impl std::future::Future<Output = T>,
    running: &AtomicBool,
) -> Result<Option<T>, String> {
    tokio::time::timeout(
        std::time::Duration::from_secs(120),
        before_capture_or_stop(future, running),
    )
    .await
    .map_err(|_| "对端未完成连接准备，请确认另一端已核对配对码并启用互控。".into())
}

/// 信任判定（纯函数，可单测）：验收标准 6「未配对设备连接被拒绝」。
/// - 取不到对端指纹（mTLS 异常）→ 拒绝：无法固定身份就不能建会话；
/// - 对端指纹 = 本机指纹 → 放行：TLS 验签已证明对方持有本机私钥，即本机双实例
///   回环调试（--loopback / T-702 浸泡，两实例共享身份文件），无冒充可能；
/// - 信任列表为空（全新安装）→ 进入首次配对（GUI 必须核对两端一致）；
/// - 列表非空且指纹在列 → 放行；
/// - 列表非空且指纹不在列 → 拒绝（SPEC §5 AuthRejected）：已配对后陌生设备不可再
///   TOFU 混入。重新配对需先从设置界面解除本机保存的配对。
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

/// 信任列表：GUI 首次配对在人工确认之前不写入信任文件，也不进入输入会话。
/// CLI/headless 保留已有 TOFU 行为，已配对后的陌生指纹仍严格拒绝。
async fn check_trust(
    conn: &Conn,
    id: &Identity,
    trust_path: Option<&Path>,
    rejected_logged: &mut std::collections::HashSet<[u8; 32]>,
    running: &AtomicBool,
    status: &ConnStatus,
) -> Result<bool, String> {
    let mut trust: TrustStore = match trust_path.map(std::fs::read) {
        Some(Ok(bytes)) => serde_json::from_slice(&bytes)
            .map_err(|_| "配对记录损坏，已停止连接；请通过设备管理重新配对。".to_owned())?,
        Some(Err(error)) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err("无法读取配对记录，已停止连接。".into());
        }
        _ => TrustStore::new(),
    };
    let peer_fp = conn.peer_fingerprint();
    let mut decision = decide_trust(peer_fp.as_ref(), &id.fingerprint(), &trust);
    if decision == TrustDecision::Rejected
        && peer_fp.is_some()
        && status.pairing_confirmation_required()
        && status.adding_device()
    {
        decision = TrustDecision::FirstPair;
    }
    match decision {
        TrustDecision::Trusted => {
            if status.adding_device() {
                status.set_error("请在新电脑上启用配对；添加模式会跳过已保存设备。");
                return Ok(false);
            }
            tracing::info!(target: "mouseshare::app", "已信任的对端，自动连接（免核对配对码）");
            Ok(true)
        }
        TrustDecision::FirstPair => {
            // 等待期间 QUIC 使用既有 3s keepalive；旧端的 ScreenInfo 接收没有人工确认
            // 超时，不需改协议、消息编号或降低 TLS 证书校验要求。
            let sas = conn.compute_sas(&id.fingerprint());
            if status.pairing_confirmation_required() {
                let Some(sas) = sas else {
                    status.set_error("无法生成配对码，请重新连接后再试。");
                    return Err("TLS 配对码生成失败".to_owned());
                };
                status.begin_pairing(PairingRequest {
                    sas,
                    address: conn.remote_addr().to_string(),
                });
                let decision = wait_for_pairing(conn, running, status).await;
                status.clear_pairing();
                match decision {
                    PairingOutcome::Approved => {}
                    PairingOutcome::Stopped => return Ok(false),
                    PairingOutcome::Rejected => {
                        status.set_error(
                            "已取消本次配对。准备好后点击重新连接，再核对两台电脑的配对码。",
                        );
                        return Err("用户取消首次配对".to_owned());
                    }
                    PairingOutcome::TimedOut => {
                        status.set_error("配对确认已超时。请在两台电脑上重新连接，并核对配对码。");
                        return Err("首次配对确认超时".to_owned());
                    }
                    PairingOutcome::Disconnected => {
                        status.set_error(
                            "配对过程中对端已断开。请确认两端软件正在运行，然后重新连接。",
                        );
                        return Err("首次配对期间连接关闭".to_owned());
                    }
                }
            } else if let Some(sas) = sas {
                tracing::info!(target: "mouseshare::app", %sas, "新对端配对码（headless TOFU；请核对两端一致）");
            }
            if !running.load(Ordering::Acquire) {
                return Ok(false);
            }
            if let Some(fp) = peer_fp {
                trust.trust(&fp);
            }
            if let Some(p) = trust_path {
                if let Some(d) = p.parent() {
                    let _ = std::fs::create_dir_all(d);
                }
                if std::fs::write(p, trust.to_bytes()).is_err() {
                    status.set_error("无法保存配对信息，请检查软件配置目录的写入权限后重新连接。");
                    return Err("写入配对信任列表失败".to_owned());
                }
            }
            status.allow_add_device(false);
            Ok(true)
        }
        TrustDecision::Rejected => {
            let fp_hex =
                peer_fp.map(|fp| ms_core::topology::DeviceId::from_fingerprint(fp).to_string());
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
                    "拒绝未配对设备（验收标准 6）：本机已有信任对端，陌生设备不可混入；如需换机请先从设置解除配对"
                );
            } else {
                tracing::debug!(
                    target: "mouseshare::app",
                    reason = ?DisconnectReason::AuthRejected,
                    peer = %conn.remote_addr(),
                    "再次拒绝同一未配对设备"
                );
            }
            Ok(false)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PairingOutcome {
    Approved,
    Rejected,
    Stopped,
    TimedOut,
    Disconnected,
}

async fn wait_for_pairing(
    conn: &Conn,
    running: &AtomicBool,
    status: &ConnStatus,
) -> PairingOutcome {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(90);
    wait_for_pairing_until(conn, running, status, deadline).await
}

async fn wait_for_pairing_until(
    conn: &Conn,
    running: &AtomicBool,
    status: &ConnStatus,
    deadline: tokio::time::Instant,
) -> PairingOutcome {
    loop {
        if !running.load(Ordering::Acquire) {
            return PairingOutcome::Stopped;
        }
        if let Some(approved) = status.pairing_decision() {
            return if approved {
                PairingOutcome::Approved
            } else {
                PairingOutcome::Rejected
            };
        }
        tokio::select! {
            _ = conn.wait_closed() => return PairingOutcome::Disconnected,
            _ = tokio::time::sleep_until(deadline) => return PairingOutcome::TimedOut,
            _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {},
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
            && session_safety::valid_screen(rect)
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
    collision_priority: bool,
) -> Result<(), String> {
    if !session_safety::valid_screen(Rect {
        x: 0.0,
        y: 0.0,
        w: args.screen.0,
        h: args.screen.1,
    }) || !session_safety::valid_screen(Rect {
        x: 0.0,
        y: 0.0,
        w: args.peer_screen.0,
        h: args.peer_screen.1,
    }) {
        return Err("屏幕尺寸无效，请使用有效的物理像素宽高。".into());
    }
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
    let Some(opened) = handshake_step(conn.open_msg_stream(), running).await? else {
        return Ok(());
    };
    let mut send_stream = opened.map_err(|e| e.to_string())?;

    // 交换屏幕尺寸（FR-203：用对端真实尺寸按比例映射）。先发以解开 accept 互等。
    let screen_message = Msg::from(ms_proto::ScreenInfo {
        virtual_rect: ms_proto::Rect {
            x: own.x,
            y: own.y,
            w: own.w,
            h: own.h,
        },
        scale: 1.0,
    });
    let Some(sent) =
        handshake_step(send_bounded(&mut send_stream, &screen_message), running).await?
    else {
        return Ok(());
    };
    sent?;

    let peer_fallback = Rect {
        x: 0.0,
        y: 0.0,
        w: args.peer_screen.0,
        h: args.peer_screen.1,
    };
    let Some(recv_stream) = handshake_step(conn.accept_msg_stream(), running).await? else {
        return Ok(());
    };
    let mut recv_stream = recv_stream.map_err(|e| e.to_string())?;
    let Some(first_message) = handshake_step(recv_stream.recv(), running).await? else {
        return Ok(());
    };
    let peer = match first_message.map_err(|e| e.to_string())? {
        Some(Msg::ScreenInfo(si))
            if session_safety::valid_screen(Rect {
                x: si.virtual_rect.x,
                y: si.virtual_rect.y,
                w: si.virtual_rect.w,
                h: si.virtual_rect.h,
            }) =>
        {
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
    engine.set_collision_priority(collision_priority);
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

    let receiver_handle = if !args.no_clipboard {
        match clip::spawn_receiver(conn.clone(), platform, clip_state.clone(), running.clone()) {
            Ok(handle) => Some(handle),
            Err(error) => {
                tracing::warn!(target: "mouseshare::app", %error, "剪贴板接收不可用");
                None
            }
        }
    } else {
        None
    };

    tracing::info!(target: "mouseshare::app", side = %args.side, "MouseShare 运行中：鼠标移到该侧边缘即可穿越到对端");
    // 连接质量：标记已连，初始记一次 RTT（供托盘显示，FR-606）
    status.set_connected(true);
    status.set_control_state(engine.state());
    status.set_rtt_ms(conn.rtt_ms());

    let mut hb = tokio::time::interval(std::time::Duration::from_secs(1));
    hb.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut seq = 0u32;
    let mut heartbeat = HeartbeatTracker::new();
    let mut control_poll = tokio::time::interval(std::time::Duration::from_millis(50));
    let mut previous_drops = 0;
    let mut screen_tick = 0u32;
    #[cfg(target_os = "macos")]
    let power_state = Arc::new(std::sync::atomic::AtomicU8::new(0));
    #[cfg(target_os = "macos")]
    let _power_monitor = if platform.name() == "macos" {
        let sleeping = power_state.clone();
        let waking = power_state.clone();
        Some(ms_platform_macos::power_monitor::PowerMonitor::start(
            move || sleeping.store(1, Ordering::Release),
            move || waking.store(2, Ordering::Release),
        ))
    } else {
        None
    };
    let mut key_stats = KeyLinkStats::default();
    let mut key_stats_logged = (KeyLinkStats::default(), 0u64);

    let session_result: Result<(), String> = async {
    while running.load(Ordering::Acquire) {
        tokio::select! {
            _ = control_poll.tick() => {
                if !running.load(Ordering::Acquire) { break; }
                capture.check_health().map_err(|e| format!("捕获已停止（TapDisabledLoop/HookLost）：{e}"))?;
                if let Some(locked) = status.take_lock_request() {
                    let out = engine.set_local_lock(locked);
                    apply(out, &mut capture, &mut inject, &mut send_stream, &mut key_stats).await?;
                    status.set_local_locked(engine.local_locked());
                    status.set_control_state(engine.state());
                }
                let drops = cap_dropped.load(Ordering::Relaxed);
                if drops != previous_drops {
                    return Err("输入队列拥塞，已释放按键并重建连接，避免遗漏抬键。".into());
                }
                previous_drops = drops;
                #[cfg(target_os = "macos")]
                if power_state.load(Ordering::Acquire) != 0 {
                    let _ = tokio::time::timeout(std::time::Duration::from_millis(100), send_stream.send(&Msg::from(ms_proto::Suspend {}))).await;
                    tracing::info!(target: "mouseshare::app", reason = ?DisconnectReason::PeerSuspend, "本机睡眠或唤醒，释放输入并重建会话");
                    break;
                }
            }
            _ = conn.wait_closed() => {
                tracing::warn!(target: "mouseshare::app", reason = ?DisconnectReason::QuicError, "连接断开");
                status.inc_disconnect();
                break;
            }
            _ = hb.tick() => {
                if !platform.has_input_permission() {
                    status.set_phase(ConnectionPhase::PermissionRequired);
                    tracing::warn!(target: "mouseshare::app", reason = ?DisconnectReason::PermissionRevoked, "输入权限已撤销");
                    return Err("辅助功能权限已撤销，已安全释放输入；请重新授权后连接。".into());
                }
                if heartbeat.tick() {
                    tracing::warn!(target: "mouseshare::app", reason = ?DisconnectReason::HeartbeatTimeout, "连续三次无心跳响应");
                    return Err("对端未响应心跳，已释放输入并准备重连。".into());
                }
                seq = seq.wrapping_add(1);
                send_bounded(&mut send_stream, &Msg::from(Ping { seq, t0: 0 })).await?;
                screen_tick += 1;
                if screen_tick % 2 == 0
                    && let Ok(current) = platform.screen_info().and_then(|mut s| s.virtual_screen())
                    && session_safety::valid_screen(current.rect) && current.rect != own
                {
                    return Err("显示器排列或分辨率已变化，正在重新读取屏幕并连接。".into());
                }
                status.set_rtt_ms(conn.rtt_ms());
                // 设置即时生效：每秒重读共享配置，引擎相关项有变化则热应用（无需重启互控）。
                let cur = LiveSettings::from_config(config);
                if cur != live {
                    // 布局/主控角色变化先安全返回，再热应用，避免旧影子坐标继续注入。
                    if cur.side != live.side || cur.display_offset != live.display_offset || cur.no_takeover != live.no_takeover {
                        let out = engine.set_local_lock(true);
                        apply(out, &mut capture, &mut inject, &mut send_stream, &mut key_stats).await?;
                        engine.set_local_lock(status.local_locked());
                    }
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
                apply(out, &mut capture, &mut inject, &mut send_stream, &mut key_stats).await?;
                status.set_local_locked(engine.local_locked());
                if prev != now {
                    status.set_control_state(now);
                }
                clip::on_transition(prev, now, conn, clip_read.as_mut(), &clip_state, file_transfer);
            }
            r = recv_stream.recv() => {
                match r {
                    Ok(Some(Msg::Ping(p))) => {
                        send_bounded(&mut send_stream, &Msg::from(Pong { seq: p.seq, t0: p.t0 })).await?;
                    }
                    Ok(Some(Msg::Pong(_))) => heartbeat.on_pong(),
                    Ok(Some(Msg::Suspend(_))) => {
                        tracing::info!(target: "mouseshare::app", reason = ?DisconnectReason::PeerSuspend, "对端睡眠");
                        break;
                    }
                    Ok(Some(Msg::ScreenInfo(_))) => return Err("对端屏幕发生变化，正在重连。".into()),
                    Ok(Some(msg)) => {
                        if matches!(msg, Msg::KeyEvent(_)) {
                            key_stats.recv += 1;
                        }
                        let prev = engine.state();
                        let out = engine.feed_peer(msg);
                        let now = engine.state();
                        apply(out, &mut capture, &mut inject, &mut send_stream, &mut key_stats).await?;
                status.set_local_locked(engine.local_locked());
                        if prev != now {
                            status.set_control_state(now);
                        }
                        clip::on_transition(prev, now, conn, clip_read.as_mut(), &clip_state, file_transfer);
                    }
                    _ => {
                        tracing::warn!(target: "mouseshare::app", reason = ?DisconnectReason::PeerShutdown, "接收流结束");
                        status.inc_disconnect();
                        break;
                    }
                }
            }
        }
    }

    Ok(())
    }.await;

    // 所有退出（包括注入/网络错误）都经过同一无网络等待的安全事务。
    let _ = engine.on_disconnect();
    let cleanup_errors = session_safety::cleanup(capture.as_mut(), inject.as_mut());
    for error in cleanup_errors {
        tracing::warn!(target: "mouseshare::app", %error, "输入清理失败，继续释放其余状态");
    }
    conn.close();
    if let Some(mut handle) = receiver_handle {
        if tokio::time::timeout(std::time::Duration::from_secs(1), &mut handle)
            .await
            .is_err()
        {
            handle.abort();
            let _ = handle.await;
        }
    }
    status.set_connected(false);
    tracing::info!(target: "mouseshare::app", "会话已按安全顺序清理");
    session_result
}

async fn send_bounded(stream: &mut MsgSendStream, msg: &Msg) -> Result<(), String> {
    tokio::time::timeout(std::time::Duration::from_millis(500), stream.send(msg))
        .await
        .map_err(|_| "网络发送超时，已停止本轮控制。".to_owned())?
        .map_err(|e| format!("网络发送失败：{e}"))
}

/// 本机动作先执行，再进行有界网络发送；本机恢复不等待已死连接。
async fn apply(
    out: EngineOut,
    capture: &mut Box<dyn InputCapture>,
    inject: &mut Box<dyn InputInject>,
    send_stream: &mut MsgSendStream,
    key_stats: &mut KeyLinkStats,
) -> Result<(), String> {
    key_stats.inj += session_safety::apply_platform(&out, capture.as_mut(), inject.as_mut())?;
    for msg in out.to_peer {
        send_bounded(send_stream, &msg).await?;
        if matches!(msg, Msg::KeyEvent(_)) {
            key_stats.fwd += 1;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn scratch_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/ui-status-tests")
            .join(uuid::Uuid::new_v4().to_string())
    }

    #[test]
    fn 解除配对仅清信任文件且重复操作成功() {
        let dir = scratch_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let trust = dir.join("trust.json");
        let cert = dir.join("cert.der");
        let key = dir.join("key.der");
        std::fs::write(&trust, b"paired").unwrap();
        std::fs::write(&cert, b"certificate").unwrap();
        std::fs::write(&key, b"private key").unwrap();
        assert_eq!(clear_trust_file(&trust), Ok(()));
        assert!(!trust.exists());
        assert_eq!(std::fs::read(&cert).unwrap(), b"certificate");
        assert_eq!(std::fs::read(&key).unwrap(), b"private key");
        assert_eq!(clear_trust_file(&trust), Ok(()));
        assert!(clear_trust_file(&dir).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    async fn loopback_pair() -> (Endpoint, Endpoint, Conn, Conn, Identity, Identity) {
        let own = Identity::generate().unwrap();
        let peer = Identity::generate().unwrap();
        let own_ep = Endpoint::new("127.0.0.1:0".parse().unwrap(), &own).unwrap();
        let peer_ep = Endpoint::new("127.0.0.1:0".parse().unwrap(), &peer).unwrap();
        let (connection, accepted) = tokio::join!(
            own_ep.connect(peer_ep.local_addr().unwrap()),
            peer_ep.accept(),
        );
        (
            own_ep,
            peer_ep,
            connection.unwrap(),
            accepted.unwrap().unwrap(),
            own,
            peer,
        )
    }

    #[tokio::test]
    async fn 图形界面首次配对须确认且拒绝不保存信任() {
        for approved in [false, true] {
            let (_own_ep, _peer_ep, connection, accepted, own, peer) = loopback_pair().await;
            let expected_sas = accepted.compute_sas(&peer.fingerprint()).unwrap();
            let dir = scratch_dir();
            std::fs::create_dir_all(&dir).unwrap();
            let trust_path = dir.join("trust.json");
            let status = Arc::new(ConnStatus::new());
            status.require_pairing_confirmation(true);
            let running = Arc::new(AtomicBool::new(true));
            let check = tokio::spawn({
                let status = Arc::clone(&status);
                let running = Arc::clone(&running);
                let trust_path = trust_path.clone();
                async move {
                    check_trust(
                        &connection,
                        &own,
                        Some(&trust_path),
                        &mut std::collections::HashSet::new(),
                        &running,
                        &status,
                    )
                    .await
                }
            });
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while status.pending_pairing().is_none() {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            assert_eq!(status.phase(), ConnectionPhase::Pairing);
            assert_eq!(status.pending_pairing().unwrap().sas, expected_sas);
            assert!(!trust_path.exists(), "尚未确认时不可保存对端身份");
            assert!(!status.connected(), "尚未确认时不可显示为键鼠已就绪");
            assert!(!check.is_finished(), "尚未确认时不可进入互控会话");

            status.decide_pairing(approved);
            let result = tokio::time::timeout(std::time::Duration::from_secs(2), check)
                .await
                .unwrap()
                .unwrap();
            assert!(status.pending_pairing().is_none());
            if approved {
                assert_eq!(result, Ok(true));
                assert!(
                    TrustStore::from_bytes(&std::fs::read(&trust_path).unwrap())
                        .is_trusted(&peer.fingerprint())
                );
            } else {
                assert!(result.is_err());
                assert!(!trust_path.exists(), "拒绝后不可保留隐式信任");
                assert!(status.last_error().unwrap().contains("已取消本次配对"));
            }
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[tokio::test]
    async fn 配对等待响应停止与超时() {
        let (_own_ep, _peer_ep, connection, _accepted, _own, _peer) = loopback_pair().await;
        let status = ConnStatus::new();
        let running = AtomicBool::new(false);
        assert_eq!(
            wait_for_pairing(&connection, &running, &status).await,
            PairingOutcome::Stopped
        );
        running.store(true, Ordering::Release);
        assert_eq!(
            wait_for_pairing_until(&connection, &running, &status, tokio::time::Instant::now(),)
                .await,
            PairingOutcome::TimedOut
        );
    }

    #[tokio::test]
    async fn 已有信任只能经显式添加与确认接纳新设备_损坏记录不降级为空() {
        let (_a, _b, connection, _accepted, own, peer) = loopback_pair().await;
        let dir = scratch_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("trust.json");
        let mut trust = TrustStore::new();
        trust.trust(&[42; 32]);
        std::fs::write(&path, trust.to_bytes()).unwrap();
        let status = ConnStatus::new();
        status.require_pairing_confirmation(true);
        let running = AtomicBool::new(true);
        let mut rejected = std::collections::HashSet::new();
        assert!(
            !check_trust(
                &connection,
                &own,
                Some(&path),
                &mut rejected,
                &running,
                &status
            )
            .await
            .unwrap()
        );
        assert!(status.pending_pairing().is_none());
        status.allow_add_device(true);
        let approve = async {
            while status.pending_pairing().is_none() {
                tokio::task::yield_now().await;
            }
            status.decide_pairing(true);
        };
        let (result, _) = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            tokio::join!(
                check_trust(
                    &connection,
                    &own,
                    Some(&path),
                    &mut rejected,
                    &running,
                    &status
                ),
                approve
            )
        })
        .await
        .unwrap();
        assert!(result.unwrap());
        let saved = TrustStore::from_bytes(&std::fs::read(&path).unwrap());
        assert!(saved.is_trusted(&[42; 32]) && saved.is_trusted(&peer.fingerprint()));
        assert!(!status.adding_device());
        std::fs::write(&path, b"broken-json").unwrap();
        assert!(
            check_trust(
                &connection,
                &own,
                Some(&path),
                &mut rejected,
                &running,
                &status
            )
            .await
            .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"broken-json");
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn stop_soon(running: Arc<AtomicBool>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            running.store(false, Ordering::Release);
        })
    }

    #[tokio::test]
    async fn 停止能取消尚未收到控制流的握手() {
        let (_own_ep, _peer_ep, connection, _accepted, _own, _peer) = loopback_pair().await;
        // 对端只建立 TLS、尚未发出控制流，模拟另一端还在等待人工核对配对码。
        let running = Arc::new(AtomicBool::new(true));
        let stop = stop_soon(Arc::clone(&running));
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            before_capture_or_stop(connection.accept_msg_stream(), &running),
        )
        .await
        .unwrap();
        assert!(result.is_none(), "停止后不能继续等人工配对或占用监听端口");
        stop.await.unwrap();
    }

    #[tokio::test]
    async fn 停止能取消控制流中尚未收到消息的握手() {
        let (_own_ep, _peer_ep, connection, accepted, _own, _peer) = loopback_pair().await;
        let mut sender = accepted.open_msg_stream().await.unwrap();
        sender
            .send(&Msg::from(Ping { seq: 1, t0: 0 }))
            .await
            .unwrap();
        let mut receiver = connection.accept_msg_stream().await.unwrap();
        assert!(matches!(receiver.recv().await.unwrap(), Some(Msg::Ping(_))));
        // QUIC 控制流存在但下一条帧迟迟未到，覆盖 recv() 阶段的停止行为。
        let running = Arc::new(AtomicBool::new(true));
        let stop = stop_soon(Arc::clone(&running));
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            before_capture_or_stop(receiver.recv(), &running),
        )
        .await
        .unwrap();
        assert!(result.is_none(), "停止后不能继续等待 ScreenInfo");
        stop.await.unwrap();
    }

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
    fn mac_端旧配置保持从属_显式开启后允许主控() {
        // T-608 复现：UI 已按「Windows=控制端、mac=被控端」裁剪（T-606），但引擎若仍按
        // 配置允许 mac 发起穿越，残留的 side（用户实测 "bottom"）会把 mac 屏幕对应边
        // 变成 100px 蓄力穿越边——挡住自动隐藏的程序坞，推够还会误穿越去控制 Windows。
        let s = LiveSettings::from_config(&cfg("bottom", false, false, true));
        if cfg!(target_os = "macos") {
            assert!(s.no_takeover, "旧Mac配置仍默认从属");
            let c = cfg("bottom", false, false, true);
            c.lock().unwrap().mac_control = true;
            assert!(
                !LiveSettings::from_config(&c).no_takeover,
                "只有明确开启Mac主控才发起跨屏"
            );
            c.lock().unwrap().no_takeover = true;
            assert!(
                LiveSettings::from_config(&c).no_takeover,
                "只接收模式仍优先"
            );
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

#[cfg(test)]
#[path = "runtime_tests.rs"]
mod runtime_tests;
