//! 真实 QUIC 会话驱动假平台，验证 capture 启动后的错误、撤权、暂停与屏幕变化。
//! 不调用系统捕获、注入、剪贴板，也不初始化日志或持久化配置。

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use ms_platform::{
    Autostart, Button, EventSink, HidKey, PlatformError, ScreenInfo, SecureStore, VirtualScreen,
};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

struct FakeState {
    calls: Mutex<Vec<&'static str>>,
    sink: Mutex<Option<EventSink>>,
    started: AtomicBool,
    health_failed: AtomicBool,
    permission: AtomicBool,
    fail_key: AtomicBool,
    screen_width: AtomicU64,
}

impl FakeState {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            sink: Mutex::new(None),
            started: AtomicBool::new(false),
            health_failed: AtomicBool::new(false),
            permission: AtomicBool::new(true),
            fail_key: AtomicBool::new(false),
            screen_width: AtomicU64::new(800),
        })
    }

    fn record(&self, call: &'static str) {
        self.calls.lock().unwrap().push(call);
    }
}

struct FakeCapture(Arc<FakeState>);

impl InputCapture for FakeCapture {
    fn start(&mut self, sink: EventSink, grab: bool) -> ms_platform::Result<()> {
        assert!(!grab, "初始捕获不可直接拦截输入");
        self.0.record("start");
        *self.0.sink.lock().unwrap() = Some(sink);
        self.0.started.store(true, Ordering::Release);
        Ok(())
    }

    fn check_health(&self) -> ms_platform::Result<()> {
        if self.0.health_failed.load(Ordering::Acquire) {
            Err(PlatformError::Api("fake capture lost".into()))
        } else {
            Ok(())
        }
    }

    fn set_grab(&mut self, on: bool) -> ms_platform::Result<()> {
        self.0.record(if on { "grab" } else { "ungrab" });
        Ok(())
    }

    fn confine_cursor(&mut self, on: bool) -> ms_platform::Result<()> {
        self.0.record(if on { "confine" } else { "unconfine" });
        Ok(())
    }

    fn stop(&mut self) -> ms_platform::Result<()> {
        self.0.record("stop");
        self.0.sink.lock().unwrap().take();
        Ok(())
    }
}

struct FakeInject(Arc<FakeState>);

impl InputInject for FakeInject {
    fn key(&mut self, _: HidKey, _: bool) -> ms_platform::Result<()> {
        self.0.record("key");
        if self.0.fail_key.load(Ordering::Acquire) {
            Err(PlatformError::Api("fake injection failed".into()))
        } else {
            Ok(())
        }
    }

    fn mouse_move_to(&mut self, _: f64, _: f64) -> ms_platform::Result<()> {
        self.0.record("move");
        Ok(())
    }

    fn mouse_button(&mut self, _: Button, _: bool) -> ms_platform::Result<()> {
        Ok(())
    }

    fn scroll(&mut self, _: f64, _: f64, _: bool) -> ms_platform::Result<()> {
        Ok(())
    }

    fn release_all(&mut self) -> ms_platform::Result<()> {
        self.0.record("release");
        Ok(())
    }
}

struct FakeScreen(Arc<FakeState>);

impl ScreenInfo for FakeScreen {
    fn virtual_screen(&mut self) -> ms_platform::Result<VirtualScreen> {
        Ok(VirtualScreen {
            rect: Rect {
                x: 0.0,
                y: 0.0,
                w: self.0.screen_width.load(Ordering::Acquire) as f64,
                h: 600.0,
            },
            scale: 1.0,
        })
    }

    fn keyboard_layout(&mut self) -> ms_platform::Result<String> {
        Ok("test-layout".into())
    }
}

struct FakePlatform(Arc<FakeState>);

