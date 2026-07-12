//! 剪贴板同步接入守护进程（T-405，FR-401~405，DEVELOPMENT.md §5.2）。
//!
//! 触发时机 = 控制权切换（FR-402）：本机由「直接使用」转为「控制对端」或「被对端
//! 释放」时，读本机剪贴板，若有新内容（changeCount 变化、未机密、未超限）则压缩分块
//! 经**独立 QUIC 流**发往对端（与键鼠控制流隔离，避免大图卡顿鼠标，DEVELOPMENT §4.1）。
//! 对端在后台接收任务里重组并写入系统剪贴板。

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use ms_core::clip_sync::{self, ClipSyncState, Reassembler};
use ms_core::control::ControlState;
use ms_net::Conn;
use ms_net::quic::MsgRecvStream;
use ms_platform::{ClipboardAccess, Platform};
use ms_proto::{ClipboardData, Msg};

/// 共享的剪贴板同步去重状态：主循环（读+发）与接收任务（写入后标记）都要更新，
/// 以 changeCount 去重并抑制内容在两端间来回回弹（echo）。
pub type SharedClipState = Arc<Mutex<ClipSyncState>>;

/// 新建共享去重状态。`enabled=false`（`--no-clipboard`）时 `should_sync` 恒返回 false。
pub fn shared_state(enabled: bool) -> SharedClipState {
    Arc::new(Mutex::new(ClipSyncState::new(enabled)))
}

fn lock(state: &SharedClipState) -> std::sync::MutexGuard<'_, ClipSyncState> {
    // 锁仅在切换/接收时短暂持有，不跨 await；中毒亦取回内层数据继续（不 panic）。
    state.lock().unwrap_or_else(|e| e.into_inner())
}

/// 该控制态跃迁是否应推送本机剪贴板（FR-402：控制权切换到目标设备时同步源端剪贴板）。
/// - `Local→Remote`：本机开始控制对端 → 推本机剪贴板，供对端粘贴。
/// - `Controlled→其它`：对端结束对本机的控制（期间复制发生在本机）→ 推本机剪贴板。
fn should_push(prev: ControlState, now: ControlState) -> bool {
    (prev == ControlState::Local && now == ControlState::Remote)
        || (prev == ControlState::Controlled && now != ControlState::Controlled)
}

/// 主循环每次喂事件后调用：若发生触发同步的控制态跃迁，则读本机剪贴板并经独立流发往对端。
/// `clip` 为主循环独占的剪贴板读句柄（`None` 表示平台剪贴板不可用，跳过）。
/// `file_transfer` 为真（专业版解锁）时，剪贴板若含文件引用则改走粘贴式文件传输（FR-501/502）。
pub fn on_transition(
    prev: ControlState,
    now: ControlState,
    conn: &Conn,
    clip: Option<&mut Box<dyn ClipboardAccess>>,
    state: &SharedClipState,
    file_transfer: bool,
) {
    if !should_push(prev, now) {
        return;
    }
    let Some(clip) = clip else { return };

    // 粘贴式文件传输（FR-501/502）：剪贴板含文件引用 → 走文件流，不当作剪贴板内容。
    // 与内容同享 changeCount 去重，避免每次切换重复发送同一批文件。
    if file_transfer {
        let files = match clip.read_files() {
            Ok(f) => f,
            Err(e) => {
                tracing::warn!(target: "mouseshare::clip", %e, "读剪贴板文件引用失败");
                Vec::new()
            }
        };
        // 诊断（排查单向文件传输不通）：每次切换记录读到的文件数
        tracing::info!(
            target: "mouseshare::clip",
            files = files.len(),
            "控制权切换：读到剪贴板文件引用数"
        );
        if !files.is_empty() {
            if let Ok(cc) = clip.change_count()
                && !lock(state).should_sync(cc)
            {
                return; // 这批文件已发过
            }
            crate::xfer::spawn_send(conn, files);
            return;
        }
    }

    push_clipboard(conn, clip.as_mut(), state);
}