impl Platform for FakePlatform {
    fn name(&self) -> &'static str {
        "test"
    }
    fn input_capture(&self) -> ms_platform::Result<Box<dyn InputCapture>> {
        Ok(Box::new(FakeCapture(self.0.clone())))
    }
    fn input_inject(&self) -> ms_platform::Result<Box<dyn InputInject>> {
        Ok(Box::new(FakeInject(self.0.clone())))
    }
    fn clipboard(&self) -> ms_platform::Result<Box<dyn ClipboardAccess>> {
        panic!("no_clipboard 会话不可访问系统剪贴板")
    }
    fn screen_info(&self) -> ms_platform::Result<Box<dyn ScreenInfo>> {
        Ok(Box::new(FakeScreen(self.0.clone())))
    }
    fn autostart(&self) -> ms_platform::Result<Box<dyn Autostart>> {
        Err(PlatformError::Unimplemented("test autostart"))
    }
    fn secure_store(&self) -> ms_platform::Result<Box<dyn SecureStore>> {
        Err(PlatformError::Unimplemented("test secure store"))
    }
    fn has_input_permission(&self) -> bool {
        self.0.permission.load(Ordering::Acquire)
    }
}

async fn until(mut ready: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("假平台会话应在2秒内进入预期状态");
}

struct Session {
    _own_ep: Endpoint,
    _peer_ep: Endpoint,
    own: Conn,
    peer: Conn,
    state: Arc<FakeState>,
    status: Arc<ConnStatus>,
    running: Arc<AtomicBool>,
    commands: mpsc::UnboundedSender<Msg>,
    task: JoinHandle<Result<(), String>>,
    peer_task: JoinHandle<()>,
}

impl Session {
    async fn start() -> Self {
        let own_ep = Endpoint::new(
            "127.0.0.1:0".parse().unwrap(),
            &Identity::generate().unwrap(),
        )
        .unwrap();
        let peer_ep = Endpoint::new(
            "127.0.0.1:0".parse().unwrap(),
            &Identity::generate().unwrap(),
        )
        .unwrap();
        let (own, peer) = tokio::join!(
            own_ep.connect(peer_ep.local_addr().unwrap()),
            peer_ep.accept()
        );
        let own = own.unwrap();
        let peer = peer.unwrap().unwrap();
        let (commands, mut messages) = mpsc::unbounded_channel::<Msg>();
        let responding = commands.clone();
        let peer_connection = peer.clone();
        let peer_task = tokio::spawn(async move {
            let mut sender = peer_connection.open_msg_stream().await.unwrap();
            sender
                .send(&Msg::ScreenInfo(ms_proto::ScreenInfo {
                    virtual_rect: ms_proto::Rect {
                        x: 0.0,
                        y: 0.0,
                        w: 800.0,
                        h: 600.0,
                    },
                    scale: 1.0,
                }))
                .await
                .unwrap();
            let mut receiver = peer_connection.accept_msg_stream().await.unwrap();
            // 独立读任务持有 recv future，不因测试命令或心跳定时器取消半帧读取。
            let reader = tokio::spawn(async move {
                while let Ok(Some(message)) = receiver.recv().await {
                    if let Msg::Ping(ping) = message {
                        let _ = responding.send(Msg::Pong(Pong {
                            seq: ping.seq,
                            t0: ping.t0,
                        }));
                    }
                }
            });
            loop {
                tokio::select! {
                    _ = peer_connection.wait_closed() => break,
                    Some(message) = messages.recv() => {
                        if sender.send(&message).await.is_err() { break; }
                    }
                }
            }
            reader.abort();
            let _ = reader.await;
        });
        let state = FakeState::new();
        let status = Arc::new(ConnStatus::new());
        let running = Arc::new(AtomicBool::new(true));
        let task = {
            let conn = own.clone();
            let state = state.clone();
            let status = status.clone();
            let running = running.clone();
            tokio::spawn(async move {
                let args = Args {
                    no_tray: true,
                    no_clipboard: true,
                    screen: (800.0, 600.0),
                    peer_screen: (800.0, 600.0),
                    side: "right".into(),
                    ..Args::default()
                };
                let mut config = Config::from_args(&args);
                config.mac_control = true;
                config.shortcuts_enabled = false;
                run_session(
                    &conn,
                    &FakePlatform(state),
                    &args,
                    &Arc::new(Mutex::new(config)),
                    &running,
                    &status,
                    true,
                )
                .await
            })
        };
        until(|| state.started.load(Ordering::Acquire) && status.connected()).await;
        Self {
            _own_ep: own_ep,
            _peer_ep: peer_ep,
            own,
            peer,
            state,
            status,
            running,
            commands,
            task,
            peer_task,
        }
    }

    async fn grab(&self) {
        self.state.sink.lock().unwrap().as_mut().unwrap()(InputEvent::MouseMove {
            dx: 200.0,
            dy: 0.0,
            ax: 799.0,
            ay: 300.0,
        });
        until(|| self.status.control_state() == ms_core::control::ControlState::Remote).await;
        assert!(self.state.calls.lock().unwrap().contains(&"grab"));
    }

    async fn finish(self, error: Option<&str>) {
        let result = tokio::time::timeout(Duration::from_secs(3), self.task)
            .await
            .expect("捕获启动后的回退必须有界返回")
            .unwrap();
        match error {
            Some(expected) => assert!(
                result
                    .as_ref()
                    .is_err_and(|message| message.contains(expected)),
                "预期 {expected}，得到 {result:?}"
            ),
            None => assert_eq!(result, Ok(())),
        }
        let calls = self.state.calls.lock().unwrap().clone();
        assert_eq!(calls.first(), Some(&"start"));
        assert!(
            calls.ends_with(&["ungrab", "release", "unconfine", "stop"]),
            "安全清理顺序错误：{calls:?}"
        );
        assert!(
            self.state.sink.lock().unwrap().is_none(),
            "捕获回调必须释放"
        );
        assert!(!self.status.connected());
        assert_eq!(
            self.status.control_state(),
            ms_core::control::ControlState::Local
        );
        tokio::time::timeout(Duration::from_secs(1), self.own.wait_closed())
            .await
            .expect("本端连接应关闭");
        tokio::time::timeout(Duration::from_secs(1), self.peer.wait_closed())
            .await
            .expect("对端也应观察到连接关闭");
        tokio::time::timeout(Duration::from_secs(1), self.peer_task)
            .await
            .expect("假对端任务应退出")
            .unwrap();
    }
}

#[tokio::test]
async fn 会话_真实对端键盘注入失败仍按固定顺序释放并关闭连接() {
    let session = Session::start().await;
    session
        .commands
        .send(Msg::EnterScreen(ms_proto::EnterScreen {
            entry_edge: Edge::Left,
            ratio: 0.5,
        }))
        .unwrap();
    until(|| session.status.control_state() == ms_core::control::ControlState::Controlled).await;
    session.state.fail_key.store(true, Ordering::Release);
    session
        .commands
        .send(Msg::KeyEvent(ms_proto::KeyEvent {
            hid: 0x04,
            down: true,
            shadow: (100.0, 100.0),
        }))
        .unwrap();
    session.finish(Some("输入注入失败")).await;
}

#[tokio::test]
async fn 会话_捕获故障撤权和暂停都解除已生效的grab() {
    for reason in ["capture", "permission", "stop"] {
        let session = Session::start().await;
        session.grab().await;
        let error = match reason {
            "capture" => {
                session.state.health_failed.store(true, Ordering::Release);
                Some("捕获已停止")
            }
            "permission" => {
                session.state.permission.store(false, Ordering::Release);
                Some("权限已撤销")
            }
            _ => {
                session.running.store(false, Ordering::Release);
                None
            }
        };
        session.finish(error).await;
    }
}

#[tokio::test]
async fn 会话_显示器变化时退出旧坐标会话并释放输入() {
    let session = Session::start().await;
    session.grab().await;
    session.state.screen_width.store(1024, Ordering::Release);
    session.finish(Some("显示器排列或分辨率已变化")).await;
}