/// 读本机剪贴板，去重通过后压缩分块、起一个后台任务经独立流发送（不阻塞主循环）。
fn push_clipboard(conn: &Conn, clip: &mut dyn ClipboardAccess, state: &SharedClipState) {
    let Ok(cc) = clip.change_count() else {
        return;
    };
    {
        // 开关关闭 / 自上次同步无变化 → 不发（FR-402/404）
        if !lock(state).should_sync(cc) {
            return;
        }
    }
    // 机密内容（org.nspasteboard.ConcealedType 等）read 返回 None，跳过（FR-405）
    let content = match clip.read() {
        Ok(Some(c)) => c,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!(target: "mouseshare::clip", %e, "读剪贴板失败，跳过同步");
            return;
        }
    };
    // 诊断（排查「文件传输后文本不同步」）：记录将要发送的内容类型与大小
    let (kind, bytes) = match &content {
        ms_platform::ClipboardContent::Text(s) => ("text", s.len()),
        ms_platform::ClipboardContent::Html(s) => ("html", s.len()),
        ms_platform::ClipboardContent::Png(b) => ("png", b.len()),
    };
    tracing::info!(target: "mouseshare::clip", kind, bytes, "切换：准备发送剪贴板内容");
    let chunks = match clip_sync::encode(&content) {
        Ok(c) => c,
        Err(e) => {
            // 超限（FR-403）或编码错误：不同步
            tracing::warn!(target: "mouseshare::clip", %e, "剪贴板编码失败/超限，跳过同步");
            return;
        }
    };
    // 通过编码校验（未超限/非机密）才入历史（T-808）
    crate::cliphist::record(&content, false);
    let conn = conn.clone();
    tokio::spawn(async move {
        send_chunks(&conn, chunks).await;
    });
}

async fn send_chunks(conn: &Conn, chunks: Vec<ClipboardData>) {
    let mut stream = match conn.open_msg_stream().await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(target: "mouseshare::clip", %e, "打开剪贴板流失败");
            return;
        }
    };
    for c in chunks {
        if let Err(e) = stream.send(&Msg::from(c)).await {
            tracing::warn!(target: "mouseshare::clip", %e, "发送剪贴板分块失败");
            return;
        }
    }
    let _ = stream.finish().await;
    tracing::info!(target: "mouseshare::clip", "剪贴板内容已发往对端");
}

/// 接收端剪贴板句柄：跨并发流任务共享，写入时短暂加锁（同 SharedClipState 的用法）。
type ClipHandle = Arc<Mutex<Box<dyn ClipboardAccess>>>;

fn lock_clip(clip: &ClipHandle) -> std::sync::MutexGuard<'_, Box<dyn ClipboardAccess>> {
    clip.lock().unwrap_or_else(|e| e.into_inner())
}

/// 同一次复制的多个文件按「批」聚合写回剪贴板（T-807）。
/// 发送端一次复制 N 个文件会同时开 N 条流，各 FileOffer 到达间隔极小；
/// 距上一条 offer 超过该窗口即视为新的复制操作、重开一批。
const FILE_BATCH_WINDOW: Duration = Duration::from_secs(2);

/// 本批已落地的文件路径 + 最近一次 offer 到达时刻。
/// 已知限制：多个 >1GiB 大文件各自等用户确认（FR-504）时 offer 可能相隔超窗，
/// 后确认的文件会另起一批（剪贴板只剩后一批）——比修复前「永远只剩一个」仍是改善。
struct FileBatch {
    paths: Vec<PathBuf>,
    last_offer: Option<Instant>,
}

fn lock_batch(b: &Mutex<FileBatch>) -> std::sync::MutexGuard<'_, FileBatch> {
    b.lock().unwrap_or_else(|e| e.into_inner())
}

/// 启动后台接收分发任务：循环接受独立流，**每条流派发到独立并发任务**处理
/// （不在主循环里顺序 await 完再收下一条）。
///
/// 原因：一条流处理完才会再 `accept_msg_stream()`，若顺序处理，大文件/弱网下
/// 迟迟收不完的文件流会**头部阻塞**之后到达的所有流（含剪贴板文本）——用户回报
/// 「复制文件后复制文本必现问题、很多情况下都无法粘贴」正是此因：文件流卡住后
/// 全部后续同步都排在它后面永远等不到。改为并发派发后，各流独立收取，仅在真正
/// 写入本机剪贴板的**极短**临界区里加锁（`ClipHandle`），不阻塞其它流的网络 I/O。
///
/// 必须在主循环建立完控制流（已 accept 一次）之后调用，避免抢走控制流。
pub fn spawn_receiver<P: Platform>(
    conn: Conn,
    platform: &P,
    state: SharedClipState,
    running: Arc<AtomicBool>,
) -> Result<tokio::task::JoinHandle<()>, String> {
    let clip: ClipHandle = Arc::new(Mutex::new(platform.clipboard().map_err(|e| e.to_string())?));
    let batch = Arc::new(Mutex::new(FileBatch {
        paths: Vec::new(),
        last_offer: None,
    }));
    Ok(tokio::spawn(async move {
        while running.load(Ordering::Acquire) {
            let mut stream = match conn.accept_msg_stream().await {
                Ok(s) => s,
                Err(_) => break, // 连接关闭
            };
            let clip = clip.clone();
            let state = state.clone();
            let batch = batch.clone();
            tokio::spawn(async move {
                match stream.recv().await {
                    Ok(Some(Msg::ClipboardData(first))) => {
                        // 诊断（排查「文件传输后文本不同步」）：收到剪贴板流首条
                        tracing::info!(
                            target: "mouseshare::clip",
                            mime = %first.mime,
                            "接收到独立流：剪贴板内容（开始重组写入）"
                        );
                        receive_clipboard(&mut stream, first, &clip, &state).await;
                        // 剪贴板已被非文件内容占据：清空文件批次，防止之后落地的
                        // 文件把已过期的旧批路径一并写回（T-807）。
                        lock_batch(&batch).paths.clear();
                    }
                    Ok(Some(Msg::FileOffer(offer))) => {
                        // 归批按 offer 到达时刻判定（须在接收前，落地耗时不影响归批）：
                        // 同一次复制的多个文件由发送端同时开流，offer 几乎同时到达；
                        // 距上一条超过窗口即视为新的复制操作、重开一批（T-807）。
                        {
                            let mut b = lock_batch(&batch);
                            if b.last_offer.is_none_or(|t| t.elapsed() > FILE_BATCH_WINDOW) {
                                b.paths.clear();
                            }
                            b.last_offer = Some(Instant::now());
                        }
                        if let Some(path) = crate::xfer::receive(&mut stream, offer).await {
                            // 累积本批全部已落地路径**整批**写回剪贴板，使一次复制的
                            // 多个文件都可粘贴——修复前各流只写自己的路径，互相覆盖
                            // 只剩最后完成的一个（T-807/T-504 已知限制）。
                            let all = {
                                let mut b = lock_batch(&batch);
                                b.paths.push(path);
                                b.paths.clone()
                            };
                            let mut c = lock_clip(&clip);
                            if c.write_files(&all).is_ok()
                                && let Ok(cc) = c.change_count()
                            {
                                // 写入抬升本地序号，标记已同步避免把刚收到的文件回弹给对端。
                                lock(&state).note_synced(cc);
                            }
                        }
                    }
                    Ok(Some(_other)) => {} // 其它非预期消息：丢弃该流
                    _ => {}
                }
            });
        }
    }))
}

/// 在已接受的流上重组一份完整剪贴板内容并写入本机系统剪贴板。
async fn receive_clipboard(
    stream: &mut MsgRecvStream,
    first: ClipboardData,
    clip: &ClipHandle,
    state: &SharedClipState,
) {
    let mut re = Reassembler::new();
    let mut done = match re.push(first) {
        Ok(d) => d,
        Err(_) => return,
    };
    while done.is_none() {
        match stream.recv().await {
            Ok(Some(Msg::ClipboardData(d))) => {
                done = match re.push(d) {
                    Ok(x) => x,
                    Err(e) => {
                        tracing::warn!(target: "mouseshare::clip", %e, "剪贴板重组失败");
                        return;
                    }
                };
            }
            _ => return, // 流提前结束/出错：丢弃这次同步
        }
    }
    let Some(content) = done else { return };
    let mut c = lock_clip(clip);
    if let Err(e) = c.write(&content) {
        tracing::warn!(target: "mouseshare::clip", %e, "写本机剪贴板失败");
        return;
    }
    // 写入抬升了本地 changeCount；标记为已同步，避免该内容被当作本地新内容回弹给对端。
    if let Ok(cc) = c.change_count() {
        lock(state).note_synced(cc);
    }
    drop(c);
    crate::cliphist::record(&content, true); // 对端同步来的入历史（T-808）
    tracing::info!(target: "mouseshare::clip", "已写入对端剪贴板内容");
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use ms_platform::ClipboardContent;

    /// 内存假剪贴板：写入即记录并抬升 changeCount，供端到端回环测试。
    #[derive(Default)]
    struct MockClip {
        content: Option<ClipboardContent>,
        cc: u64,
    }
    impl ClipboardAccess for MockClip {
        fn read(&mut self) -> ms_platform::Result<Option<ClipboardContent>> {
            Ok(self.content.clone())
        }
        fn write(&mut self, c: &ClipboardContent) -> ms_platform::Result<()> {
            self.content = Some(c.clone());
            self.cc += 1;
            Ok(())
        }
        fn change_count(&mut self) -> ms_platform::Result<u64> {
            Ok(self.cc)
        }
    }

    async fn connect_pair() -> (Conn, Conn) {
        use std::net::{Ipv4Addr, SocketAddr};
        let loopback: SocketAddr = (Ipv4Addr::LOCALHOST, 0).into();
        let server_id = ms_net::Identity::generate().unwrap();
        let client_id = ms_net::Identity::generate().unwrap();
        let server = ms_net::Endpoint::new(loopback, &server_id).unwrap();
        let server_addr = server.local_addr().unwrap();
        let client = ms_net::Endpoint::new(loopback, &client_id).unwrap();
        let accept = tokio::spawn(async move { server.accept().await.unwrap().unwrap() });
        let client_conn = client.connect(server_addr).await.unwrap();
        let server_conn = accept.await.unwrap();
        std::mem::forget(client); // endpoint 须在连接存活期间保留
        (client_conn, server_conn)
    }

    /// T-405 验证：A 端编码→独立流发送，B 端分发→重组→写入，内容一致。
    #[test]
    fn 回环_剪贴板内容端到端一致() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let (a, b) = connect_pair().await;
            let content = ClipboardContent::Text("跨机剪贴板 hello 你好".into());
            let chunks = clip_sync::encode(&content).unwrap();

            let sender = {
                let a = a.clone();
                tokio::spawn(async move { send_chunks(&a, chunks).await })
            };

            // 复刻 spawn_receiver 的分发：接受流→读首条→重组写入
            let mut stream = b.accept_msg_stream().await.unwrap();
            let first = match stream.recv().await.unwrap().unwrap() {
                Msg::ClipboardData(d) => d,
                other => panic!("首条应为 ClipboardData，得到 {other:?}"),
            };
            let mock: ClipHandle = Arc::new(Mutex::new(Box::new(MockClip::default())));
            let state = shared_state(true);
            receive_clipboard(&mut stream, first, &mock, &state).await;
            sender.await.unwrap();

            let mut c = lock_clip(&mock);
            assert_eq!(c.read().unwrap(), Some(content), "B 端剪贴板应与 A 端一致");
            let cc = c.change_count().unwrap();
            drop(c);
            // 写入后 note_synced 已记录，B 自身切换不应把同内容回弹
            assert!(!lock(&state).should_sync(cc), "刚写入的内容不应回弹");
        });
    }

    #[test]
    fn 仅在切换到目标设备时推送() {
        use ControlState::{Controlled, Local, Remote};
        // 本机开始控制对端
        assert!(should_push(Local, Remote));
        // 对端释放本机
        assert!(should_push(Controlled, Local));
        // 维持原态不推
        assert!(!should_push(Remote, Remote));
        assert!(!should_push(Local, Local));
        assert!(!should_push(Controlled, Controlled));
        // 对端开始控制本机：源端是对端，由对端推（本机不推）
        assert!(!should_push(Local, Controlled));
        // 本机结束控制对端回到本地：源端是对端，本机不推（避免双推）
        assert!(!should_push(Remote, Local));
    }

    // ---- 复现：用户报「复制文件后文本无法复制」----

    use std::path::PathBuf;

    /// 内存剪贴板存储：镜像真实剪贴板「写文本清文件 / 写文件清文本」语义，每写一次抬 cc。
    #[derive(Default)]
    struct ClipStore {
        content: Option<ClipboardContent>,
        files: Vec<PathBuf>,
        cc: u64,
    }

    #[derive(Clone)]
    struct SharedClip(Arc<Mutex<ClipStore>>);

    impl ClipboardAccess for SharedClip {
        fn read(&mut self) -> ms_platform::Result<Option<ClipboardContent>> {
            Ok(self.0.lock().unwrap().content.clone())
        }
        fn write(&mut self, c: &ClipboardContent) -> ms_platform::Result<()> {
            let mut s = self.0.lock().unwrap();
            s.content = Some(c.clone());
            s.files.clear();
            s.cc += 1;
            Ok(())
        }
        fn change_count(&mut self) -> ms_platform::Result<u64> {
            Ok(self.0.lock().unwrap().cc)
        }
        fn read_files(&mut self) -> ms_platform::Result<Vec<PathBuf>> {
            Ok(self.0.lock().unwrap().files.clone())
        }
        fn write_files(&mut self, paths: &[PathBuf]) -> ms_platform::Result<()> {
            let mut s = self.0.lock().unwrap();
            s.files = paths.to_vec();
            s.content = None;
            s.cc += 1;
            Ok(())
        }
    }

    struct MockPlatform(SharedClip);
    impl ms_platform::Platform for MockPlatform {
        fn name(&self) -> &'static str {
            "mock"
        }
        fn input_capture(&self) -> ms_platform::Result<Box<dyn ms_platform::InputCapture>> {
            Err(ms_platform::PlatformError::Unimplemented("x"))
        }
        fn input_inject(&self) -> ms_platform::Result<Box<dyn ms_platform::InputInject>> {
            Err(ms_platform::PlatformError::Unimplemented("x"))
        }
        fn clipboard(&self) -> ms_platform::Result<Box<dyn ClipboardAccess>> {
            Ok(Box::new(self.0.clone()))
        }
        fn screen_info(&self) -> ms_platform::Result<Box<dyn ms_platform::ScreenInfo>> {
            Err(ms_platform::PlatformError::Unimplemented("x"))
        }
        fn autostart(&self) -> ms_platform::Result<Box<dyn ms_platform::Autostart>> {
            Err(ms_platform::PlatformError::Unimplemented("x"))
        }
        fn secure_store(&self) -> ms_platform::Result<Box<dyn ms_platform::SecureStore>> {
            Err(ms_platform::PlatformError::Unimplemented("x"))
        }
    }

    /// 复现用户场景：发送端真实 `on_transition`（含 read_files/dedup），接收端真实
    /// `spawn_receiver`，两端真实 QUIC（含 DSCP socket）。序列：文本→文件→文本。
    #[test]
    fn 回环_文件传输后文本仍能同步() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            // 接收目录隔离到临时 HOME，避免污染 ~/Downloads
            let tmp_home =
                std::env::temp_dir().join(format!("ms-loop-home-{}", std::process::id()));
            std::fs::create_dir_all(&tmp_home).unwrap();
            unsafe {
                std::env::set_var("HOME", &tmp_home);
            }

            // 两端连接：sender=a（client），receiver=b（server）；endpoint 全程保活
            use std::net::{Ipv4Addr, SocketAddr};
            let loopback: SocketAddr = (Ipv4Addr::LOCALHOST, 0).into();
            let server =
                ms_net::Endpoint::new(loopback, &ms_net::Identity::generate().unwrap()).unwrap();
            let server_addr = server.local_addr().unwrap();
            let client =
                ms_net::Endpoint::new(loopback, &ms_net::Identity::generate().unwrap()).unwrap();
            let accept = tokio::spawn(async move {
                let c = server.accept().await.unwrap().unwrap();
                std::mem::forget(server);
                c
            });
            let a = client.connect(server_addr).await.unwrap();
            let b = accept.await.unwrap();
            std::mem::forget(client);

            // 接收端真实循环
            let store_b = Arc::new(Mutex::new(ClipStore::default()));
            let platform_b = MockPlatform(SharedClip(store_b.clone()));
            let running = Arc::new(AtomicBool::new(true));
            spawn_receiver(b, &platform_b, shared_state(true), running.clone()).unwrap();

            // 发送端剪贴板 + 状态
            let store_a = Arc::new(Mutex::new(ClipStore::default()));
            let mut clip_a: Box<dyn ClipboardAccess> = Box::new(SharedClip(store_a.clone()));
            let state_a = shared_state(true);
            let push = |clip: &mut Box<dyn ClipboardAccess>, st: &SharedClipState| {
                on_transition(
                    ControlState::Local,
                    ControlState::Remote,
                    &a,
                    Some(clip),
                    st,
                    true,
                );
            };
            async fn settle() {
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            }

            // ① 文件前：复制文本 → 接收端应拿到文本
            {
                let mut s = store_a.lock().unwrap();
                s.content = Some(ClipboardContent::Text("文件前文本".into()));
                s.files.clear();
                s.cc += 1;
            }
            push(&mut clip_a, &state_a);
            settle().await;
            assert_eq!(
                store_b.lock().unwrap().content,
                Some(ClipboardContent::Text("文件前文本".into())),
                "①文件前文本应同步"
            );

            // ② 复制文件 → 接收端应落地文件
            let f = tmp_home.join("payload.bin");
            // 64 KiB（多数据报、多分块）覆盖「文件传输后文本仍能同步」回归路径
            tokio::fs::write(&f, vec![0xABu8; 64 * 1024]).await.unwrap();
            {
                let mut s = store_a.lock().unwrap();
                s.files = vec![f.clone()];
                s.content = None;
                s.cc += 1;
            }
            push(&mut clip_a, &state_a);
            // 轮询等大文件传完（手写 DSCP socket 批量较慢），最多 30s
            let mut landed = false;
            for _ in 0..300 {
                if !store_b.lock().unwrap().files.is_empty() {
                    landed = true;
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            assert!(landed, "②文件应落地到接收端剪贴板（30s 内）");

            // ③ 复制文本 → 接收端应拿到新文本（用户报此处失效）
            {
                let mut s = store_a.lock().unwrap();
                s.content = Some(ClipboardContent::Text("文件后文本".into()));
                s.files.clear();
                s.cc += 1;
            }
            push(&mut clip_a, &state_a);
            settle().await;
            assert_eq!(
                store_b.lock().unwrap().content,
                Some(ClipboardContent::Text("文件后文本".into())),
                "③文件后文本应仍能同步（复现点）"
            );

            running.store(false, Ordering::Release);
        });
    }

    /// 复现：接收端 `spawn_receiver` 按流顺序处理——一条**卡住/未完成**的文件流会
    /// 头部阻塞后续所有流（含剪贴板文本），因为循环要等当前流完全处理完才会再
    /// `accept_msg_stream()` 接下一条。用户回报"复制文件后复制文本必现问题、且
    /// 很多情况下都不行"与此完全吻合：文件流迟迟不完（大文件/网络慢/丢包），
    /// 之后任何文本同步都会被卡住，表现为"怎么复制都粘贴不出来"。
    #[test]
    fn 回环_卡住的文件流不应阻塞后续文本同步() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            use std::net::{Ipv4Addr, SocketAddr};
            let loopback: SocketAddr = (Ipv4Addr::LOCALHOST, 0).into();
            let server =
                ms_net::Endpoint::new(loopback, &ms_net::Identity::generate().unwrap()).unwrap();
            let server_addr = server.local_addr().unwrap();
            let client =
                ms_net::Endpoint::new(loopback, &ms_net::Identity::generate().unwrap()).unwrap();
            let accept = tokio::spawn(async move {
                let c = server.accept().await.unwrap().unwrap();
                std::mem::forget(server);
                c
            });
            let a = client.connect(server_addr).await.unwrap();
            let b = accept.await.unwrap();
            std::mem::forget(client);

            let store_b = Arc::new(Mutex::new(ClipStore::default()));
            let platform_b = MockPlatform(SharedClip(store_b.clone()));
            let running = Arc::new(AtomicBool::new(true));
            spawn_receiver(b, &platform_b, shared_state(true), running.clone()).unwrap();

            // ① 模拟"卡住"的文件传输：开一条流、发 FileOffer，但**不发 FileChunk/FileDone**
            // （模拟大文件/弱网下迟迟传不完，接收端会在 recv_into 里一直 await 等下一条消息）。
            let mut stuck_stream = a.open_msg_stream().await.unwrap();
            stuck_stream
                .send(&Msg::from(ms_proto::FileOffer {
                    id: uuid::Uuid::new_v4(),
                    name: "stuck.bin".into(),
                    size: 999_999_999,
                }))
                .await
                .unwrap();
            // 不 finish()：流保持打开，接收端的 xfer::receive 会一直 await 下一条消息。

            // ② 紧接着推送文本（另开一条独立流），不应被①卡住。
            let store_a = Arc::new(Mutex::new(ClipStore::default()));
            let mut clip_a: Box<dyn ClipboardAccess> = Box::new(SharedClip(store_a.clone()));
            {
                let mut s = store_a.lock().unwrap();
                s.content = Some(ClipboardContent::Text("卡住文件后的文本".into()));
                s.cc += 1;
            }
            on_transition(
                ControlState::Local,
                ControlState::Remote,
                &a,
                Some(&mut clip_a),
                &shared_state(true),
                true,
            );

            // 文本应在有界时间内送达，不受①的卡住流影响（当前实现会在此处超时失败）。
            let mut landed = false;
            for _ in 0..50 {
                if store_b.lock().unwrap().content
                    == Some(ClipboardContent::Text("卡住文件后的文本".into()))
                {
                    landed = true;
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            assert!(
                landed,
                "卡住的文件流不应头部阻塞后续文本同步（5s 内应送达）"
            );

            running.store(false, Ordering::Release);
            drop(stuck_stream);
        });
    }

    /// 复现（T-807）：一次复制多个文件穿越后，接收端应把**全部**文件路径写回剪贴板。
    /// 修复前每条文件流的任务各自 `write_files(&[自己])`，互相覆盖，最终剪贴板只剩
    /// 最后完成的那个文件（T-504 已知限制「多文件一次 Cmd+V 仅写回最后一个」）。
    #[test]
    fn 回环_多文件粘贴应写回全部路径() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let tmp_home =
                std::env::temp_dir().join(format!("ms-multi-home-{}", std::process::id()));
            std::fs::create_dir_all(&tmp_home).unwrap();
            unsafe {
                std::env::set_var("HOME", &tmp_home);
            }

            use std::net::{Ipv4Addr, SocketAddr};
            let loopback: SocketAddr = (Ipv4Addr::LOCALHOST, 0).into();
            let server =
                ms_net::Endpoint::new(loopback, &ms_net::Identity::generate().unwrap()).unwrap();
            let server_addr = server.local_addr().unwrap();
            let client =
                ms_net::Endpoint::new(loopback, &ms_net::Identity::generate().unwrap()).unwrap();
            let accept = tokio::spawn(async move {
                let c = server.accept().await.unwrap().unwrap();
                std::mem::forget(server);
                c
            });
            let a = client.connect(server_addr).await.unwrap();
            let b = accept.await.unwrap();
            std::mem::forget(client);

            let store_b = Arc::new(Mutex::new(ClipStore::default()));
            let platform_b = MockPlatform(SharedClip(store_b.clone()));
            let running = Arc::new(AtomicBool::new(true));
            spawn_receiver(b, &platform_b, shared_state(true), running.clone()).unwrap();

            // 发送端一次复制 3 个文件
            let names = ["multi-a.bin", "multi-b.bin", "multi-c.bin"];
            let mut paths = Vec::new();
            for (i, n) in names.iter().enumerate() {
                let p = tmp_home.join(n);
                tokio::fs::write(&p, vec![i as u8 + 1; 16 * 1024])
                    .await
                    .unwrap();
                paths.push(p);
            }
            let store_a = Arc::new(Mutex::new(ClipStore::default()));
            let mut clip_a: Box<dyn ClipboardAccess> = Box::new(SharedClip(store_a.clone()));
            {
                let mut s = store_a.lock().unwrap();
                s.files = paths;
                s.content = None;
                s.cc += 1;
            }
            on_transition(
                ControlState::Local,
                ControlState::Remote,
                &a,
                Some(&mut clip_a),
                &shared_state(true),
                true,
            );

            // 轮询：接收端剪贴板最终应含全部 3 个文件（修复前只剩最后完成的 1 个）
            let mut got: Vec<String> = Vec::new();
            for _ in 0..300 {
                got = store_b
                    .lock()
                    .unwrap()
                    .files
                    .iter()
                    .filter_map(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .collect();
                if got.len() == names.len() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            got.sort();
            assert_eq!(
                got,
                names.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
                "一次复制的多个文件应全部写回接收端剪贴板（T-807 复现点）"
            );

            running.store(false, Ordering::Release);
        });
    }
}
